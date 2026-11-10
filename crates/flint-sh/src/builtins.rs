//! Builtin commands.

use std::collections::BTreeMap;

use crate::jobs::JobState;
use crate::printf;
use crate::shell::{Flow, Shell};
use crate::testcmd;
use crate::util::{errln, out, out_bytes};

pub type Builtin = fn(&mut Shell, &[String]) -> i32;

pub fn lookup(name: &str) -> Option<Builtin> {
    Some(match name {
        ":" | "true" => |_, _| 0,
        "false" => |_, _| 1,
        "echo" => echo,
        "printf" => printf_b,
        "test" => test,
        "[" => bracket,
        "cd" => cd,
        "pwd" => pwd,
        "exit" => exit,
        "export" => export,
        "unset" => unset,
        "set" => set,
        "shift" => shift,
        "read" => read,
        "return" => ret,
        "break" => brk,
        "continue" => cont,
        "eval" => eval,
        "." | "source" => source,
        "exec" => exec,
        "type" => type_b,
        "command" => command,
        "umask" => umask,
        "jobs" => jobs,
        "fg" => fg,
        "bg" => bg,
        "wait" => wait,
        "kill" => kill,
        "local" => local,
        "trap" => trap,
        _ => return None,
    })
}

/// Special builtins keep their `VAR=value` prefixes after they run.
pub fn is_special(name: &str) -> bool {
    matches!(name, ":" | "." | "source" | "break" | "continue" | "eval" | "exec" | "exit" | "export" | "return" | "set" | "shift" | "unset")
}

fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------- output builtins

fn echo(_: &mut Shell, args: &[String]) -> i32 {
    let (mut newline, mut escapes) = (true, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => newline = false,
            "-e" => escapes = true,
            "-E" => escapes = false,
            "-ne" | "-en" => {
                newline = false;
                escapes = true;
            }
            _ => break,
        }
        i += 1;
    }
    let text = args[i..].join(" ");
    let mut bytes;
    let mut stop = false;
    if escapes {
        let (b, s) = printf::unescape(&text, true);
        bytes = b;
        stop = s;
    } else {
        bytes = text.into_bytes();
    }
    if newline && !stop {
        bytes.push(b'\n');
    }
    out_bytes(&bytes);
    0
}

fn printf_b(_: &mut Shell, args: &[String]) -> i32 {
    let Some(fmt) = args.first() else {
        errln("printf: usage: printf FORMAT [ARGUMENT...]");
        return 2;
    };
    let (bytes, errors) = printf::format(fmt, &args[1..]);
    out_bytes(&bytes);
    for e in &errors {
        errln(&format!("printf: {e}"));
    }
    i32::from(!errors.is_empty())
}

fn test(_: &mut Shell, args: &[String]) -> i32 {
    match testcmd::evaluate(args) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(m) => {
            errln(&format!("test: {m}"));
            2
        }
    }
}

fn bracket(sh: &mut Shell, args: &[String]) -> i32 {
    if args.last().map(String::as_str) != Some("]") {
        errln("[: missing ']'");
        return 2;
    }
    test(sh, &args[..args.len() - 1])
}

// ---------------------------------------------------------------- directories

fn cd(sh: &mut Shell, args: &[String]) -> i32 {
    let target = match args.first().map(String::as_str) {
        None => match sh.get("HOME") {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => {
                errln("cd: HOME not set");
                return 1;
            }
        },
        Some("-") => match sh.get("OLDPWD") {
            Some(d) => {
                let d = d.to_string();
                out(&format!("{d}\n"));
                d
            }
            None => {
                errln("cd: OLDPWD not set");
                return 1;
            }
        },
        Some(d) => d.to_string(),
    };
    let old = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if let Err(e) = std::env::set_current_dir(&target) {
        errln(&format!("cd: {target}: {}", crate::util::strerror(e.raw_os_error().unwrap_or(0))));
        return 1;
    }
    let new = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or(target);
    sh.set_exported("OLDPWD", old);
    sh.set_exported("PWD", new);
    0
}

fn pwd(_: &mut Shell, _: &[String]) -> i32 {
    match std::env::current_dir() {
        Ok(p) => {
            out(&format!("{}\n", p.display()));
            0
        }
        Err(e) => {
            errln(&format!("pwd: {e}"));
            1
        }
    }
}

// ---------------------------------------------------------------- variables

