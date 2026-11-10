//! The shell: variables, command execution, redirections, pipelines, processes and jobs.

use std::collections::HashMap;
use std::ffi::CString;
use std::rc::Rc;

use crate::ast::{Command, Item, Redir, RedirKind, Simple};
use crate::builtins;
use crate::glob;
use crate::jobs::{JobState, Jobs};
use crate::parser::{parse, ParseError};
use crate::util::{errln, errno, strerror, write_fd};

#[derive(Clone)]
pub struct Var {
    pub value: String,
    pub exported: bool,
}

/// Pending non-local control flow: `break`, `continue`, `return`, `exit`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Break(u32),
    Continue(u32),
    Return(i32),
    Exit(i32),
}

#[derive(Clone, Copy)]
pub enum Pgrp {
    Inherit,
    New,
    Join(i32),
}

enum LoopAct {
    Next,
    Break,
    Continue,
}

pub struct Shell {
    pub vars: HashMap<String, Var>,
    pub funcs: HashMap<String, Rc<Command>>,
    pub positional: Vec<String>,
    pub arg0: String,
    pub last_status: i32,
    pub last_bg: i32,
    pub interactive: bool,
    pub shell_pgid: i32,
    pub jobs: Jobs,
    pub flow: Option<Flow>,
    pub func_depth: u32,
    /// How many `while`/`until`/`for` loops are running (for `break` and `continue`).
    pub loop_depth: u32,
    pub locals: Vec<Vec<(String, Option<Var>)>>,
    pub pid: i32,
    /// Status of the last command substitution in the current simple command (`x=$(false)`).
    pub cmdsub_status: Option<i32>,
}

pub fn decode_status(st: i32) -> i32 {
    if libc::WIFEXITED(st) {
        libc::WEXITSTATUS(st)
    } else if libc::WIFSIGNALED(st) {
        128 + libc::WTERMSIG(st)
    } else {
        0
    }
}

fn signal_name(sig: i32) -> String {
    match sig {
        libc::SIGHUP => "Hangup".into(),
        libc::SIGQUIT => "Quit".into(),
        libc::SIGILL => "Illegal instruction".into(),
        libc::SIGABRT => "Aborted".into(),
        libc::SIGBUS => "Bus error".into(),
        libc::SIGFPE => "Floating point exception".into(),
        libc::SIGKILL => "Killed".into(),
        libc::SIGSEGV => "Segmentation fault".into(),
        libc::SIGTERM => "Terminated".into(),
        libc::SIGUSR1 => "User defined signal 1".into(),
        libc::SIGUSR2 => "User defined signal 2".into(),
        libc::SIGALRM => "Alarm clock".into(),
        n => format!("Signal {n}"),
    }
}

/// A short text for the job table.
pub fn render(cmd: &Command) -> String {
    match cmd {
        Command::Simple(s) => s.words.join(" "),
        Command::Pipeline { stages, .. } => stages.iter().map(render).collect::<Vec<_>>().join(" | "),
        Command::List(items) if items.len() == 1 => render(&items[0].cmd),
        Command::AndOr(first, _) => format!("{} ...", render(first)),
        Command::Subshell(_) => "( ... )".into(),
        _ => "...".into(),
    }
}

fn restore(saved: &[(i32, i32)]) {
    for &(fd, copy) in saved.iter().rev() {
        // SAFETY: plain descriptor operations.
        unsafe {
            if copy >= 0 {
                libc::dup2(copy, fd);
                libc::close(copy);
            } else {
                libc::close(fd);
            }
        }
    }
}

fn open_file(path: &str, flags: i32) -> Result<i32, String> {
    let c = CString::new(path).map_err(|_| format!("{path}: invalid name"))?;
    // SAFETY: valid NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), flags, 0o666) };
    if fd < 0 {
        Err(format!("{path}: {}", strerror(errno())))
    } else {
        Ok(fd)
    }
}

