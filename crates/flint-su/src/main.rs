//! flint-su: run a shell as another user.
//!
//! Installed setuid root. Like flint-passwd it is small and strict:
//!
//! * the caller is identified by the *real* uid, never by arguments or the environment;
//! * root may become anyone without a password; everyone else must give the *target's* password;
//! * if a group named `wheel` exists, only its members may become root (BSD convention);
//! * a wrong password costs a 2 second delay, and there is one attempt per run;
//! * `-s SHELL` is root-only, and a non-root caller can only get a shell listed in /etc/shells
//!   (when that file exists).

use std::fs;
use std::os::unix::process::CommandExt;
use std::process::{Command};
use std::thread;
use std::time::Duration;

use flint_auth::files::{find_user, find_user_by_uid, groups_of, parse_group, parse_passwd, shadow_hash, Group, User};
use flint_auth::hash::{is_locked, verify};
use flint_core::tty::read_secret;

const WRONG_PASSWORD_DELAY: Duration = Duration::from_secs(2);
const ROOT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const USER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

const USAGE: &str = "usage: su [-] [-c COMMAND] [-s SHELL] [USER]
  -, -l, --login   start a login shell with a clean environment
  -c, --command    run COMMAND with the shell's -c instead of an interactive shell
  -s, --shell      use SHELL instead of the user's (root only)
  USER defaults to root";

#[derive(Debug, PartialEq, Eq)]
struct Opts {
    login: bool,
    command: Option<String>,
    shell: Option<String>,
    user: String,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { login: false, command: None, shell: None, user: "root".to_string() };
    let mut user_given = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        match a {
            "-" | "-l" | "--login" => o.login = true,
            "-c" | "--command" => {
                o.command = Some(args.get(i).ok_or("option requires an argument -- 'c'")?.clone());
                i += 1;
            }
            "-s" | "--shell" => {
                o.shell = Some(args.get(i).ok_or("option requires an argument -- 's'")?.clone());
                i += 1;
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            _ if a.starts_with('-') => return Err(format!("invalid option '{a}'")),
            _ => {
                if user_given {
                    return Err("too many arguments".to_string());
                }
                o.user = a.to_string();
                user_given = true;
            }
        }
    }
    Ok(o)
}

/// BSD rule: when a `wheel` group exists, only its members (and root) may become root.
/// With no such group anyone who knows the password may.
fn wheel_ok(groups: &[Group], caller: &User) -> bool {
    let Some(wheel) = groups.iter().find(|g| g.name == "wheel") else { return true };
    caller.uid == 0 || caller.gid == wheel.gid || wheel.members.iter().any(|m| m == &caller.name)
}

/// Environment for the new shell. A login shell gets only a few clean variables; otherwise the
/// caller's environment is kept and the identity variables are updated.
fn build_env(login: bool, target: &User, shell: &str, current: &[(String, String)]) -> Vec<(String, String)> {
    let path = if target.uid == 0 { ROOT_PATH } else { USER_PATH };
    let identity = |env: &mut Vec<(String, String)>| {
        for (k, v) in [("HOME", target.home.as_str()), ("SHELL", shell), ("USER", target.name.as_str()), ("LOGNAME", target.name.as_str())] {
            env.retain(|(ek, _)| ek != k);
            env.push((k.to_string(), v.to_string()));
        }
    };
    if login {
        let term = current.iter().find(|(k, _)| k == "TERM").map(|(_, v)| v.clone()).unwrap_or_else(|| "linux".to_string());
        let mut env = vec![("PATH".to_string(), path.to_string()), ("TERM".to_string(), term)];
        identity(&mut env);
        env
    } else {
        let mut env = current.to_vec();
        identity(&mut env);
        env
    }
}

fn shell_listed(shells_file: &str, shell: &str) -> bool {
    shells_file.lines().map(str::trim).any(|l| !l.is_empty() && !l.starts_with('#') && l == shell)
}