fn exit(sh: &mut Shell, args: &[String]) -> i32 {
    let code = match args.first() {
        None => sh.last_status,
        Some(a) => match a.parse::<i32>() {
            Ok(n) => n & 0xff,
            Err(_) => {
                errln(&format!("exit: {a}: numeric argument required"));
                2
            }
        },
    };
    sh.flow = Some(Flow::Exit(code));
    code
}

fn export(sh: &mut Shell, args: &[String]) -> i32 {
    if args.is_empty() || args == ["-p"] {
        let sorted: BTreeMap<_, _> = sh.vars.iter().filter(|(_, v)| v.exported).collect();
        for (k, v) in sorted {
            out(&format!("export {k}={}\n", quote(&v.value)));
        }
        return 0;
    }
    let mut status = 0;
    for a in args {
        let (name, value) = match a.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        if !valid_name(name) {
            errln(&format!("export: '{a}': not a valid identifier"));
            status = 1;
            continue;
        }
        match value {
            Some(v) => sh.set_exported(name, v),
            None => {
                let cur = sh.get(name).unwrap_or("").to_string();
                sh.set_exported(name, cur);
            }
        }
    }
    status
}

fn unset(sh: &mut Shell, args: &[String]) -> i32 {
    let mut funcs = false;
    for a in args {
        match a.as_str() {
            "-f" => funcs = true,
            "-v" => funcs = false,
            name => {
                if funcs {
                    sh.funcs.remove(name);
                } else {
                    sh.unset(name);
                }
            }
        }
    }
    0
}

fn set(sh: &mut Shell, args: &[String]) -> i32 {
    if args.is_empty() {
        let sorted: BTreeMap<_, _> = sh.vars.iter().collect();
        for (k, v) in sorted {
            out(&format!("{k}={}\n", quote(&v.value)));
        }
        return 0;
    }
    let mut i = 0;
    // Options (-e, -u, +x, ...) are accepted and ignored for now.
    while i < args.len() && args[i] != "--" && (args[i].starts_with('-') || args[i].starts_with('+')) && args[i].len() > 1 {
        i += 1;
    }
    if i < args.len() && args[i] == "--" {
        i += 1;
    }
    if i < args.len() || args.iter().any(|a| a == "--") {
        sh.positional = args[i..].to_vec();
    }
    0
}

fn shift(sh: &mut Shell, args: &[String]) -> i32 {
    let n = match args.first() {
        None => 1,
        Some(a) => match a.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                errln(&format!("shift: {a}: numeric argument required"));
                return 2;
            }
        },
    };
    if n > sh.positional.len() {
        errln("shift: shift count out of range");
        return 1;
    }
    sh.positional.drain(..n);
    0
}

fn local(sh: &mut Shell, args: &[String]) -> i32 {
    if sh.func_depth == 0 {
        errln("local: can only be used in a function");
        return 1;
    }
    for a in args {
        let (name, value) = match a.split_once('=') {
            Some((n, v)) => (n, v.to_string()),
            None => (a.as_str(), String::new()),
        };
        if !valid_name(name) {
            errln(&format!("local: '{a}': not a valid identifier"));
            return 1;
        }
        let old = sh.vars.get(name).cloned();
        if let Some(frame) = sh.locals.last_mut() {
            frame.push((name.to_string(), old));
        }
        sh.set(name, value);
    }
    0
}

/// Fields of a `read` line: the first `nvars - 1` words, then the rest (trimmed) in the last.
pub fn split_read_fields(line: &str, ifs: &str, nvars: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let is_ws = |c: char| ifs.contains(c) && c.is_whitespace();
    let is_sep = |c: char| ifs.contains(c);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() && is_ws(chars[i]) {
        i += 1;
    }
    while out.len() + 1 < nvars && i < chars.len() {
        let start = i;
        while i < chars.len() && !is_sep(chars[i]) {
            i += 1;
        }
        out.push(chars[start..i].iter().collect());
        // skip the separator: whitespace runs collapse, a non-whitespace separator counts once
        let mut seen_nonws = false;
        while i < chars.len() && is_sep(chars[i]) {
            if !is_ws(chars[i]) {
                if seen_nonws {
                    break;
                }
                seen_nonws = true;
            }
            i += 1;
        }
    }
    if out.len() < nvars {
        let mut rest: Vec<char> = chars[i.min(chars.len())..].to_vec();
        while rest.last().is_some_and(|&c| is_ws(c)) {
            rest.pop();
        }
        if !rest.is_empty() || i < chars.len() || out.len() + 1 == nvars {
            out.push(rest.into_iter().collect());
        }
    }
    out
}