impl Shell {
    pub fn new(arg0: String, args: Vec<String>) -> Shell {
        let mut vars = HashMap::new();
        for (k, v) in std::env::vars_os() {
            vars.insert(k.to_string_lossy().into_owned(), Var { value: v.to_string_lossy().into_owned(), exported: true });
        }
        // SAFETY: plain libc query.
        let pid = unsafe { libc::getpid() };
        let mut sh = Shell {
            vars,
            funcs: HashMap::new(),
            positional: args,
            arg0,
            last_status: 0,
            last_bg: 0,
            interactive: false,
            shell_pgid: pid,
            jobs: Jobs::default(),
            flow: None,
            func_depth: 0,
            loop_depth: 0,
            locals: Vec::new(),
            pid,
            cmdsub_status: None,
        };
        if !sh.vars.contains_key("PATH") {
            sh.set_exported("PATH", "/usr/local/bin:/usr/bin:/bin".to_string());
        }
        if let Ok(cwd) = std::env::current_dir() {
            sh.set_exported("PWD", cwd.to_string_lossy().into_owned());
        }
        sh
    }

    // ------------------------------------------------------------ variables

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|v| v.value.as_str())
    }

    pub fn set(&mut self, name: &str, value: String) {
        match self.vars.get_mut(name) {
            Some(v) => v.value = value,
            None => {
                self.vars.insert(name.to_string(), Var { value, exported: false });
            }
        }
    }

    pub fn set_exported(&mut self, name: &str, value: String) {
        self.vars.insert(name.to_string(), Var { value, exported: true });
    }

    pub fn unset(&mut self, name: &str) {
        self.vars.remove(name);
    }

    /// `KEY=VALUE` strings for exported variables, with `extra` overriding.
    pub fn environ(&self, extra: &[(String, String)]) -> Vec<CString> {
        let mut m: Vec<(&str, &str)> = self.vars.iter().filter(|(_, v)| v.exported).map(|(k, v)| (k.as_str(), v.value.as_str())).collect();
        m.retain(|(k, _)| !extra.iter().any(|(ek, _)| ek == k));
        m.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        m.sort();
        m.iter().filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok()).collect()
    }

    fn apply_temp_assigns(&mut self, assigns: &[(String, String)]) -> Vec<(String, Option<Var>)> {
        let mut saved = Vec::new();
        for (n, v) in assigns {
            saved.push((n.clone(), self.vars.get(n).cloned()));
            self.set_exported(n, v.clone());
        }
        saved
    }

    fn restore_assigns(&mut self, saved: Vec<(String, Option<Var>)>) {
        for (n, old) in saved.into_iter().rev() {
            match old {
                Some(v) => {
                    self.vars.insert(n, v);
                }
                None => {
                    self.vars.remove(&n);
                }
            }
        }
    }

    // ------------------------------------------------------------ running source

    pub fn run_source(&mut self, src: &str) -> i32 {
        match parse(src) {
            Ok(cmd) => self.exec(&cmd),
            Err(ParseError::Incomplete) => {
                errln("syntax error: unexpected end of input");
                self.last_status = 2;
                2
            }
            Err(ParseError::Syntax(m)) => {
                errln(&format!("syntax error: {m}"));
                self.last_status = 2;
                2
            }
        }
    }

    pub fn exec(&mut self, cmd: &Command) -> i32 {
        let st = self.exec_inner(cmd);
        self.last_status = st;
        st
    }

    fn exec_inner(&mut self, cmd: &Command) -> i32 {
        match cmd {
            Command::Simple(s) => self.exec_simple(s, false),
            Command::Pipeline { negate, stages } => {
                let st = if stages.len() == 1 { self.exec(&stages[0]) } else { self.exec_pipeline(stages) };
                if *negate {
                    i32::from(st == 0)
                } else {
                    st
                }
            }
            Command::AndOr(first, rest) => {
                let mut st = self.exec(first);
                for (is_and, c) in rest {
                    if self.flow.is_some() {
                        break;
                    }
                    if (*is_and && st == 0) || (!*is_and && st != 0) {
                        st = self.exec(c);
                    }
                }
                st
            }
            Command::List(items) => self.exec_list(items),
            Command::Subshell(inner) => {
                let pgrp = self.pgrp_new();
                match self.fork_run(pgrp, true, -1, -1, &[], |sh| sh.exec(inner)) {
                    Ok(pid) => self.wait_foreground(&[pid], pid, "( ... )", None),
                    Err(e) => {
                        errln(&format!("fork: {e}"));
                        1
                    }
                }
            }
            Command::Group(inner) => self.exec(inner),
            Command::If { arms, otherwise } => {
                for (cond, body) in arms {
                    let c = self.exec(cond);
                    if self.flow.is_some() {
                        return c;
                    }
                    if c == 0 {
                        return self.exec(body);
                    }
                }
                match otherwise {
                    Some(b) => self.exec(b),
                    None => 0,
                }
            }
            Command::While { cond, body, until } => {
                let mut st = 0;
                self.loop_depth += 1;
                loop {
                    let c = self.exec(cond);
                    if self.flow.is_some() {
                        st = c;
                        break;
                    }
                    if (c == 0) == *until {
                        break;
                    }
                    st = self.exec(body);
                    match self.loop_flow() {
                        Some(LoopAct::Break) => break,
                        Some(LoopAct::Continue) | Some(LoopAct::Next) => {}
                        None => break, // exit/return/outer break: propagate
                    }
                }
                self.loop_depth -= 1;
                st
            }
            Command::For { var, words, body } => {
                let values: Vec<String> = match words {
                    Some(ws) => ws.iter().flat_map(|w| self.expand_fields(w)).collect(),
                    None => self.positional.clone(),
                };
                let mut st = 0;
                self.loop_depth += 1;
                for v in values {
                    self.set(var, v);
                    st = self.exec(body);
                    match self.loop_flow() {
                        Some(LoopAct::Break) => break,
                        Some(LoopAct::Continue) | Some(LoopAct::Next) => {}
                        None => break,
                    }
                }
                self.loop_depth -= 1;
                st
            }
            Command::Case { word, arms } => {
                let value = self.expand_one(word);
                for (pats, body) in arms {
                    for p in pats {
                        let pat = self.expand_pattern(p);
                        if glob::matches(&pat, &value) {
                            return self.exec(body);
                        }
                    }
                }
                0
            }
            Command::FuncDef { name, body } => {
                self.funcs.insert(name.clone(), Rc::clone(body));
                0
            }
            Command::Redirected(inner, redirs) => self.with_redirs(redirs, |sh| sh.exec(inner)),
        }
    }

    /// After a loop body: what should the loop do? `None` means leave (a flow is still pending).
    fn loop_flow(&mut self) -> Option<LoopAct> {
        match self.flow {
            None => Some(LoopAct::Next),
            Some(Flow::Break(n)) => {
                if n > 1 {
                    self.flow = Some(Flow::Break(n - 1));
                    None
                } else {
                    self.flow = None;
                    Some(LoopAct::Break)
                }
            }
            Some(Flow::Continue(n)) => {
                if n > 1 {
                    self.flow = Some(Flow::Continue(n - 1));
                    None
                } else {
                    self.flow = None;
                    Some(LoopAct::Continue)
                }
            }
            Some(_) => None,
        }
    }

    fn exec_list(&mut self, items: &[Item]) -> i32 {
        let mut st = 0;
        for it in items {
            st = if it.background { self.exec_background(&it.cmd) } else { self.exec(&it.cmd) };
            if self.flow.is_some() {
                break;
            }
        }
        st
    }

    // ------------------------------------------------------------ simple commands

    pub fn exec_simple(&mut self, s: &Simple, direct: bool) -> i32 {
        self.cmdsub_status = None;
        let mut argv: Vec<String> = Vec::new();
        for w in &s.words {
            argv.extend(self.expand_fields(w));
            if self.flow.is_some() {
                return self.last_status;
            }
        }
        if argv.is_empty() {
            for (n, v) in &s.assigns {
                let val = self.expand_one(v);
                self.set(n, val);
            }
            let st = self.with_redirs(&s.redirs, |_| 0);
            return if st != 0 { st } else { self.cmdsub_status.unwrap_or(0) };
        }
        let assigns: Vec<(String, String)> = s.assigns.iter().map(|(n, v)| (n.clone(), self.expand_one(v))).collect();
        self.run_argv(&argv, &assigns, &s.redirs, direct, true)
    }

    /// Run an already expanded command: function, builtin, or external program.
    pub fn run_argv(&mut self, argv: &[String], assigns: &[(String, String)], redirs: &[Redir], direct: bool, allow_funcs: bool) -> i32 {
        let name = argv[0].as_str();
        if allow_funcs {
            if let Some(body) = self.funcs.get(name).cloned() {
                let saved = self.apply_temp_assigns(assigns);
                let st = self.with_redirs(redirs, |sh| sh.call_function(body, &argv[1..]));
                self.restore_assigns(saved);
                return st;
            }
        }
        if let Some(b) = builtins::lookup(name) {
            let saved = if builtins::is_special(name) {
                for (n, v) in assigns {
                    self.set(n, v.clone());
                }
                Vec::new()
            } else {
                self.apply_temp_assigns(assigns)
            };
            let st = self.with_redirs(redirs, |sh| b(sh, &argv[1..]));
            self.restore_assigns(saved);
            return st;
        }
        if direct {
            return self.exec_in_place(argv, assigns, redirs);
        }
        let pgrp = self.pgrp_new();
        match self.fork_run(pgrp, true, -1, -1, &[], |sh| sh.exec_in_place(argv, assigns, redirs)) {
            Ok(pid) => self.wait_foreground(&[pid], pid, &argv.join(" "), None),
            Err(e) => {
                errln(&format!("fork: {e}"));
                1
            }
        }
    }

    pub fn call_function(&mut self, body: Rc<Command>, args: &[String]) -> i32 {
        let saved_pos = std::mem::replace(&mut self.positional, args.to_vec());
        self.func_depth += 1;
        self.locals.push(Vec::new());
        let mut st = self.exec(&body);
        if let Some(Flow::Return(n)) = self.flow {
            self.flow = None;
            st = n;
        }
        for (name, old) in self.locals.pop().unwrap_or_default().into_iter().rev() {
            match old {
                Some(v) => {
                    self.vars.insert(name, v);
                }
                None => {
                    self.vars.remove(&name);
                }
            }
        }
        self.func_depth -= 1;
        self.positional = saved_pos;
        st
    }

    // ------------------------------------------------------------ redirections

    pub fn with_redirs<F: FnOnce(&mut Shell) -> i32>(&mut self, redirs: &[Redir], f: F) -> i32 {
        if redirs.is_empty() {
            return f(self);
        }
        let mut saved = Vec::new();
        for r in redirs {
            if let Err(m) = self.apply_redir(r, &mut saved) {
                errln(&m);
                restore(&saved);
                return 1;
            }
        }
        let st = f(self);
        restore(&saved);
        st
    }

    fn apply_redir(&mut self, r: &Redir, saved: &mut Vec<(i32, i32)>) -> Result<(), String> {
        let fd = r.fd;
        // SAFETY: plain descriptor operations throughout.
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
        saved.push((fd, copy));
        let dup_onto = |nfd: i32| unsafe {
            if nfd != fd {
                libc::dup2(nfd, fd);
                libc::close(nfd);
            }
        };
        match &r.kind {
            RedirKind::In(w) => {
                let path = self.expand_one(w);
                dup_onto(open_file(&path, libc::O_RDONLY)?);
            }
            RedirKind::Out(w) => {
                let path = self.expand_one(w);
                dup_onto(open_file(&path, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC)?);
            }
            RedirKind::Append(w) => {
                let path = self.expand_one(w);
                dup_onto(open_file(&path, libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND)?);
            }
            RedirKind::Dup(w) => {
                let target = self.expand_one(w);
                if target == "-" {
                    unsafe { libc::close(fd) };
                } else {
                    let n: i32 = target.parse().map_err(|_| format!("{target}: bad redirection target"))?;
                    if unsafe { libc::fcntl(n, libc::F_GETFD) } == -1 {
                        return Err(format!("{n}: Bad file descriptor"));
                    }
                    unsafe { libc::dup2(n, fd) };
                }
            }
            RedirKind::HereDoc { body, expand } => {
                let text = if *expand { self.expand_dq_text(body) } else { body.clone() };
                if text.len() > 60_000 {
                    return Err("here-document too large (limit 60000 bytes)".into());
                }
                let mut fds = [0i32; 2];
                if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                    return Err(format!("pipe: {}", strerror(errno())));
                }
                write_fd(fds[1], text.as_bytes());
                unsafe { libc::close(fds[1]) };
                dup_onto(fds[0]);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ processes

    fn pgrp_new(&self) -> Pgrp {
        if self.interactive {
            Pgrp::New
        } else {
            Pgrp::Inherit
        }
    }

    /// Fork; the child runs `body` and exits with its status. `stdin`/`stdout` (when >= 0) are
    /// dup'ed onto 0/1 and every fd in `close` is closed in the child.
    pub fn fork_run<F: FnOnce(&mut Shell) -> i32>(&mut self, pgrp: Pgrp, foreground: bool, stdin: i32, stdout: i32, close: &[i32], body: F) -> Result<i32, String> {
        // SAFETY: fork is only used from the single-threaded shell.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(strerror(errno()));
        }
        if pid == 0 {
            // SAFETY: plain libc calls in the child before it runs `body`.
            unsafe {
                match pgrp {
                    Pgrp::New => {
                        libc::setpgid(0, 0);
                        if foreground && self.interactive {
                            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                            libc::tcsetpgrp(0, libc::getpid());
                        }
                    }
                    Pgrp::Join(g) => {
                        libc::setpgid(0, g);
                    }
                    Pgrp::Inherit => {}
                }
                for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU, libc::SIGPIPE, libc::SIGCHLD] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                if stdin >= 0 && stdin != 0 {
                    libc::dup2(stdin, 0);
                }
                if stdout >= 0 && stdout != 1 {
                    libc::dup2(stdout, 1);
                }
                for &fd in close {
                    libc::close(fd);
                }
            }
            self.interactive = false;
            self.jobs = Jobs::default();
            let st = body(self);
            let st = match self.flow {
                Some(Flow::Exit(n)) => n,
                _ => st,
            };
            // SAFETY: terminate the child without running the parent's cleanup.
            unsafe { libc::_exit(st & 0xff) };
        }
        // SAFETY: mirrors the child's setpgid to close the race; errors are expected and ignored.
        unsafe {
            match pgrp {
                Pgrp::New => {
                    libc::setpgid(pid, pid);
                }
                Pgrp::Join(g) => {
                    libc::setpgid(pid, g);
                }
                Pgrp::Inherit => {}
            }
        }
        Ok(pid)
    }

    pub fn find_in_path(&self, name: &str) -> Option<String> {
        if name.contains('/') {
            return Some(name.to_string());
        }
        let path = self.get("PATH").unwrap_or("/usr/local/bin:/usr/bin:/bin");
        for dir in path.split(':') {
            let dir = if dir.is_empty() { "." } else { dir };
            let cand = format!("{dir}/{name}");
            if let Ok(c) = CString::new(cand.as_str()) {
                // SAFETY: valid C string.
                if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 && std::fs::metadata(&cand).map_or(false, |m| m.is_file()) {
                    return Some(cand);
                }
            }
        }
        None
    }

    /// Replace this process with the program. Returns only on failure, with the shell status.
    pub fn exec_in_place(&mut self, argv: &[String], assigns: &[(String, String)], redirs: &[Redir]) -> i32 {
        for r in redirs {
            if let Err(m) = self.apply_redir(r, &mut Vec::new()) {
                errln(&m);
                return 1;
            }
        }
        let name = &argv[0];
        let Some(path) = self.find_in_path(name) else {
            errln(&format!("{name}: not found"));
            return 127;
        };
        let env = self.environ(assigns);
        let to_c = |v: &[String]| -> Vec<CString> { v.iter().filter_map(|s| CString::new(s.as_str()).ok()).collect() };
        let (Ok(path_c), args_c) = (CString::new(path.as_str()), to_c(argv)) else {
            errln(&format!("{name}: invalid name"));
            return 126;
        };
        let mut argp: Vec<*const libc::c_char> = args_c.iter().map(|c| c.as_ptr()).collect();
        argp.push(std::ptr::null());
        let mut envp: Vec<*const libc::c_char> = env.iter().map(|c| c.as_ptr()).collect();
        envp.push(std::ptr::null());
        // SAFETY: all pointers are valid NUL-terminated strings and the arrays end with null.
        unsafe { libc::execve(path_c.as_ptr(), argp.as_ptr(), envp.as_ptr()) };
        let e = errno();
        if e == libc::ENOEXEC {
            // A script without #!: run it with this shell.
            let mut sh_args = vec!["/bin/sh".to_string(), path.clone()];
            sh_args.extend(argv[1..].iter().cloned());
            let sh_c = to_c(&sh_args);
            let mut p2: Vec<*const libc::c_char> = sh_c.iter().map(|c| c.as_ptr()).collect();
            p2.push(std::ptr::null());
            // SAFETY: as above.
            unsafe { libc::execve(b"/bin/sh\0".as_ptr().cast(), p2.as_ptr(), envp.as_ptr()) };
        }
        errln(&format!("{name}: {}", strerror(e)));
        if e == libc::ENOENT {
            127
        } else {
            126
        }
    }

    fn exec_pipeline(&mut self, stages: &[Command]) -> i32 {
        let n = stages.len();
        let mut pipes: Vec<(i32, i32)> = Vec::new();
        for _ in 0..n - 1 {
            let mut fds = [0i32; 2];
            // SAFETY: `fds` has room for two descriptors.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                errln(&format!("pipe: {}", strerror(errno())));
                for (r, w) in &pipes {
                    unsafe {
                        libc::close(*r);
                        libc::close(*w);
                    }
                }
                return 1;
            }
            pipes.push((fds[0], fds[1]));
        }
        let all: Vec<i32> = pipes.iter().flat_map(|&(r, w)| [r, w]).collect();
        let mut pids: Vec<i32> = Vec::new();
        let mut pgid = 0;
        let mut failed = false;
        for (i, stage) in stages.iter().enumerate() {
            let stdin = if i > 0 { pipes[i - 1].0 } else { -1 };
            let stdout = if i < n - 1 { pipes[i].1 } else { -1 };
            let pgrp = if !self.interactive {
                Pgrp::Inherit
            } else if i == 0 {
                Pgrp::New
            } else {
                Pgrp::Join(pgid)
            };
            let r = self.fork_run(pgrp, true, stdin, stdout, &all, |sh| match stage {
                Command::Simple(s) => sh.exec_simple(s, true),
                _ => sh.exec(stage),
            });
            match r {
                Ok(pid) => {
                    if i == 0 {
                        pgid = pid;
                    }
                    pids.push(pid);
                }
                Err(e) => {
                    errln(&format!("fork: {e}"));
                    failed = true;
                    break;
                }
            }
        }
        for fd in all {
            // SAFETY: closing the parent's copies of the pipe ends.
            unsafe { libc::close(fd) };
        }
        let desc = stages.iter().map(render).collect::<Vec<_>>().join(" | ");
        let st = self.wait_foreground(&pids, pgid, &desc, None);
        if failed {
            1
        } else {
            st
        }
    }

    fn exec_background(&mut self, cmd: &Command) -> i32 {
        let devnull = if self.interactive { -1 } else { open_file("/dev/null", libc::O_RDONLY).unwrap_or(-1) };
        let pgrp = self.pgrp_new();
        let r = self.fork_run(pgrp, false, devnull, -1, &[], |sh| sh.exec(cmd));
        if devnull > 0 {
            // SAFETY: closing the parent's copy.
            unsafe { libc::close(devnull) };
        }
        match r {
            Ok(pid) => {
                self.last_bg = pid;
                let id = self.jobs.add(pid, vec![pid], render(cmd), JobState::Running);
                if self.interactive {
                    write_fd(2, format!("[{id}] {pid}\n").as_bytes());
                }
                0
            }
            Err(e) => {
                errln(&format!("fork: {e}"));
                1
            }
        }
    }

    /// Wait for a foreground job. Returns the last process's status; if the job is stopped
    /// (Ctrl-Z) it is added to (or kept in) the job table and the status is 128 + signal.
    pub fn wait_foreground(&mut self, pids: &[i32], pgid: i32, desc: &str, job_id: Option<usize>) -> i32 {
        if self.interactive {
            // SAFETY: give the terminal to the job; SIGTTOU is ignored by the shell.
            unsafe { libc::tcsetpgrp(0, pgid) };
        }
        let mut last = 0;
        let mut stopped: Option<(usize, i32)> = None;
        for (idx, &pid) in pids.iter().enumerate() {
            let mut st = 0;
            loop {
                let flags = if self.interactive { libc::WUNTRACED } else { 0 };
                // SAFETY: `st` is a valid out pointer.
                let r = unsafe { libc::waitpid(pid, &mut st, flags) };
                if r == -1 {
                    if errno() == libc::EINTR {
                        continue;
                    }
                    st = 0; // ECHILD: already gone
                }
                break;
            }
            if libc::WIFSTOPPED(st) {
                stopped = Some((idx, libc::WSTOPSIG(st)));
                break;
            }
            if idx == pids.len() - 1 {
                last = decode_status(st);
                if libc::WIFSIGNALED(st) && self.interactive {
                    let sig = libc::WTERMSIG(st);
                    if sig == libc::SIGINT {
                        write_fd(2, b"\n");
                    } else if sig != libc::SIGPIPE {
                        write_fd(2, format!("{}\n", signal_name(sig)).as_bytes());
                    }
                }
            }
        }
        if self.interactive {
            // SAFETY: take the terminal back.
            unsafe { libc::tcsetpgrp(0, self.shell_pgid) };
        }
        if let Some((idx, sig)) = stopped {
            let remaining = pids[idx..].to_vec();
            let id = match job_id {
                Some(id) => {
                    if let Some(j) = self.jobs.get_mut(id) {
                        j.pids = remaining;
                        j.state = JobState::Stopped;
                    }
                    id
                }
                None => self.jobs.add(pgid, remaining, desc.to_string(), JobState::Stopped),
            };
            write_fd(2, format!("\n[{id}]+  Stopped                 {desc}\n").as_bytes());
            return 128 + sig;
        }
        if let Some(id) = job_id {
            self.jobs.remove(id);
        }
        last
    }

    /// Collect finished and stopped background jobs without blocking.
    pub fn reap_jobs(&mut self) {
        loop {
            let mut st = 0;
            // SAFETY: `st` is a valid out pointer.
            let pid = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED) };
            if pid <= 0 {
                break;
            }
            if let Some(job) = self.jobs.job_of_pid(pid) {
                if libc::WIFSTOPPED(st) {
                    job.state = JobState::Stopped;
                } else if libc::WIFCONTINUED(st) {
                    job.state = JobState::Running;
                } else {
                    job.pids.retain(|p| *p != pid);
                    if pid == job.last_pid {
                        job.status = decode_status(st);
                    }
                }
            }
        }
    }

    /// Report finished jobs (interactive) and drop them from the table.
    pub fn notify_jobs(&mut self) {
        let done: Vec<_> = self.jobs.list.iter().filter(|j| j.pids.is_empty()).cloned().collect();
        for j in done {
            if self.interactive {
                let what = match j.status {
                    0 => "Done".to_string(),
                    s if s > 128 => signal_name(s - 128),
                    s => format!("Exit {s}"),
                };
                write_fd(2, format!("[{}]+  {:<22} {}\n", j.id, what, j.cmd).as_bytes());
            }
            self.jobs.remove(j.id);
        }
    }

    // ------------------------------------------------------------ command substitution

    pub fn command_subst(&mut self, src: &str) -> String {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` has room for two descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            errln(&format!("pipe: {}", strerror(errno())));
            return String::new();
        }
        let (r, w) = (fds[0], fds[1]);
        let src = src.to_string();
        let child = self.fork_run(Pgrp::Inherit, false, -1, w, &[r, w], move |sh| sh.run_source(&src));
        // SAFETY: the parent no longer needs the write end.
        unsafe { libc::close(w) };
        let Ok(pid) = child else {
            unsafe { libc::close(r) };
            return String::new();
        };
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: buffer pointer and length are valid.
            let n = unsafe { libc::read(r, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                data.extend_from_slice(&buf[..n as usize]);
            } else if n < 0 && errno() == libc::EINTR {
                continue;
            } else {
                break;
            }
        }
        // SAFETY: closing our read end, then collecting the child.
        unsafe { libc::close(r) };
        let mut st = 0;
        loop {
            let rc = unsafe { libc::waitpid(pid, &mut st, 0) };
            if rc == -1 && errno() == libc::EINTR {
                continue;
            }
            break;
        }
        self.cmdsub_status = Some(decode_status(st));
        while data.last() == Some(&b'\n') {
            data.pop();
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    pub fn exit_code(&self) -> i32 {
        match self.flow {
            Some(Flow::Exit(n)) => n,
            _ => self.last_status,
        }
    }
}
