//! flint-pkg: Flint's package manager.

mod control;
mod db;
mod ops;
mod pack;
mod paths;
mod resolve;
mod tar;
mod version;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use control::{IndexEntry, THIS_ARCH};
use db::{Db, Installed};
use ops::{install_step, remove_pkg, verify, Ctx, Lock};
use resolve::{autoremovable, load_repos, plan_install, plan_remove, plan_upgrade, repo_dirs_from_conf, Repo, Request};
use version::{compare, Constraint};

const USAGE: &str = "usage: flint-pkg [--root DIR] [--repo DIR]... [-n|--dry-run] [-f|--force] COMMAND [ARGS]

  install PKG...      install packages (names, name>=1.0, or .fpk files) with their dependencies
  remove PKG...       remove packages (refuses if others need them, unless --force)
  upgrade [PKG...]    upgrade to the newest repository versions
  autoremove          remove dependencies nothing needs any more
  list                installed packages
  info PKG            details of an installed or available package
  files PKG           files of an installed package
  owns PATH           which package owns a file
  search TERM         search names and descriptions
  verify [PKG...]     compare installed files with their recorded checksums
  repos               show the configured repositories
  build SRC [OUT]     build NAME-VERSION.fpk from SRC/control and SRC/files
  index DIR           write DIR/INDEX for the packages in DIR

--root installs into another directory tree. Repositories come from --repo and from
ROOT/etc/flint/pkg.conf (lines: repo /path/to/dir).";

