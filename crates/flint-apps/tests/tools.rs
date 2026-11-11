//! Runs the core tools through the real flint binary (`flint ls ...`).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_flint");

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_in(dir: &Path, args: &[&str]) -> Out {
    let o = Command::new(BIN).args(args).current_dir(dir).output().expect("run flint");
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn run(args: &[&str]) -> Out {
    run_in(Path::new("/"), args)
}

fn run_stdin(args: &[&str], input: &str) -> Out {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
    }
}

/// A scratch directory removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("flint-box-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn s(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().into_owned()
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn list_has_every_tool_and_program() {
    let o = run(&["--list"]);
    assert_eq!(o.code, 0);
    let listed: Vec<&str> = o.stdout.lines().collect();
    for tool in include_str!("../../flint-box/applets.txt").lines().filter(|l| !l.is_empty()) {
        assert!(listed.contains(&tool), "{tool} missing from flint --list");
    }
    for prog in ["sh", "pkg", "init", "login", "poweroff", "reboot", "halt"] {
        assert!(listed.contains(&prog), "{prog} missing from flint --list");
    }
    assert!(!listed.iter().any(|n| n.starts_with("flint")), "no flint-* names: {listed:?}");
}

#[test]
fn dispatch_by_symlink_name_and_unknown_applet() {
    let t = Tmp::new();
    let link = t.path().join("echo");
    symlink(BIN, &link).unwrap();
    let o = Command::new(&link).arg("via link").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "via link\n");

    let o = run(&["nosuchapplet"]);
    assert_eq!(o.code, 127);
    assert!(o.stderr.contains("no program called 'nosuchapplet'"), "{}", o.stderr);
    assert_eq!(run(&[]).code, 2);

    // the same through a symlink with an unknown name
    let odd = t.path().join("frobnicate");
    symlink(BIN, &odd).unwrap();
    let o = Command::new(&odd).output().unwrap();
    assert_eq!(o.status.code(), Some(127));

    // a login shell is started as "-sh"
    let sh = t.path().join("sh");
    symlink(BIN, &sh).unwrap();
    use std::os::unix::process::CommandExt;
    let o = Command::new(&sh).arg0("-sh").args(["-c", "echo login-$0"]).env("HOME", t.path()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "login--sh\n");

    let v = run(&["--version"]);
    assert!(v.stdout.starts_with("Flint Linux "), "{}", v.stdout);
}

#[test]
fn echo_true_false_pwd() {
    assert_eq!(run(&["echo", "a", "b"]).stdout, "a b\n");
    assert_eq!(run(&["echo", "-n", "x"]).stdout, "x");
    assert_eq!(run(&["echo", "-e", "a\\tb"]).stdout, "a\tb\n");
    assert_eq!(run(&["echo", "-e", "a\\cb"]).stdout, "a");
    assert_eq!(run(&["true"]).code, 0);
    assert_eq!(run(&["false"]).code, 1);
    let t = Tmp::new();
    let want = fs::canonicalize(t.path()).unwrap();
    assert_eq!(run_in(t.path(), &["pwd"]).stdout.trim(), want.to_str().unwrap());
}

#[test]
fn cat_files_and_stdin() {
    let t = Tmp::new();
    fs::write(t.s("a"), "AAA\n").unwrap();
    fs::write(t.s("b"), "BBB\n").unwrap();
    assert_eq!(run(&["cat", &t.s("a"), &t.s("b")]).stdout, "AAA\nBBB\n");
    assert_eq!(run_stdin(&["cat"], "from stdin\n").stdout, "from stdin\n");
    assert_eq!(run_stdin(&["cat", &t.s("a"), "-"], "mid\n").stdout, "AAA\nmid\n");
    let o = run(&["cat", &t.s("missing")]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("missing"));
}

