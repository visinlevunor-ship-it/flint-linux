//! Program entry: argument handling, login files, the interactive loop, the prompt.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::parser::{parse, ParseError};
use crate::shell::{Flow, Shell};
use crate::util::{errln, write_fd};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

const USAGE: &str = "usage: flint-sh [-c COMMAND [NAME [ARG...]]] | [-i] [-l] [SCRIPT [ARG...]] | -s [ARG...]\n";

/// Expand `\u \h \w \W \$ \n \e \\ \[ \]` in a prompt template.
pub fn prompt_escapes(t: &str, user: &str, host: &str, cwd: &str, home: &str, root: bool) -> String {
    let shown = match cwd.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => cwd.to_string(),
    };
    let base = if shown == "~" || shown == "/" { shown.clone() } else { shown.rsplit('/').next().unwrap_or(&shown).to_string() };
    let mut out = String::new();
    let mut it = t.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('u') => out.push_str(user),
            Some('h') => out.push_str(host.split('.').next().unwrap_or(host)),
            Some('w') => out.push_str(&shown),
            Some('W') => out.push_str(&base),
            Some('$') => out.push(if root { '#' } else { '$' }),
            Some('n') => out.push('\n'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some('[') | Some(']') => {}
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl Shell {
    fn prompt(&mut self, var: &str) -> String {
        let default = if var == "PS1" { "\\u@\\h:\\w\\$ " } else { "> " };
        let template = self.get(var).unwrap_or(default).to_string();
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length.
        let host = unsafe {
            if libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) == 0 {
                let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                String::from_utf8_lossy(&buf[..end]).into_owned()
            } else {
                "flint".to_string()
            }
        };
        // SAFETY: plain libc query.
        let uid = unsafe { libc::geteuid() };
        let user = self.get("USER").map(str::to_string).unwrap_or_else(|| uid.to_string());
        let cwd = self.get("PWD").map(str::to_string).unwrap_or_else(|| "/".to_string());
        let home = self.get("HOME").unwrap_or("").to_string();
        let escaped = prompt_escapes(&template, &user, &host, &cwd, &home, uid == 0);
        self.expand_dq_text(&escaped)
    }
}

enum Read {
    Line(String),
    Eof,
    Interrupted,
}

/// One line from fd 0, a byte at a time (no read-ahead). A SIGINT interrupts the read.
fn read_line() -> Read {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        // SAFETY: buffer pointer and length are valid.
        let n = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
        if n < 0 {
            if crate::util::errno() == libc::EINTR {
                if INTERRUPTED.swap(false, Ordering::SeqCst) {
                    return Read::Interrupted;
                }
                continue;
            }
            return Read::Eof;
        }
        if n == 0 {
            return if line.is_empty() { Read::Eof } else { Read::Line(String::from_utf8_lossy(&line).into_owned()) };
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Read::Line(String::from_utf8_lossy(&line).into_owned());
        }
    }
}

fn init_job_control(sh: &mut Shell) {
    // SAFETY: standard job-control setup for an interactive shell.
    unsafe {
        let mut tries = 0;
        loop {
            let fg = libc::tcgetpgrp(0);
            if fg < 0 || fg == libc::getpgrp() || tries > 100 {
                break;
            }
            libc::kill(0, libc::SIGTTIN);
            tries += 1;
        }
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as usize;
        sa.sa_flags = 0; // no SA_RESTART: a SIGINT must interrupt the prompt's read
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        for sig in [libc::SIGQUIT, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU] {
            libc::signal(sig, libc::SIG_IGN);
        }
        libc::setpgid(0, 0); // fails harmlessly for a session leader
        sh.shell_pgid = libc::getpgrp();
        libc::tcsetpgrp(0, sh.shell_pgid);
    }
    sh.interactive = true;
}

fn interactive_loop(sh: &mut Shell) -> i32 {
    let mut buf = String::new();
    loop {
        sh.reap_jobs();
        sh.notify_jobs();
        let prompt = sh.prompt(if buf.is_empty() { "PS1" } else { "PS2" });
        write_fd(2, prompt.as_bytes());
        match read_line() {
            Read::Line(l) => buf.push_str(&l),
            Read::Interrupted => {
                write_fd(2, b"\n");
                buf.clear();
                continue;
            }
            Read::Eof => {
                if !buf.is_empty() {
                    errln("syntax error: unexpected end of file");
                    buf.clear();
                }
                write_fd(2, b"exit\n");
                return sh.last_status;
            }
        }
        if buf.trim().is_empty() {
            buf.clear();
            continue;
        }
        match parse(&buf) {
            Err(ParseError::Incomplete) => continue,
            Err(ParseError::Syntax(m)) => {
                errln(&format!("syntax error: {m}"));
                sh.last_status = 2;
                buf.clear();
            }
            Ok(cmd) => {
                buf.clear();
                sh.exec(&cmd);
                if let Some(Flow::Exit(n)) = sh.flow {
                    return n;
                }
                sh.flow = None; // a stray break/continue/return at the prompt
            }
        }
    }
}

