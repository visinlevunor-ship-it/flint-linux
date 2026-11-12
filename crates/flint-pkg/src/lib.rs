//! pkg: Flint's package manager.

mod control;
mod db;
mod kernels;
mod ops;
mod pack;
mod paths;
mod remote;
mod resolve;
mod tar;
mod version;

use std::fs;
use std::path::{Path, PathBuf};

use control::{IndexEntry, THIS_ARCH};
use db::{Db, Installed};
use ops::{install_step, remove_pkg, verify, Ctx, Lock};
use remote::RepoSpec;
use resolve::{autoremovable, load_repos, plan_install, plan_remove, plan_upgrade, Repo, Request, Source};
use version::{compare, Constraint};

const USAGE: &str = "usage: pkg [--root DIR] [--repo DIR-OR-URL]... [-n|--dry-run] [-f|--force] COMMAND [ARGS]

  update              download and verify the package lists of remote repositories
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
  kernels             list kernels; kernels install|remove|default FLAVOR (zen, lts, ...)
  key new NAME [DIR]  make a signing key pair: DIR/NAME.key (secret) and DIR/NAME.pub
  sign DIR KEYFILE    sign DIR/INDEX, writing DIR/INDEX.sig

--root installs into another directory tree. Repositories come from --repo and from
ROOT/etc/flint/pkg.conf (lines: repo /path/to/dir, or repo https://host/path).
A remote repository's INDEX must be signed by a key in ROOT/etc/flint/keys/*.pub;
packages are checked against the SHA-256 in that signed INDEX.";

#[derive(Clone)]
struct Opts {
    root: PathBuf,
    repos: Vec<RepoSpec>,
    force: bool,
    dry_run: bool,
}

/// Entry point; `args[0]` is the name the program was started as.
pub fn main(args: Vec<String>) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("pkg: {msg}");
            1
        }
    }
}