#[test]
fn mkdir_rmdir_touch_ls() {
    let t = Tmp::new();
    assert_eq!(run(&["mkdir", "-p", &t.s("a/b/c")]).code, 0);
    assert!(t.path().join("a/b/c").is_dir());
    assert_eq!(run(&["mkdir", "-p", &t.s("a/b/c")]).code, 0, "-p tolerates existing");
    assert_eq!(run(&["mkdir", &t.s("a")]).code, 1, "plain mkdir refuses existing");
    assert_eq!(run(&["mkdir", &t.s("x/y")]).code, 1, "missing parent without -p");
    assert_eq!(run(&["mkdir", "-m", "700", &t.s("private")]).code, 0);
    assert_eq!(fs::metadata(t.s("private")).unwrap().permissions().mode() & 0o777, 0o700);

    assert_eq!(run(&["touch", &t.s("file"), &t.s(".hidden")]).code, 0);
    assert!(t.path().join("file").is_file());
    assert_eq!(run(&["touch", "-c", &t.s("never")]).code, 0);
    assert!(!t.path().join("never").exists());

    // ls: sorted bytewise, dotfiles hidden unless -a / -A
    assert_eq!(run_in(t.path(), &["ls"]).stdout, "a\nfile\nprivate\n");
    assert_eq!(run_in(t.path(), &["ls", "-A"]).stdout, ".hidden\na\nfile\nprivate\n");
    assert_eq!(run_in(t.path(), &["ls", "-a"]).stdout, ".\n..\n.hidden\na\nfile\nprivate\n");
    assert_eq!(run_in(t.path(), &["ls", "-d", "a"]).stdout, "a\n");
    assert_eq!(run(&["ls", &t.s("missing")]).code, 2);

    assert_eq!(run(&["rmdir", &t.s("a/b/c")]).code, 0);
    assert_eq!(run(&["rmdir", &t.s("a")]).code, 1, "not empty");
}

#[test]
fn ls_long_format() {
    let t = Tmp::new();
    fs::write(t.s("f"), "12345").unwrap();
    fs::set_permissions(t.s("f"), fs::Permissions::from_mode(0o640)).unwrap();
    symlink("f", t.s("link")).unwrap();
    fs::create_dir(t.s("d")).unwrap();
    let o = run_in(t.path(), &["ls", "-l"]);
    assert_eq!(o.code, 0);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", o.stdout);
    assert!(lines[0].starts_with("drwx"), "{}", lines[0]);
    assert!(lines[1].starts_with("-rw-r-----") && lines[1].contains(" 5 ") && lines[1].ends_with(" f"), "{}", lines[1]);
    assert!(lines[2].starts_with("lrwx") && lines[2].ends_with("link -> f"), "{}", lines[2]);
}

#[test]
fn cp_mv_rm_ln() {
    let t = Tmp::new();
    fs::write(t.s("src"), "data").unwrap();
    assert_eq!(run(&["cp", &t.s("src"), &t.s("dst")]).code, 0);
    assert_eq!(fs::read_to_string(t.s("dst")).unwrap(), "data");

    fs::create_dir_all(t.s("tree/sub")).unwrap();
    fs::write(t.s("tree/sub/leaf"), "leaf").unwrap();
    fs::write(t.s("tree/top"), "top").unwrap();
    symlink("top", t.s("tree/lnk")).unwrap();
    let o = run(&["cp", &t.s("tree"), &t.s("copy")]);
    assert_eq!(o.code, 1, "directory without -r");
    assert!(o.stderr.contains("-r"));
    assert_eq!(run(&["cp", "-r", &t.s("tree"), &t.s("copy")]).code, 0);
    assert_eq!(fs::read_to_string(t.s("copy/sub/leaf")).unwrap(), "leaf");
    assert_eq!(fs::read_link(t.s("copy/lnk")).unwrap(), Path::new("top"), "symlinks are copied as links");
    // copying into an existing directory puts the source inside it
    assert_eq!(run(&["cp", "-r", &t.s("tree"), &t.s("copy")]).code, 0);
    assert!(t.path().join("copy/tree/top").is_file());
    assert_eq!(run(&["cp", "-r", &t.s("tree"), &t.s("tree/inside")]).code, 1, "into itself");

    assert_eq!(run(&["mv", &t.s("dst"), &t.s("moved")]).code, 0);
    assert!(!t.path().join("dst").exists() && t.path().join("moved").is_file());
    assert_eq!(run(&["mv", &t.s("moved"), &t.s("copy")]).code, 0);
    assert!(t.path().join("copy/moved").is_file());
    assert_eq!(run(&["mv", &t.s("a"), &t.s("b"), &t.s("src")]).code, 1, "target not a directory");

    assert_eq!(run(&["ln", "-s", "src", &t.s("sl")]).code, 0);
    assert_eq!(fs::read_link(t.s("sl")).unwrap(), Path::new("src"));
    assert_eq!(run(&["ln", &t.s("src"), &t.s("hl")]).code, 0);
    assert_eq!(fs::read_to_string(t.s("hl")).unwrap(), "data");
    assert_eq!(run(&["ln", &t.s("src"), &t.s("hl")]).code, 1, "exists");
    assert_eq!(run(&["ln", "-sf", "elsewhere", &t.s("sl")]).code, 0);
    assert_eq!(fs::read_link(t.s("sl")).unwrap(), Path::new("elsewhere"));

    assert_eq!(run(&["rm", &t.s("src")]).code, 0);
    assert_eq!(run(&["rm", &t.s("src")]).code, 1);
    assert_eq!(run(&["rm", "-f", &t.s("src")]).code, 0);
    assert_eq!(run(&["rm", &t.s("tree")]).code, 1, "directory without -r");
    assert_eq!(run(&["rm", "-r", &t.s("tree"), &t.s("copy")]).code, 0);
    assert!(!t.path().join("tree").exists() && !t.path().join("copy").exists());
}