fn source_if_exists(sh: &mut Shell, path: &str) {
    if let Ok(text) = std::fs::read_to_string(path) {
        sh.run_source(&text);
    }
}

pub fn main() -> i32 {
    // Rust ignores SIGPIPE; a shell (and what it runs) should die quietly on a closed pipe.
    // SAFETY: resetting a signal disposition.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

    let args: Vec<String> = std::env::args().collect();
    let argv0 = args.first().cloned().unwrap_or_else(|| "flint-sh".into());
    let login = argv0.rsplit('/').next().unwrap_or("").starts_with('-');

    let (mut command_mode, mut force_interactive, mut read_stdin) = (false, false, false);
    let mut i = 1;
    while i < args.len() && args[i].starts_with('-') && args[i] != "-" {
        match args[i].as_str() {
            "--" => {
                i += 1;
                break;
            }
            "--version" => {
                write_fd(1, format!("flint-sh ({})\n", flint_core::version::long()).as_bytes());
                return 0;
            }
            "-h" | "--help" => {
                write_fd(1, USAGE.as_bytes());
                return 0;
            }
            flags => {
                for ch in flags[1..].chars() {
                    match ch {
                        'c' => command_mode = true,
                        'i' => force_interactive = true,
                        's' => read_stdin = true,
                        'l' => {}
                        other => {
                            write_fd(2, format!("flint-sh: invalid option -{other}\n{USAGE}").as_bytes());
                            return 2;
                        }
                    }
                }
            }
        }
        i += 1;
    }

    let mut command = None;
    let mut script = None;
    let name;
    let positional: Vec<String>;
    if command_mode {
        let Some(c) = args.get(i) else {
            errln("-c: option requires an argument");
            return 2;
        };
        command = Some(c.clone());
        name = args.get(i + 1).cloned().unwrap_or_else(|| argv0.clone());
        positional = args.get(i + 2..).map(<[_]>::to_vec).unwrap_or_default();
    } else if !read_stdin && i < args.len() {
        script = Some(args[i].clone());
        name = args[i].clone();
        positional = args[i + 1..].to_vec();
    } else {
        name = argv0.clone();
        positional = args.get(i..).map(<[_]>::to_vec).unwrap_or_default();
    }

    let mut sh = Shell::new(name, positional);
    // SAFETY: plain libc query.
    let tty = unsafe { libc::isatty(0) == 1 };
    let interactive = force_interactive || (command.is_none() && script.is_none() && tty);
    if interactive {
        init_job_control(&mut sh);
    }
    if login {
        source_if_exists(&mut sh, "/etc/profile");
        if let Some(home) = sh.get("HOME").map(str::to_string) {
            source_if_exists(&mut sh, &format!("{home}/.profile"));
        }
        sh.flow = None;
    }

    if let Some(c) = command {
        sh.run_source(&c);
    } else if let Some(path) = script {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                sh.run_source(&text);
            }
            Err(e) => {
                errln(&format!("{path}: {}", crate::util::strerror(e.raw_os_error().unwrap_or(0))));
                return 127;
            }
        }
    } else if interactive {
        return interactive_loop(&mut sh);
    } else {
        let mut text = String::new();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            // SAFETY: buffer pointer and length are valid.
            let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
            if n > 0 {
                bytes.extend_from_slice(&chunk[..n as usize]);
            } else if n < 0 && crate::util::errno() == libc::EINTR {
                continue;
            } else {
                break;
            }
        }
        text.push_str(&String::from_utf8_lossy(&bytes));
        sh.run_source(&text);
    }
    sh.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_templates() {
        let p = |t: &str, cwd: &str, root: bool| prompt_escapes(t, "bob", "box.example.org", cwd, "/home/bob", root);
        assert_eq!(p("\\u@\\h:\\w\\$ ", "/home/bob/src", false), "bob@box:~/src$ ");
        assert_eq!(p("\\w \\$ ", "/home/bob", true), "~ # ");
        assert_eq!(p("\\W> ", "/var/log", false), "log> ");
        assert_eq!(p("\\W> ", "/home/bob", false), "~> ");
        assert_eq!(p("\\W> ", "/", false), "/> ");
        assert_eq!(p("\\w", "/home/bobby", false), "/home/bobby", "only a whole path component counts as home");
        assert_eq!(p("a\\nb\\\\c\\[x\\]", "/", false), "a\nb\\cx");
        assert_eq!(p("\\q", "/", false), "\\q");
        assert_eq!(p("plain", "/", false), "plain");
    }
}
