//! `poweroff`, `reboot` and `halt`.
//!
//! Invoked as `power <poweroff|reboot|halt>` or through symlinks named
//! `poweroff`, `reboot` and `halt`. Normally it asks init (PID 1) to shut the
//! system down cleanly. With `-f` it syncs and calls reboot(2) directly.

use std::path::Path;

use flint_core::{sys, PowerAction};

const USAGE: &str = r"usage: poweroff|reboot|halt [-f]   (or: power [-f] <poweroff|reboot|halt>)
  (or run through a symlink named poweroff, reboot or halt)
  -f, --force   sync and tell the kernel directly, without involving init
";

#[derive(Debug, PartialEq, Eq)]
struct Request {
    action: PowerAction,
    force: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Request),
    Help,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let prog = args
        .first()
        .and_then(|a| Path::new(a).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("power");
    let mut action = PowerAction::from_name(prog);
    let mut force = false;

    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-f" | "--force" => force = true,
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => match PowerAction::from_name(s) {
                // An explicit subcommand only makes sense when argv[0] didn't already pick one.
                Some(a) if action.is_none() => action = Some(a),
                Some(_) => return Err(format!("unexpected argument '{s}'")),
                None => return Err(format!("unknown action '{s}'")),
            },
        }
    }

    match action {
        Some(action) => Ok(Parsed::Run(Request { action, force })),
        None => Err("no action given".to_string()),
    }
}

fn prog_name(args: &[String]) -> String {
    args.first().and_then(|a| Path::new(a).file_name()).and_then(|n| n.to_str()).unwrap_or("power").to_string()
}

fn run(req: Request) -> Result<(), String> {
    if req.force {
        sys::sync();
        let err = req.action.execute();
        return Err(format!("{} failed: {err}", req.action.name()));
    }
    // SAFETY: kill(2) with PID 1 and a valid signal number.
    if unsafe { libc::kill(1, req.action.signal()) } != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("cannot signal init: {err}"));
    }
    Ok(())
}

/// Entry point; `args[0]` (poweroff, reboot or halt) picks the action.
pub fn main(args: Vec<String>) -> i32 {
    match parse(&args) {
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            0
        }
        Ok(Parsed::Run(req)) => match run(req) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{}: {e}", prog_name(&args));
                1
            }
        },
        Err(e) => {
            eprintln!("{}: {e}\n{USAGE}", prog_name(&args));
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn run_of(action: PowerAction, force: bool) -> Result<Parsed, String> {
        Ok(Parsed::Run(Request { action, force }))
    }

    #[test]
    fn subcommand() {
        assert_eq!(parse(&v(&["power", "poweroff"])), run_of(PowerAction::PowerOff, false));
        assert_eq!(parse(&v(&["power", "reboot"])), run_of(PowerAction::Reboot, false));
        assert_eq!(parse(&v(&["power", "halt"])), run_of(PowerAction::Halt, false));
    }

    #[test]
    fn symlink_name() {
        assert_eq!(parse(&v(&["poweroff"])), run_of(PowerAction::PowerOff, false));
        assert_eq!(parse(&v(&["/sbin/reboot", "-f"])), run_of(PowerAction::Reboot, true));
        assert_eq!(parse(&v(&["/sbin/halt", "--force"])), run_of(PowerAction::Halt, true));
    }

    #[test]
    fn force_before_or_after() {
        assert_eq!(parse(&v(&["power", "-f", "reboot"])), run_of(PowerAction::Reboot, true));
        assert_eq!(parse(&v(&["power", "reboot", "-f"])), run_of(PowerAction::Reboot, true));
    }

    #[test]
    fn errors() {
        assert!(parse(&v(&["power"])).is_err());
        assert!(parse(&v(&["power", "bogus"])).is_err());
        assert!(parse(&v(&["power", "--nope", "reboot"])).is_err());
        // argv[0] already chose an action; a second one is ambiguous
        assert!(parse(&v(&["reboot", "halt"])).is_err());
    }

    #[test]
    fn help() {
        assert_eq!(parse(&v(&["power", "-h"])), Ok(Parsed::Help));
    }
}