/// Read one line from fd 0 a byte at a time, so no input meant for later commands is consumed.
fn read_line_fd0() -> Option<(String, bool)> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        // SAFETY: buffer pointer and length are valid.
        let n = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
        if n < 0 && crate::util::errno() == libc::EINTR {
            continue;
        }
        if n <= 0 {
            return if line.is_empty() { None } else { Some((String::from_utf8_lossy(&line).into_owned(), false)) };
        }
        if byte[0] == b'\n' {
            return Some((String::from_utf8_lossy(&line).into_owned(), true));
        }
        line.push(byte[0]);
    }
}

fn read(sh: &mut Shell, args: &[String]) -> i32 {
    let mut raw = false;
    let mut names: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-r" => raw = true,
            n if valid_name(n) => names.push(n),
            other => {
                errln(&format!("read: '{other}': not a valid identifier"));
                return 2;
            }
        }
    }
    let mut line = String::new();
    let mut complete;
    loop {
        match read_line_fd0() {
            None => {
                complete = false;
                if line.is_empty() {
                    for n in if names.is_empty() { vec!["REPLY"] } else { names.clone() } {
                        sh.set(n, String::new());
                    }
                    return 1;
                }
                break;
            }
            Some((l, had_nl)) => {
                complete = had_nl;
                if !raw && l.ends_with('\\') && had_nl {
                    line.push_str(&l[..l.len() - 1]);
                    continue;
                }
                line.push_str(&l);
                break;
            }
        }
    }
    if !raw {
        // remove remaining backslashes (`\x` becomes `x`)
        let mut cleaned = String::new();
        let mut it = line.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                if let Some(n) = it.next() {
                    cleaned.push(n);
                }
            } else {
                cleaned.push(c);
            }
        }
        line = cleaned;
    }
    if names.is_empty() {
        sh.set("REPLY", line);
    } else {
        let ifs = sh.get("IFS").unwrap_or(" \t\n").to_string();
        let mut fields = split_read_fields(&line, &ifs, names.len());
        fields.resize(names.len(), String::new());
        for (n, v) in names.iter().zip(fields) {
            sh.set(n, v);
        }
    }
    i32::from(!complete)
}

// ---------------------------------------------------------------- control flow

fn ret(sh: &mut Shell, args: &[String]) -> i32 {
    let code = args.first().and_then(|a| a.parse::<i32>().ok()).unwrap_or(sh.last_status) & 0xff;
    sh.flow = Some(Flow::Return(code));
    code
}

fn loop_count(name: &str, args: &[String]) -> Option<u32> {
    match args.first() {
        None => Some(1),
        Some(a) => match a.parse::<u32>() {
            Ok(n) if n >= 1 => Some(n),
            _ => {
                errln(&format!("{name}: {a}: loop count out of range"));
                None
            }
        },
    }
}

fn brk(sh: &mut Shell, args: &[String]) -> i32 {
    if sh.loop_depth == 0 {
        errln("break: only meaningful in a loop");
        return 0;
    }
    match loop_count("break", args) {
        Some(n) => {
            sh.flow = Some(Flow::Break(n));
            0
        }
        None => 1,
    }
}

fn cont(sh: &mut Shell, args: &[String]) -> i32 {
    if sh.loop_depth == 0 {
        errln("continue: only meaningful in a loop");
        return 0;
    }
    match loop_count("continue", args) {
        Some(n) => {
            sh.flow = Some(Flow::Continue(n));
            0
        }
        None => 1,
    }
}

fn eval(sh: &mut Shell, args: &[String]) -> i32 {
    if args.is_empty() {
        return 0;
    }
    sh.run_source(&args.join(" "))
}

fn source(sh: &mut Shell, args: &[String]) -> i32 {
    let Some(file) = args.first() else {
        errln(".: filename argument required");
        return 2;
    };
    let path = if file.contains('/') { Some(file.clone()) } else { sh.find_in_path(file).or_else(|| std::path::Path::new(file).exists().then(|| file.clone())) };
    let text = match path.as_deref().map(std::fs::read_to_string) {
        Some(Ok(t)) => t,
        Some(Err(e)) => {
            errln(&format!("{file}: {}", crate::util::strerror(e.raw_os_error().unwrap_or(0))));
            return 1;
        }
        None => {
            errln(&format!("{file}: not found"));
            return 1;
        }
    };
    let saved = (args.len() > 1).then(|| std::mem::replace(&mut sh.positional, args[1..].to_vec()));
    let mut st = sh.run_source(&text);
    if let Some(Flow::Return(n)) = sh.flow {
        sh.flow = None;
        st = n;
    }
    if let Some(p) = saved {
        sh.positional = p;
    }
    st
}