#[test]
fn rm_refuses_root() {
    let o = run(&["rm", "-rf", "/"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("dangerous"));
    assert!(Path::new("/bin").exists());
}

#[test]
fn head_tail_wc() {
    let t = Tmp::new();
    fs::write(t.s("n"), "1\n2\n3\n4\n5\n").unwrap();
    assert_eq!(run(&["head", "-n", "2", &t.s("n")]).stdout, "1\n2\n");
    assert_eq!(run(&["tail", "-n", "2", &t.s("n")]).stdout, "4\n5\n");
    assert_eq!(run(&["tail", "-n", "+4", &t.s("n")]).stdout, "4\n5\n");
    assert_eq!(run_stdin(&["head", "-n1"], "x\ny\n").stdout, "x\n");
    assert_eq!(run_stdin(&["head", "-1"], "x\ny\n").stdout, "x\n");
    assert_eq!(run_stdin(&["tail", "-1"], "x\ny\n").stdout, "y\n");
    assert_eq!(run(&["head", &t.s("n")]).stdout, "1\n2\n3\n4\n5\n");
    let two = run(&["head", "-n", "1", &t.s("n"), &t.s("n")]);
    assert!(two.stdout.contains("==> ") && two.stdout.matches("1\n").count() >= 2, "{}", two.stdout);

    assert_eq!(run_stdin(&["wc"], "hello world\nfoo\n").stdout.split_whitespace().collect::<Vec<_>>(), ["2", "3", "16"]);
    assert_eq!(run_stdin(&["wc", "-l"], "a\nb\n").stdout, "2\n", "a single count is not padded");
    assert_eq!(run(&["wc", "-c", &t.s("n")]).stdout, format!("10 {}\n", t.s("n")));
    let multi = run(&["wc", "-c", &t.s("n"), &t.s("n")]);
    assert!(multi.stdout.contains("total") && multi.stdout.contains("20"), "{}", multi.stdout);
}

#[test]
fn chmod_and_chown() {
    let t = Tmp::new();
    fs::write(t.s("f"), "").unwrap();
    let mode = |p: &str| fs::metadata(t.s(p)).unwrap().permissions().mode() & 0o7777;
    assert_eq!(run(&["chmod", "600", &t.s("f")]).code, 0);
    assert_eq!(mode("f"), 0o600);
    assert_eq!(run(&["chmod", "u+x,g+r", &t.s("f")]).code, 0);
    assert_eq!(mode("f"), 0o740);
    assert_eq!(run(&["chmod", "a=r", &t.s("f")]).code, 0);
    assert_eq!(mode("f"), 0o444);
    assert_eq!(run(&["chmod", "bogus", &t.s("f")]).code, 1);
    assert_eq!(run(&["chmod", "644", &t.s("nope")]).code, 1);

    fs::create_dir_all(t.s("d/e")).unwrap();
    fs::write(t.s("d/e/x"), "").unwrap();
    assert_eq!(run(&["chmod", "-R", "750", &t.s("d")]).code, 0);
    assert_eq!(mode("d/e/x"), 0o750);

    // chown to the current owner always works, even unprivileged
    let meta = fs::metadata(t.s("f")).unwrap();
    use std::os::unix::fs::MetadataExt;
    let spec = format!("{}:{}", meta.uid(), meta.gid());
    assert_eq!(run(&["chown", &spec, &t.s("f")]).code, 0);
    assert_eq!(run(&["chown", "no-such-user-xyz", &t.s("f")]).code, 1);
}

#[test]
fn uname_id_whoami_hostname() {
    assert_eq!(run(&["uname"]).stdout, "Linux\n");
    assert_eq!(run(&["uname", "-s"]).stdout, "Linux\n");
    let a = run(&["uname", "-a"]).stdout;
    assert!(a.starts_with("Linux ") && a.split_whitespace().count() >= 5, "{a}");
    assert!(run(&["uname", "-m"]).stdout.trim().len() > 2);
    let id = run(&["id"]);
    assert!(id.stdout.starts_with("uid=") && id.stdout.contains(" gid="), "{}", id.stdout);
    assert!(!run(&["whoami"]).stdout.trim().is_empty());
    let uid = run(&["id", "-u"]).stdout;
    assert!(uid.trim().parse::<u32>().is_ok(), "id -u: {uid:?}");
    assert!(run(&["id", "-g"]).stdout.trim().parse::<u32>().is_ok());
    assert_eq!(run(&["id", "-un"]).stdout, run(&["whoami"]).stdout, "id -un matches whoami");
    assert!(run(&["id", "-G"]).stdout.split_whitespace().all(|g| g.parse::<u32>().is_ok()));
    assert!(run(&["id", "-q"]).code != 0);
    assert!(!run(&["hostname"]).stdout.trim().is_empty());
    assert_eq!(run(&["uname", "-z"]).code, 1);
}

#[test]
fn env_prints_and_runs() {
    let o = run(&["env", "FLINT_T=hello"]);
    assert!(o.stdout.lines().any(|l| l == "FLINT_T=hello"));
    let o = run(&["env", "-i", "A=1"]);
    assert_eq!(o.stdout, "A=1\n");
    // run another program with a changed environment: flint itself as `env` with no args
    let o = Command::new(BIN).args(["env", "-i", "ZZ=9", BIN, "env"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "ZZ=9\n");
    let o = run(&["env", "/definitely/not/here"]);
    assert_eq!(o.code, 127);
}

#[test]
fn sleep_and_kill() {
    assert_eq!(run(&["sleep", "0.05"]).code, 0);
    assert_eq!(run(&["sleep"]).code, 1);
    assert_eq!(run(&["sleep", "abc"]).code, 1);

    let mut child = Command::new(BIN).args(["sleep", "30"]).spawn().unwrap();
    let pid = child.id().to_string();
    assert_eq!(run(&["kill", &pid]).code, 0);
    let status = child.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(15));

    let mut child = Command::new(BIN).args(["sleep", "30"]).spawn().unwrap();
    let pid = child.id().to_string();
    assert_eq!(run(&["kill", "-9", &pid]).code, 0);
    assert_eq!(child.wait().unwrap().signal(), Some(9));

    assert!(run(&["kill", "-l"]).stdout.contains("TERM"));
    assert_eq!(run(&["kill"]).code, 1);
    assert_eq!(run(&["kill", "notapid"]).code, 1);
}

#[test]
fn mount_lists_and_ps_shows_processes() {
    let m = run(&["mount"]);
    assert_eq!(m.code, 0);
    assert!(m.stdout.contains(" on ") && m.stdout.contains(" type "), "{}", m.stdout);
    let o = run(&["mount", "/dev/null"]);
    assert_eq!(o.code, 1, "one operand without remount is a usage error");

    let ps = run(&["ps"]);
    assert_eq!(ps.code, 0);
    assert!(ps.stdout.lines().next().unwrap().contains("PID"));
    assert!(ps.stdout.lines().count() >= 2, "{}", ps.stdout);

    assert_eq!(run(&["umount"]).code, 1);
    assert_eq!(run(&["umount", "/definitely/not/mounted"]).code, 1);
}