fn repo_specs(o: &Opts) -> Result<Vec<RepoSpec>, String> {
    let mut specs = o.repos.clone();
    let conf = o.root.join("etc/flint/pkg.conf");
    match fs::read_to_string(&conf) {
        Ok(t) => specs.extend(remote::parse_conf(&t)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", conf.display())),
    }
    Ok(specs)
}

fn remotes(o: &Opts) -> Result<Vec<String>, String> {
    Ok(repo_specs(o)?.into_iter().filter_map(|s| if let RepoSpec::Remote(u) = s { Some(u) } else { None }).collect())
}

fn update_one(o: &Opts, url: &str) -> Result<(), String> {
    let u = remote::update(&o.root, url)?;
    println!("{url}: {} package(s), signed by {}{}", u.packages, u.key, if u.changed { "" } else { " (unchanged)" });
    Ok(())
}

/// Refresh remote package lists: all of them when `all` (before installing or upgrading,
/// so the newest versions are seen), otherwise only the ones never fetched. A refresh that
/// fails is a warning: the last verified list is used, or the repository is skipped.
fn refresh(o: &Opts, all: bool) -> Result<(), String> {
    for url in remotes(o)? {
        let have = remote::cache_dir(&o.root, &url).join("INDEX").is_file();
        if have && !all {
            continue;
        }
        if let Err(e) = update_one(o, &url) {
            if have {
                eprintln!("pkg: warning: {e}; using the package list from the last update");
            } else {
                eprintln!("pkg: warning: {e}; skipping this repository");
            }
        }
    }
    Ok(())
}

fn repos(o: &Opts) -> Result<Vec<Repo>, String> {
    load_repos(&o.root, &repo_specs(o)?)
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
    // Download everything first, so a broken connection never leaves a half-done upgrade.
    for step in &plan.steps {
        if let Source::Repo { path, url: Some(url), size, sha256 } = &step.source {
            remote::fetch_package(url, path, *size, sha256)?;
        }
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
    let _ = refresh(o, false);
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

fn run(argv: Vec<String>) -> Result<i32, String> {
    let args: Vec<String> = argv.into_iter().skip(1).collect();
    let mut o = Opts { root: PathBuf::from("/"), repos: Vec::new(), force: false, dry_run: false };
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "--root" => {
                o.root = PathBuf::from(args.get(i + 1).ok_or("--root needs a directory")?);
                i += 1;
            }
            "--repo" => {
                o.repos.push(remote::spec(args.get(i + 1).ok_or("--repo needs a directory or URL")?)?);
                i += 1;
            }
            "-n" | "--dry-run" => o.dry_run = true,
            "-f" | "--force" => o.force = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(0);
            }
            "--version" => {
                println!("pkg ({})", flint_core::version::Long);
                return Ok(0);
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
        i += 1;
    }
    let Some(cmd) = args.get(i).map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[i + 1..];
    if let Some(bad) = rest.iter().find(|a| a.starts_with('-') && a.len() > 1) {
        return Err(format!("options go before the command: '{bad}'"));
    }
    let db = Db::open(&o.root);
    // "kernels" only reads, or calls run() again for the change it makes.
    let changes = matches!(cmd, "install" | "upgrade" | "remove" | "del" | "autoremove" | "update");
    let touches_packages = matches!(cmd, "install" | "upgrade" | "remove" | "del" | "autoremove");
    if changes && !o.dry_run && !o.root.is_dir() {
        return Err(format!("install root {} is not a directory", o.root.display()));
    }
    let lock = if changes && !o.dry_run { Some(Lock::acquire(&o.root)?) } else { None };
    let status = command(&o, cmd, rest, &db)?;
    drop(lock);
    if touches_packages && !o.dry_run {
        // Kernel packages also live on the boot partition, with a boot menu entry each.
        kernels::sync(&o.root)?;
    }
    Ok(status)
}

fn kernels_cmd(o: &Opts, rest: &[String], db: &Db) -> Result<i32, String> {
    let again = |args: &[&str]| -> Result<i32, String> {
        let mut argv = vec!["pkg".to_string(), "--root".to_string(), o.root.display().to_string()];
        for r in &o.repos {
            argv.push("--repo".into());
            argv.push(r.to_string());
        }
        if o.dry_run {
            argv.push("-n".into());
        }
        argv.extend(args.iter().map(|s| s.to_string()));
        run(argv)
    };
    match (rest.first().map(String::as_str), rest.get(1)) {
        (None | Some("list"), None) => {
            let _ = refresh(o, false);
            let installed = kernels::installed(&o.root);
            let running = kernels::running_release();
            let mark = |f: &str| {
                let mut m = Vec::new();
                if kernels::chosen_default(&o.root) == f {
                    m.push("default");
                }
                let rel = if f == kernels::BUILTIN { None } else { kernels::release(&o.root, f) };
                if rel.as_deref() == Some(running.as_str()) || (f == kernels::BUILTIN && !installed.iter().any(|i| kernels::release(&o.root, i).as_deref() == Some(running.as_str()))) {
                    m.push("running");
                }
                if m.is_empty() { String::new() } else { format!(" ({})", m.join(", ")) }
            };
            println!("installed:");
            println!("  {:<10} the kernel Flint came with{}", kernels::BUILTIN, mark(kernels::BUILTIN));
            for f in &installed {
                println!("  {:<10} {}{}", f, kernels::release(&o.root, f).unwrap_or_default(), mark(f));
            }
            let mut available: Vec<(String, String, String)> = Vec::new();
            for r in repos(o).unwrap_or_default() {
                for e in r.entries {
                    if let Some(f) = kernels::flavor_of(&e.control.name) {
                        if !installed.iter().any(|i| i == f) && !available.iter().any(|(a, _, _)| a == f) {
                            available.push((f.to_string(), e.control.version.clone(), e.control.description.clone()));
                        }
                    }
                }
            }
            available.sort();
            if !available.is_empty() {
                println!("available (pkg kernels install NAME):");
                for (f, v, d) in available {
                    println!("  {f:<10} {v:<14} {d}");
                }
            }
            Ok(0)
        }
        (Some("install"), Some(f)) => {
            let status = again(&["install", &kernels::package_name(f)])?;
            if status == 0 && !o.dry_run {
                println!("The boot menu now has \"Flint Linux ({f} kernel)\". To start it by default: pkg kernels default {f}");
            }
            Ok(status)
        }
        (Some("remove"), Some(f)) => {
            if f == kernels::BUILTIN {
                return Err("the generic kernel came with Flint and stays".into());
            }
            if db.load(&kernels::package_name(f))?.is_none() {
                return Err(format!("the {f} kernel is not installed"));
            }
            again(&["remove", &kernels::package_name(f)])
        }
        (Some("default"), Some(f)) => {
            if f != kernels::BUILTIN && !kernels::installed(&o.root).contains(f) {
                return Err(format!("the {f} kernel is not installed (pkg kernels install {f})"));
            }
            kernels::set_default(&o.root, f).map_err(|e| e.to_string())?;
            kernels::sync(&o.root)?;
            println!("{f} starts by default from the next boot");
            Ok(0)
        }
        _ => Err("usage: pkg kernels [list]
       pkg kernels install|remove|default FLAVOR".into()),
    }
}

fn command(o: &Opts, cmd: &str, rest: &[String], db: &Db) -> Result<i32, String> {
    let o = o.clone();

    match cmd {
        "build" => {
            let src = rest.first().ok_or("usage: pkg build SRC [OUT]")?;
            let out = rest.get(1).map_or(Path::new("."), |s| Path::new(s.as_str()));
            let (path, sha, size) = pack::build(Path::new(src), out)?;
            println!("built {} ({size} bytes, sha256 {sha})", path.display());
        }
        "index" => {
            let dir = rest.first().ok_or("usage: pkg index DIR")?;
            let n = pack::write_index(Path::new(dir))?;
            println!("wrote {}/INDEX with {n} package(s)", dir.trim_end_matches('/'));
        }
        "install" => {
            if rest.is_empty() {
                return Err("usage: pkg install PKG...".into());
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
            let rs = if need_repos {
                refresh(&o, true)?;
                repos(&o)?
            } else {
                Vec::new()
            };
            let plan = plan_install(&reqs, &installed, &rs)?;
            run_steps(&o, plan)?;
        }
        "update" => {
            let urls = remotes(&o)?;
            if urls.is_empty() {
                println!("no remote repositories configured (add 'repo https://...' to {}/etc/flint/pkg.conf)", o.root.display().to_string().trim_end_matches('/'));
            }
            let mut failed = 0;
            for url in urls {
                if let Err(e) = update_one(&o, &url) {
                    eprintln!("pkg: {e}");
                    failed += 1;
                }
            }
            if failed > 0 {
                return Ok(1);
            }
        }
        "key" => match rest.first().map(String::as_str) {
            Some("new") => {
                let name = rest.get(1).ok_or("usage: pkg key new NAME [DIR]")?;
                let dir = rest.get(2).map_or(Path::new("."), |s| Path::new(s.as_str()));
                let (sk, pk) = remote::new_key(dir, name)?;
                println!("secret key: {} (keep it private, never put it in a repository)", sk.display());
                println!("public key: {} (copy it to /etc/flint/keys/ on systems that trust this repository)", pk.display());
            }
            _ => return Err("usage: pkg key new NAME [DIR]".into()),
        },
        "sign" => {
            let (Some(dir), Some(keyfile)) = (rest.first(), rest.get(1)) else {
                return Err("usage: pkg sign DIR KEYFILE".into());
            };
            let dir = Path::new(dir);
            let index = fs::read(dir.join("INDEX")).map_err(|e| format!("{}/INDEX: {e} (make it with: pkg index {})", dir.display(), dir.display()))?;
            let n = control::parse_index(std::str::from_utf8(&index).map_err(|_| "INDEX is not text")?)?.len();
            let seed = remote::parse_secret(&fs::read_to_string(keyfile).map_err(|e| format!("{keyfile}: {e}"))?)?;
            let sig = remote::sign(&index, &seed);
            fs::write(dir.join("INDEX.sig"), &sig).map_err(|e| format!("{}/INDEX.sig: {e}", dir.display()))?;
            println!("signed {}/INDEX ({n} package(s)) with key {}", dir.display(), &sig.split_whitespace().nth(2).unwrap_or("")[..16]);
        }
        "upgrade" => {
            refresh(&o, true)?;
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
                return Err("usage: pkg remove PKG...".into());
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
            let name = rest.first().ok_or("usage: pkg files PKG")?;
            let p = db.load(name)?.ok_or_else(|| format!("{name} is not installed"))?;
            for f in &p.files {
                println!("/{}", f.path);
            }
        }
        "owns" => {
            let path = rest.first().ok_or("usage: pkg owns PATH")?;
            let rel = paths::normalize_rel(path.trim_start_matches('/'))?;
            for p in db.all()? {
                if p.files.iter().any(|f| f.path == rel && f.kind != 'd') {
                    println!("/{rel} is owned by {} {}", p.control.name, p.control.version);
                    return Ok(0);
                }
            }
            println!("/{rel} is not owned by any package");
            return Ok(1);
        }
        "info" => {
            let name = rest.first().ok_or("usage: pkg info PKG")?;
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
            let term = rest.first().ok_or("usage: pkg search TERM")?.to_lowercase();
            let installed = db.all()?;
            refresh(&o, false)?;
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
                return Ok(1);
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
                return Ok(1);
            }
        }
        "kernels" | "kernel" => return kernels_cmd(&o, rest, db),
        "repos" => {
            for spec in repo_specs(&o)? {
                match &spec {
                    RepoSpec::Local(_) => println!("{spec}"),
                    RepoSpec::Remote(url) => match remote::cached_index(&o.root, url) {
                        Ok(Some(t)) => println!("{spec} ({} package(s) at the last update)", control::parse_index(&t).map_or(0, |e| e.len())),
                        Ok(None) => println!("{spec} (not fetched yet: pkg update)"),
                        Err(e) => println!("{spec} ({e})"),
                    },
                }
            }
            let keys = remote::trusted_keys(&o.root);
            if keys.is_empty() && !remotes(&o)?.is_empty() {
                println!("trusted keys: none");
            }
            for k in keys {
                println!("trusted key: {} {}", k.name, flint_crypto::hex(&k.public));
            }
        }
        "help" => println!("{USAGE}\n\nthis system: {THIS_ARCH}"),
        other => return Err(format!("unknown command '{other}'\n{USAGE}")),
    }
    Ok(0)
}