fn exec(sh: &mut Shell, args: &[String]) -> i32 {
    if args.is_empty() {
        return 0;
    }
    sh.exec_in_place(args, &[], &[])
}

// ---------------------------------------------------------------- introspection

fn describe(sh: &Shell, name: &str) -> Option<String> {
    if sh.funcs.contains_key(name) {
        Some(format!("{name} is a function"))
    } else if lookup(name).is_some() {
        Some(format!("{name} is a shell builtin"))
    } else {
        sh.find_in_path(name).map(|p| format!("{name} is {p}"))
    }
}

fn type_b(sh: &mut Shell, args: &[String]) -> i32 {
    let mut status = 0;
    for a in args {
        match describe(sh, a) {
            Some(d) => out(&format!("{d}\n")),
            None => {
                errln(&format!("type: {a}: not found"));
                status = 1;
            }
        }
    }
    status
}

fn command(sh: &mut Shell, args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => 0,
        Some("-v") | Some("-V") => {
            let mut status = 0;
            for a in &args[1..] {
                if sh.funcs.contains_key(a.as_str()) || lookup(a).is_some() {
                    out(&format!("{a}\n"));
                } else if let Some(p) = sh.find_in_path(a) {
                    out(&format!("{p}\n"));
                } else {
                    status = 1;
                }
            }
            status
        }
        Some(_) => sh.run_argv(args, &[], &[], false, false),
    }
}

fn umask(_: &mut Shell, args: &[String]) -> i32 {
    match args.first() {
        None => {
            // SAFETY: umask has no preconditions; the value is restored immediately.
            let cur = unsafe {
                let m = libc::umask(0);
                libc::umask(m);
                m
            };
            out(&format!("{cur:04o}\n"));
            0
        }
        Some(a) => match u32::from_str_radix(a, 8) {
            Ok(m) if m <= 0o777 => {
                // SAFETY: umask has no preconditions.
                unsafe { libc::umask(m as libc::mode_t) };
                0
            }
            _ => {
                errln(&format!("umask: {a}: invalid mode"));
                1
            }
        },
    }
}

fn trap(_: &mut Shell, _: &[String]) -> i32 {
    errln("trap: not supported yet");
    1
}

// ---------------------------------------------------------------- jobs and signals

fn jobs(sh: &mut Shell, _: &[String]) -> i32 {
    sh.reap_jobs();
    let n = sh.jobs.list.len();
    for (idx, j) in sh.jobs.list.iter().enumerate() {
        if j.pids.is_empty() {
            continue;
        }
        let mark = if idx + 1 == n {
            '+'
        } else if idx + 2 == n {
            '-'
        } else {
            ' '
        };
        let state = match j.state {
            JobState::Running => "Running",
            JobState::Stopped => "Stopped",
        };
        out(&format!("[{}]{mark}  {state:<22} {}\n", j.id, j.cmd));
    }
    0
}

fn fg(sh: &mut Shell, args: &[String]) -> i32 {
    let Some(id) = sh.jobs.resolve(args.first().map(String::as_str)) else {
        errln("fg: no such job");
        return 1;
    };
    let job = sh.jobs.get_mut(id).map(|j| (j.pgid, j.pids.clone(), j.cmd.clone(), j.state)).expect("resolved job exists");
    let (pgid, pids, cmd, state) = job;
    out(&format!("{cmd}\n"));
    if state == JobState::Stopped {
        // SAFETY: signalling the job's process group.
        unsafe { libc::kill(-pgid, libc::SIGCONT) };
    }
    if let Some(j) = sh.jobs.get_mut(id) {
        j.state = JobState::Running;
    }
    sh.wait_foreground(&pids, pgid, &cmd, Some(id))
}

fn bg(sh: &mut Shell, args: &[String]) -> i32 {
    let Some(id) = sh.jobs.resolve(args.first().map(String::as_str)) else {
        errln("bg: no such job");
        return 1;
    };
    if let Some(j) = sh.jobs.get_mut(id) {
        // SAFETY: signalling the job's process group.
        unsafe { libc::kill(-j.pgid, libc::SIGCONT) };
        j.state = JobState::Running;
        out(&format!("[{}]+ {} &\n", j.id, j.cmd));
    }
    0
}