fn fail(msg: &str) -> i32 {
    eprintln!("su: {msg}");
    1
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    // SAFETY: plain libc queries.
    let (caller_uid, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
    if effective != 0 {
        return fail("must run as root (this program has to be installed setuid root)");
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };

    let users = parse_passwd(&fs::read_to_string("/etc/passwd").unwrap_or_default());
    let groups = parse_group(&fs::read_to_string("/etc/group").unwrap_or_default());
    let Some(caller) = find_user_by_uid(&users, caller_uid).cloned().or_else(|| {
        (caller_uid == 0).then(|| User {
            name: "root".into(),
            passwd: "x".into(),
            uid: 0,
            gid: 0,
            gecos: String::new(),
            home: "/root".into(),
            shell: "/bin/sh".into(),
        })
    }) else {
        return fail("cannot find your user name");
    };
    let Some(target) = find_user(&users, &opts.user).cloned() else {
        return fail(&format!("user {} does not exist", opts.user));
    };

    if caller.uid != 0 {
        if opts.shell.is_some() {
            return fail("only root may choose the shell");
        }
        if target.uid == 0 && !wheel_ok(&groups, &caller) {
            return fail("you are not permitted to become root (not in group wheel)");
        }
        let shadow = fs::read_to_string("/etc/shadow").unwrap_or_default();
        let stored = shadow_hash(&shadow, &target.name).unwrap_or("");
        let Some(password) = read_secret("Password: ") else { return 1 };
        let ok = !is_locked(stored) && verify(&password, stored) == Ok(true);
        if !ok {
            thread::sleep(WRONG_PASSWORD_DELAY);
            return fail("Authentication failure");
        }
    }

    let shell = opts.shell.clone().unwrap_or_else(|| if target.shell.is_empty() { "/bin/sh".to_string() } else { target.shell.clone() });
    if caller.uid != 0 {
        if let Ok(list) = fs::read_to_string("/etc/shells") {
            if !shell_listed(&list, &shell) {
                return fail(&format!("{shell} is not listed in /etc/shells"));
            }
        }
    }

    // Become the target: supplementary groups first, then gid, then uid.
    let gids = groups_of(&groups, &target);
    // SAFETY: setgroups reads `gids.len()` entries from a valid slice.
    let switched = unsafe {
        libc::setgroups(gids.len() as _, gids.as_ptr() as *const libc::gid_t) == 0
            && libc::setgid(target.gid) == 0
            && libc::setuid(target.uid) == 0
    };
    if !switched {
        return fail(&format!("cannot switch to {}: {}", target.name, std::io::Error::last_os_error()));
    }

    let current: Vec<(String, String)> = std::env::vars().collect();
    let env = build_env(opts.login, &target, &shell, &current);
    if opts.login && std::env::set_current_dir(&target.home).is_err() {
        let _ = std::env::set_current_dir("/");
    }

    let base = shell.rsplit('/').next().unwrap_or(&shell).to_string();
    let mut cmd = Command::new(&shell);
    cmd.arg0(if opts.login { format!("-{base}") } else { base }).env_clear().envs(env);
    if let Some(c) = &opts.command {
        cmd.arg("-c").arg(c);
    }
    let err = cmd.exec();
    fail(&format!("cannot run {shell}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn user(name: &str, uid: u32, gid: u32) -> User {
        User { name: name.into(), passwd: "x".into(), uid, gid, gecos: String::new(), home: format!("/home/{name}"), shell: "/bin/sh".into() }
    }

    #[test]
    fn argument_parsing() {
        let d = |u: &str| Opts { login: false, command: None, shell: None, user: u.to_string() };
        assert_eq!(parse_args(&v(&[])), Ok(d("root")));
        assert_eq!(parse_args(&v(&["bob"])), Ok(d("bob")));
        assert_eq!(parse_args(&v(&["-", "bob"])), Ok(Opts { login: true, ..d("bob") }));
        assert_eq!(parse_args(&v(&["-l"])), Ok(Opts { login: true, ..d("root") }));
        assert_eq!(parse_args(&v(&["-c", "id", "bob"])), Ok(Opts { command: Some("id".into()), ..d("bob") }));
        assert_eq!(parse_args(&v(&["bob", "-c", "id"])), Ok(Opts { command: Some("id".into()), ..d("bob") }));
        assert_eq!(parse_args(&v(&["-s", "/bin/ash"])), Ok(Opts { shell: Some("/bin/ash".into()), ..d("root") }));
        assert!(parse_args(&v(&["-c"])).is_err());
        assert!(parse_args(&v(&["-s"])).is_err());
        assert!(parse_args(&v(&["-z"])).is_err());
        assert!(parse_args(&v(&["a", "b"])).is_err());
        assert!(parse_args(&v(&["--help"])).is_err());
    }

    #[test]
    fn wheel_rule() {
        let none: Vec<Group> = vec![Group { name: "users".into(), gid: 100, members: vec![] }];
        assert!(wheel_ok(&none, &user("bob", 1000, 1000)), "no wheel group: anyone with the password");
        let wheel = vec![Group { name: "wheel".into(), gid: 10, members: vec!["alice".into()] }];
        assert!(wheel_ok(&wheel, &user("alice", 1001, 1001)), "listed member");
        assert!(wheel_ok(&wheel, &user("carol", 1002, 10)), "primary group is wheel");
        assert!(wheel_ok(&wheel, &user("root", 0, 0)), "root");
        assert!(!wheel_ok(&wheel, &user("bob", 1000, 1000)), "everyone else");
    }

    #[test]
    fn environments() {
        let target = user("bob", 1000, 1000);
        let current = v(&["TERM", "xterm", "EDITOR", "vi", "HOME", "/home/alice", "USER", "alice"]);
        let pairs: Vec<(String, String)> = current.chunks(2).map(|c| (c[0].clone(), c[1].clone())).collect();
        let get = |env: &[(String, String)], k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());

        let login = build_env(true, &target, "/bin/sh", &pairs);
        assert_eq!(get(&login, "HOME").unwrap(), "/home/bob");
        assert_eq!(get(&login, "USER").unwrap(), "bob");
        assert_eq!(get(&login, "TERM").unwrap(), "xterm");
        assert_eq!(get(&login, "PATH").unwrap(), USER_PATH);
        assert!(get(&login, "EDITOR").is_none(), "login shells do not inherit the caller's environment");

        let plain = build_env(false, &target, "/bin/ash", &pairs);
        assert_eq!(get(&plain, "EDITOR").unwrap(), "vi", "kept");
        assert_eq!(get(&plain, "HOME").unwrap(), "/home/bob", "identity updated");
        assert_eq!(get(&plain, "SHELL").unwrap(), "/bin/ash");
        assert_eq!(plain.iter().filter(|(k, _)| k == "USER").count(), 1, "no duplicates");

        let root = build_env(true, &user("root", 0, 0), "/bin/sh", &[]);
        assert_eq!(get(&root, "PATH").unwrap(), ROOT_PATH);
        assert_eq!(get(&root, "TERM").unwrap(), "linux");
    }

    #[test]
    fn shells_file() {
        let f = "# comment\n/bin/sh\n\n  /bin/ash  \n";
        assert!(shell_listed(f, "/bin/sh"));
        assert!(shell_listed(f, "/bin/ash"));
        assert!(!shell_listed(f, "/bin/bash"));
        assert!(!shell_listed(f, "# comment"));
        assert!(!shell_listed("", "/bin/sh"));
    }
}