struct Opts {
    root: PathBuf,
    repos: Vec<PathBuf>,
    force: bool,
    dry_run: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("flint-pkg: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn repo_dirs(o: &Opts) -> Result<Vec<PathBuf>, String> {
    let mut dirs = o.repos.clone();
    let conf = o.root.join("etc/flint/pkg.conf");
    match fs::read_to_string(&conf) {
        Ok(t) => dirs.extend(repo_dirs_from_conf(&t)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", conf.display())),
    }
    Ok(dirs)
}

fn repos(o: &Opts) -> Result<Vec<Repo>, String> {
    load_repos(&repo_dirs(o)?)
}

fn sync() {
    // SAFETY: sync(2) has no preconditions.
    unsafe { libc::sync() };
}

fn describe_step(s: &resolve::Step) -> String {
    match &s.from {
        Some(f) => format!("{} {} (upgrade from {f})", s.control.name, s.control.version),
        None => format!("{} {}", s.control.name, s.control.version),
    }
}

fn run_steps(o: &Opts, plan: resolve::Plan) -> Result<(), String> {
    for n in &plan.notes {
        println!("{n}");
    }
    if plan.steps.is_empty() {
        println!("nothing to do");
        return Ok(());
    }
    println!("will install: {}", plan.steps.iter().map(describe_step).collect::<Vec<_>>().join(", "));
    if o.dry_run {
        println!("dry run: nothing was changed");
        return Ok(());
    }
    let ctx = Ctx { root: o.root.clone(), force: o.force, db: Db::open(&o.root) };
    let mut notes = Vec::new();
    for step in &plan.steps {
        let old = ctx.db.load(&step.control.name)?;
        println!("{} {} {}", if old.is_some() { "upgrading" } else { "installing" }, step.control.name, step.control.version);
        install_step(&ctx, step, old.as_ref(), &mut notes).map_err(|e| format!("while installing {}: {e}", step.control.name))?;
    }
    sync();
    for n in notes {
        println!("note: {n}");
    }
    println!("{} package(s) installed", plan.steps.len());
    Ok(())
}

fn do_remove(o: &Opts, names: &[String], installed: &[Installed]) -> Result<(), String> {
    let order = plan_remove(names, installed, o.force)?;
    if o.dry_run {
        println!("will remove: {}", order.join(", "));
        println!("dry run: nothing was changed");
        return Ok(());
    }
    let ctx = Ctx { root: o.root.clone(), force: o.force, db: Db::open(&o.root) };
    let mut notes = Vec::new();
    for n in &order {
        println!("removing {n}");
        remove_pkg(&ctx, n, &mut notes)?;
    }
    sync();
    for n in notes {
        println!("note: {n}");
    }
    Ok(())
}

fn newest_available(o: &Opts, name: &str) -> Option<IndexEntry> {
    let mut best: Option<IndexEntry> = None;
    for r in repos(o).ok()? {
        for e in r.entries {
            if e.control.name == name && best.as_ref().map_or(true, |b| compare(&e.control.version, &b.control.version).is_gt()) {
                best = Some(e);
            }
        }
    }
    best
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Opts { root: PathBuf::from("/"), repos: Vec::new(), force: false, dry_run: false };
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "--root" => {
                o.root = PathBuf::from(args.get(i + 1).ok_or("--root needs a directory")?);
                i += 1;
            }
            "--repo" => {
                o.repos.push(PathBuf::from(args.get(i + 1).ok_or("--repo needs a directory")?));
                i += 1;
            }
            "-n" | "--dry-run" => o.dry_run = true,
            "-f" | "--force" => o.force = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "--version" => {
                println!("flint-pkg {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
        i += 1;
    }
    let Some(cmd) = args.get(i).map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let rest = &args[i + 1..];
    if let Some(bad) = rest.iter().find(|a| a.starts_with('-') && a.len() > 1) {
        return Err(format!("options go before the command: '{bad}'"));
    }
    let db = Db::open(&o.root);
    let changes = matches!(cmd, "install" | "upgrade" | "remove" | "del" | "autoremove");
    if changes && !o.dry_run && !o.root.is_dir() {
        return Err(format!("install root {} is not a directory", o.root.display()));
    }
    let _lock = if changes && !o.dry_run { Some(Lock::acquire(&o.root)?) } else { None };

    match cmd {
        "build" => {
            let src = rest.first().ok_or("usage: flint-pkg build SRC [OUT]")?;
            let out = rest.get(1).map_or(Path::new("."), |s| Path::new(s.as_str()));
            let (path, sha, size) = pack::build(Path::new(src), out)?;
            println!("built {} ({size} bytes, sha256 {sha})", path.display());
        }
        "index" => {
            let dir = rest.first().ok_or("usage: flint-pkg index DIR")?;
            let n = pack::write_index(Path::new(dir))?;
            println!("wrote {}/INDEX with {n} package(s)", dir.trim_end_matches('/'));
        }
        "install" => {
            if rest.is_empty() {
                return Err("usage: flint-pkg install PKG...".into());
            }
            let mut reqs = Vec::new();
            for a in rest {
                if a.ends_with(".fpk") || (a.contains('/') && Path::new(a).is_file()) {
                    reqs.push(Request::File(PathBuf::from(a), pack::read_control(Path::new(a))?));
                } else {
                    reqs.push(Request::Name(Constraint::parse(a)?));
                }
            }
            let installed = db.all()?;
            let need_repos = reqs.iter().any(|r| matches!(r, Request::Name(_))) || reqs.iter().any(|r| matches!(r, Request::File(_, c) if !c.depends.is_empty()));
            let rs = if need_repos { repos(&o)? } else { Vec::new() };
            let plan = plan_install(&reqs, &installed, &rs)?;
            run_steps(&o, plan)?;
        }
        "upgrade" => {
            let installed = db.all()?;
            let plan = plan_upgrade(rest, &installed, &repos(&o)?)?;
            if plan.steps.is_empty() {
                println!("everything is up to date");
            } else {
                run_steps(&o, plan)?;
            }
        }
        "remove" | "del" => {
            if rest.is_empty() {
                return Err("usage: flint-pkg remove PKG...".into());
            }
            do_remove(&o, rest, &db.all()?)?;
        }
        "autoremove" => {
            let installed = db.all()?;
            let orphans = autoremovable(&installed);
            if orphans.is_empty() {
                println!("nothing to remove");
            } else {
                do_remove(&o, &orphans, &installed)?;
            }
        }
        "list" => {
            for p in db.all()? {
                println!("{} {}{}", p.control.name, p.control.version, if p.explicit { "" } else { " (dependency)" });
            }
        }
        "files" => {
            let name = rest.first().ok_or("usage: flint-pkg files PKG")?;
            let p = db.load(name)?.ok_or_else(|| format!("{name} is not installed"))?;
            for f in &p.files {
                println!("/{}", f.path);
            }
        }
        "owns" => {
            let path = rest.first().ok_or("usage: flint-pkg owns PATH")?;
            let rel = paths::normalize_rel(path.trim_start_matches('/'))?;
            for p in db.all()? {
                if p.files.iter().any(|f| f.path == rel && f.kind != 'd') {
                    println!("/{rel} is owned by {} {}", p.control.name, p.control.version);
                    return Ok(ExitCode::SUCCESS);
                }
            }
            println!("/{rel} is not owned by any package");
            return Ok(ExitCode::FAILURE);
        }
        "info" => {
            let name = rest.first().ok_or("usage: flint-pkg info PKG")?;
            let installed = db.load(name)?;
            let available = newest_available(&o, name);
            let Some(c) = installed.as_ref().map(|i| &i.control).or(available.as_ref().map(|a| &a.control)) else {
                return Err(format!("package '{name}' not found"));
            };
            println!("name: {}\nversion: {}\ndescription: {}\narch: {}", c.name, c.version, c.description, c.arch);
            if !c.depends.is_empty() {
                println!("depends: {}", c.depends.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "));
            }
            if !c.conflicts.is_empty() {
                println!("conflicts: {}", c.conflicts.join(", "));
            }
            match &installed {
                Some(i) => println!("status: installed ({}, {} files)", if i.explicit { "explicit" } else { "dependency" }, i.files.iter().filter(|f| f.kind != 'd').count()),
                None => println!("status: not installed"),
            }
            if let (Some(i), Some(a)) = (&installed, &available) {
                if compare(&a.control.version, &i.control.version).is_gt() {
                    println!("available: {}", a.control.version);
                }
            }
        }
        "search" => {
            let term = rest.first().ok_or("usage: flint-pkg search TERM")?.to_lowercase();
            let installed = db.all()?;
            let mut all: Vec<control::Control> = repos(&o).map(|rs| rs.into_iter().flat_map(|r| r.entries.into_iter().map(|e| e.control)).collect()).unwrap_or_default();
            all.extend(installed.iter().map(|i| i.control.clone()));
            all.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| compare(&a.version, &b.version)));
            all.dedup_by(|a, b| a.name == b.name && a.version == b.version);
            let mut found = false;
            for c in all {
                if c.name.contains(&term) || c.description.to_lowercase().contains(&term) {
                    let mark = if installed.iter().any(|i| i.control.name == c.name && i.control.version == c.version) { " [installed]" } else { "" };
                    println!("{} {} - {}{mark}", c.name, c.version, c.description);
                    found = true;
                }
            }
            if !found {
                return Ok(ExitCode::FAILURE);
            }
        }
        "verify" => {
            for n in rest {
                if db.load(n)?.is_none() {
                    return Err(format!("{n} is not installed"));
                }
            }
            let ctx = Ctx { root: o.root.clone(), force: false, db: Db::open(&o.root) };
            let (mut bad, mut checked) = (0, 0);
            for p in db.all()? {
                if !rest.is_empty() && !rest.contains(&p.control.name) {
                    continue;
                }
                checked += 1;
                for problem in verify(&ctx, &p) {
                    println!("{}: {problem}", p.control.name);
                    bad += 1;
                }
            }
            if bad == 0 {
                println!("verified {checked} package(s): all files match");
            } else {
                println!("{bad} problem(s) found");
                return Ok(ExitCode::FAILURE);
            }
        }
        "repos" => {
            for d in repo_dirs(&o)? {
                println!("{}", d.display());
            }
        }
        "help" => println!("{USAGE}\n\nthis system: {THIS_ARCH}"),
        other => return Err(format!("unknown command '{other}'\n{USAGE}")),
    }
    Ok(ExitCode::SUCCESS)
}