fn wait_pid(pid: i32) -> i32 {
    let mut st = 0;
    loop {
        // SAFETY: `st` is a valid out pointer.
        let r = unsafe { libc::waitpid(pid, &mut st, 0) };
        if r == -1 && crate::util::errno() == libc::EINTR {
            continue;
        }
        if r == -1 {
            return 127;
        }
        return crate::shell::decode_status(st);
    }
}

fn wait(sh: &mut Shell, args: &[String]) -> i32 {
    if args.is_empty() {
        let pids: Vec<i32> = sh.jobs.list.iter().flat_map(|j| j.pids.clone()).collect();
        for p in pids {
            wait_pid(p);
        }
        sh.jobs.list.clear();
        return 0;
    }
    let mut status = 0;
    for a in args {
        let pid = if a.starts_with('%') {
            sh.jobs.resolve(Some(a)).and_then(|id| sh.jobs.get_mut(id).map(|j| j.last_pid))
        } else {
            a.parse::<i32>().ok()
        };
        match pid {
            Some(p) => {
                status = wait_pid(p);
                if let Some(j) = sh.jobs.job_of_pid(p) {
                    let id = j.id;
                    sh.jobs.remove(id);
                }
            }
            None => {
                errln(&format!("wait: {a}: no such job or pid"));
                status = 127;
            }
        }
    }
    status
}

const SIGNALS: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("KILL", 9),
    ("USR1", 10),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
];

fn parse_signal(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return (0..=64).contains(&n).then_some(n);
    }
    let upper = s.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

fn kill(sh: &mut Shell, args: &[String]) -> i32 {
    let mut sig = libc::SIGTERM;
    let mut i = 0;
    if let Some(first) = args.first() {
        if first == "-l" {
            out(&format!("{}\n", SIGNALS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" ")));
            return 0;
        }
        if first == "-s" {
            match args.get(1).and_then(|s| parse_signal(s)) {
                Some(s) => sig = s,
                None => {
                    errln("kill: invalid signal");
                    return 1;
                }
            }
            i = 2;
        } else if let Some(spec) = first.strip_prefix('-') {
            if let Some(s) = parse_signal(spec) {
                sig = s;
                i = 1;
            }
        }
    }
    if i >= args.len() {
        errln("kill: usage: kill [-s SIGNAL | -SIGNAL] pid | %job ...");
        return 1;
    }
    let mut status = 0;
    for a in &args[i..] {
        let target = if a.starts_with('%') { sh.jobs.resolve(Some(a)).and_then(|id| sh.jobs.get_mut(id).map(|j| -j.pgid)) } else { a.parse::<i32>().ok() };
        match target {
            // SAFETY: kill(2) takes plain integers.
            Some(t) if unsafe { libc::kill(t, sig) } == 0 => {}
            Some(t) => {
                errln(&format!("kill: {t}: {}", crate::util::strerror(crate::util::errno())));
                status = 1;
            }
            None => {
                errln(&format!("kill: {a}: no such job or invalid process id"));
                status = 1;
            }
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_splitting() {
        let ws = " \t\n";
        assert_eq!(split_read_fields("a b c", ws, 3), ["a", "b", "c"]);
        assert_eq!(split_read_fields("a b c d e", ws, 2), ["a", "b c d e"]);
        assert_eq!(split_read_fields("  a   b  ", ws, 2), ["a", "b"]);
        assert_eq!(split_read_fields("a", ws, 3), ["a"]);
        assert_eq!(split_read_fields("one two three", ws, 1), ["one two three"]);
        assert_eq!(split_read_fields("  padded  ", ws, 1), ["padded"]);
        assert_eq!(split_read_fields("a:b:c", ":", 3), ["a", "b", "c"]);
        assert_eq!(split_read_fields("a::c", ":", 3), ["a", "", "c"]);
        assert_eq!(split_read_fields("", ws, 2), Vec::<String>::new());
    }

    #[test]
    fn names_and_quoting() {
        assert!(valid_name("FOO_1"));
        assert!(!valid_name("1FOO"));
        assert!(!valid_name("A-B"));
        assert!(!valid_name(""));
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn signals() {
        assert_eq!(parse_signal("TERM"), Some(15));
        assert_eq!(parse_signal("sigkill"), Some(9));
        assert_eq!(parse_signal("9"), Some(9));
        assert_eq!(parse_signal("NOPE"), None);
    }
}
