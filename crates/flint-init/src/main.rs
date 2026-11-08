//! flint-init: PID 1 for Flint Linux.
//!
//! Boot: mount pseudo filesystems, run /etc/flint/boot tasks, then supervise
//! the services in /etc/flint/services. Shutdown is requested with signals
//! (see `flint_core::PowerAction`; `flint-power` sends them).

mod boot;
mod config;
mod shutdown;
mod spawn;
mod supervisor;

use std::env;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use flint_core::{flint_err, flint_info, flint_warn, log, version, PowerAction};
use supervisor::Supervisor;

const CONFIG_DIR: &str = "/etc/flint";
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn main() -> ExitCode {
    if std::process::id() == 1 {
        // The kernel passes unknown command-line words to init as arguments: ignore them all.
        run();
    }
    cli()
}

/// Everything except running as init is a harmless diagnostic.
fn cli() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-V" | "--version") => {
            println!("{}", version::long());
            ExitCode::SUCCESS
        }
        Some("--check") => check(args.get(1).map_or(CONFIG_DIR, String::as_str)),
        _ => {
            eprintln!(
                "flint-init is the system init and only runs as PID 1.\n\
                 usage: flint-init --version | --check [CONFIG_DIR]"
            );
            ExitCode::from(2)
        }
    }
}

/// Validate the config files without booting.
fn check(dir: &str) -> ExitCode {
    let dir_path = Path::new(dir);
    let mut ok = true;
    match config::load_services(dir_path) {
        Ok(Some(s)) => println!("{dir}/services: ok, {} service(s)", s.len()),
        Ok(None) => println!("{dir}/services: not present, the built-in default would be used"),
        Err(e) => {
            eprintln!("{e}");
            ok = false;
        }
    }
    match config::load_tasks(dir_path) {
        Ok(Some(t)) => println!("{dir}/boot: ok, {} task(s)", t.len()),
        Ok(None) => println!("{dir}/boot: not present, no boot tasks"),
        Err(e) => {
            eprintln!("{e}");
            ok = false;
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run() -> ! {
    log::init("flint-init");
    env::set_var("PATH", PATH);

    // Block first so no signal is lost while booting.
    let signals = match supervisor::block_signals() {
        Ok(set) => Some(set),
        Err(e) => {
            flint_err!("cannot block signals, shutdown requests will not work: {e}");
            None
        }
    };

    boot::early();

    let dir = Path::new(CONFIG_DIR);
    match config::load_tasks(dir) {
        Ok(Some(tasks)) => boot::run_tasks(&tasks),
        Ok(None) => {}
        Err(e) => flint_err!("{e}; skipping boot tasks"),
    }
    let services = match config::load_services(dir) {
        Ok(Some(s)) => s,
        Ok(None) => {
            flint_warn!("{CONFIG_DIR}/services not found, using the built-in default");
            config::default_services()
        }
        Err(e) => {
            flint_err!("{e}; using the built-in default");
            config::default_services()
        }
    };

    let mut sup = Supervisor::new(services);
    loop {
        // PID 1 must survive its own bugs: a panic in one iteration is logged, not fatal.
        match panic::catch_unwind(AssertUnwindSafe(|| step(&mut sup, signals.as_ref()))) {
            Ok(Some(action)) => shutdown::run(action),
            Ok(None) => {}
            Err(_) => {
                flint_err!("internal error in the supervisor loop, continuing");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// One turn of the main loop. Returns the requested power action, if any.
fn step(sup: &mut Supervisor, signals: Option<&libc::sigset_t>) -> Option<PowerAction> {
    sup.start_due();
    let sig = supervisor::wait_signal(signals, sup.timeout());
    sup.reap(); // SIGCHLD coalesces, so always reap, whatever woke us
    let sig = sig?;
    if let Some(action) = PowerAction::from_signal(sig) {
        flint_info!("signal {sig}: {}", action.name());
        return Some(action);
    }
    if sig == libc::SIGHUP {
        flint_info!("SIGHUP ignored (reloading the configuration is not supported yet)");
    }
    None
}
