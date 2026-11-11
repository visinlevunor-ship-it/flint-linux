//! End-to-end tests: build packages, publish a repository, and install, upgrade, remove and
//! verify into throwaway roots by running the real pkg binary.

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_pkg");

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

impl Out {
    fn ok(&self) -> &Self {
        assert_eq!(self.code, 0, "stdout: {}\nstderr: {}", self.stdout, self.stderr);
        self
    }
    fn fails(&self, needle: &str) -> &Self {
        assert_ne!(self.code, 0, "expected failure; stdout: {}", self.stdout);
        assert!(self.stderr.contains(needle) || self.stdout.contains(needle), "wanted {needle:?} in\nstdout: {}\nstderr: {}", self.stdout, self.stderr);
        self
    }
}

fn run(args: &[&str]) -> Out {
    let o = Command::new(BIN).args(args).env("SOURCE_DATE_EPOCH", "0").output().unwrap();
    Out { stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned(), code: o.status.code().unwrap_or(-1) }
}

/// A scratch area with `repo/`, `root/` and `src/` directories.
struct Env {
    base: PathBuf,
}

impl Env {
    fn new() -> Env {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!("flint-pkg-it-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&base);
        for d in ["repo", "root", "src"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        Env { base: fs::canonicalize(&base).unwrap() }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }
    fn s(&self, rel: &str) -> String {
        self.p(rel).to_string_lossy().into_owned()
    }
    fn root(&self, rel: &str) -> PathBuf {
        self.p("root").join(rel)
    }

    /// Stage a package source: control text plus (path, content, mode) files.
    fn package(&self, name: &str, control: &str, files: &[(&str, &str, u32)]) -> String {
        let src = self.p(&format!("src/{name}"));
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(src.join("files")).unwrap();
        fs::write(src.join("control"), control).unwrap();
        for (path, content, mode) in files {
            let f = src.join("files").join(path);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, content).unwrap();
            fs::set_permissions(&f, fs::Permissions::from_mode(*mode)).unwrap();
        }
        src.to_string_lossy().into_owned()
    }

    fn publish(&self, name: &str, control: &str, files: &[(&str, &str, u32)]) {
        let src = self.package(name, control, files);
        run(&["build", &src, &self.s("repo")]).ok();
        run(&["index", &self.s("repo")]).ok();
    }

    /// pkg --root ROOT --repo REPO ARGS...
    fn pkg(&self, args: &[&str]) -> Out {
        let (root, repo) = (self.s("root"), self.s("repo"));
        let mut all = vec!["--root", root.as_str(), "--repo", repo.as_str()];
        all.extend_from_slice(args);
        run(&all)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn install_with_dependencies_list_files_owns_info_search() {
    let e = Env::new();
    e.publish("lib", "name: libgreet\nversion: 1.0\ndescription: Greeting library\n", &[("usr/lib/libgreet.txt", "hello from lib\n", 0o644)]);
    e.publish("app", "name: hello\nversion: 1.0\ndescription: Says hello\ndepends: libgreet>=1.0\n", &[("usr/bin/hello", "#!/bin/sh\necho hi\n", 0o755)]);

    let o = e.pkg(&["install", "hello"]);
    o.ok();
    assert!(o.stdout.contains("will install: libgreet 1.0, hello 1.0"), "dependency first: {}", o.stdout);
    assert_eq!(read(&e.root("usr/bin/hello")), "#!/bin/sh\necho hi\n");
    assert_eq!(fs::metadata(e.root("usr/bin/hello")).unwrap().mode() & 0o7777, 0o755);
    assert_eq!(read(&e.root("usr/lib/libgreet.txt")), "hello from lib\n");

    assert_eq!(e.pkg(&["list"]).ok().stdout, "hello 1.0\nlibgreet 1.0 (dependency)\n");
    assert_eq!(e.pkg(&["files", "hello"]).ok().stdout, "/usr\n/usr/bin\n/usr/bin/hello\n");
    assert!(e.pkg(&["owns", "/usr/bin/hello"]).ok().stdout.contains("owned by hello 1.0"));
    e.pkg(&["owns", "/usr/bin/nothing"]).fails("not owned");
    let info = e.pkg(&["info", "hello"]);
    assert!(info.ok().stdout.contains("depends: libgreet>=1.0") && info.stdout.contains("status: installed (explicit, 1 files)"), "{}", info.stdout);
    assert!(e.pkg(&["search", "greet"]).ok().stdout.contains("libgreet 1.0 - Greeting library [installed]"));
    e.pkg(&["search", "zzzz"]).fails("");

    let again = e.pkg(&["install", "hello"]);
    assert!(again.ok().stdout.contains("hello 1.0 is already installed"));
    assert!(e.pkg(&["verify"]).ok().stdout.contains("verified 2 package(s): all files match"));
}

#[test]
fn dry_run_changes_nothing() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\n", &[("opt/a", "a\n", 0o644)]);
    let o = e.pkg(&["-n", "install", "a"]);
    assert!(o.ok().stdout.contains("dry run"));
    assert!(!e.root("opt/a").exists());
    assert!(!e.root("var/lib/pkg").exists());
}

#[test]
fn remove_respects_dependents_and_autoremove_cleans_up() {
    let e = Env::new();
    e.publish("lib", "name: lib\nversion: 1\n", &[("usr/share/lib/data", "x\n", 0o644)]);
    e.publish("app", "name: app\nversion: 1\ndepends: lib\n", &[("usr/bin/app", "app\n", 0o755)]);
    e.publish("other", "name: other\nversion: 1\n", &[("usr/bin/other", "o\n", 0o755)]);
    e.pkg(&["install", "app", "other"]).ok();

    e.pkg(&["remove", "lib"]).fails("required by app");
    e.pkg(&["remove", "app"]).ok();
    assert!(!e.root("usr/bin/app").exists());
    assert!(e.root("usr/bin/other").exists(), "files of other packages stay");
    assert!(e.root("usr/bin").is_dir(), "a directory another package lists stays");
    assert!(e.root("usr/share/lib/data").exists(), "the dependency is still there");

    let o = e.pkg(&["autoremove"]);
    assert!(o.ok().stdout.contains("removing lib"), "{}", o.stdout);
    assert!(!e.root("usr/share/lib").exists(), "its now-empty directory is gone");
    assert!(!e.root("usr/share").exists());
    assert_eq!(e.pkg(&["list"]).ok().stdout, "other 1\n");
    assert!(e.pkg(&["autoremove"]).ok().stdout.contains("nothing to remove"));
    e.pkg(&["remove", "ghost"]).fails("not installed");
}

#[test]
fn upgrade_replaces_files_and_keeps_edited_config() {
    let e = Env::new();
    e.publish("v1", "name: tool\nversion: 1.0\nconfig: etc/tool.conf\n", &[
        ("usr/bin/tool", "v1\n", 0o755),
        ("usr/share/tool/old-only", "gone in v2\n", 0o644),
        ("etc/tool.conf", "setting=1\n", 0o644),
        ("etc/untouched.conf", "a\n", 0o644),
    ]);
    e.pkg(&["install", "tool"]).ok();
    fs::write(e.root("etc/tool.conf"), "setting=MINE\n").unwrap();

    assert!(e.pkg(&["upgrade"]).ok().stdout.contains("everything is up to date"));
    e.publish("v2", "name: tool\nversion: 1.10\nconfig: etc/tool.conf\n", &[
        ("usr/bin/tool", "v2\n", 0o755),
        ("etc/tool.conf", "setting=2\n", 0o644),
        ("etc/untouched.conf", "a\n", 0o644),
    ]);
    let info = e.pkg(&["info", "tool"]);
    assert!(info.ok().stdout.contains("available: 1.10"), "{}", info.stdout);

    let o = e.pkg(&["upgrade"]);
    o.ok();
    assert!(o.stdout.contains("tool 1.10 (upgrade from 1.0)"), "{}", o.stdout);
    assert!(o.stdout.contains("/etc/tool.conf was modified; the new version was saved as /etc/tool.conf.flint-new"), "{}", o.stdout);
    assert_eq!(read(&e.root("usr/bin/tool")), "v2\n");
    assert_eq!(read(&e.root("etc/tool.conf")), "setting=MINE\n", "the user's edit is kept");
    assert_eq!(read(&e.root("etc/tool.conf.flint-new")), "setting=2\n");
    assert!(!e.root("usr/share/tool/old-only").exists(), "files dropped by the new version are removed");
    assert!(!e.root("usr/share/tool").exists());
    assert_eq!(e.pkg(&["list"]).ok().stdout, "tool 1.10\n");

    // removing keeps the edited config, deletes the rest
    let r = e.pkg(&["remove", "tool"]);
    assert!(r.ok().stdout.contains("kept modified config file /etc/tool.conf"));
    assert!(e.root("etc/tool.conf").exists() && !e.root("etc/untouched.conf").exists() && !e.root("usr/bin/tool").exists());
}

#[test]
fn verify_reports_damage() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\n", &[("usr/bin/a", "a\n", 0o755), ("usr/share/a/x", "x\n", 0o644)]);
    e.pkg(&["install", "a"]).ok();
    fs::write(e.root("usr/bin/a"), "tampered\n").unwrap();
    fs::remove_file(e.root("usr/share/a/x")).unwrap();
    let o = e.pkg(&["verify", "a"]);
    o.fails("2 problem(s) found");
    assert!(o.stdout.contains("modified: /usr/bin/a") && o.stdout.contains("missing: /usr/share/a/x"), "{}", o.stdout);
    e.pkg(&["verify", "nope"]).fails("not installed");
}

#[test]
fn tampered_repository_package_is_refused() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\n", &[("opt/a", "original\n", 0o644)]);
    // swap the package for a different one after the index was written
    let src = e.package("evil", "name: a\nversion: 1\n", &[("opt/a", "evil!!!!\n", 0o644)]);
    run(&["build", &src, &e.s("src")]).ok();
    fs::copy(e.p("src/a-1.fpk"), e.p("repo/a-1.fpk")).unwrap();
    e.pkg(&["install", "a"]).fails("checksum mismatch");
    assert!(!e.root("opt/a").exists());
}

#[test]
fn file_conflicts() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\n", &[("usr/bin/same", "a\n", 0o755)]);
    e.publish("b", "name: b\nversion: 1\n", &[("usr/bin/same", "b\n", 0o755)]);
    e.pkg(&["install", "a"]).ok();
    e.pkg(&["install", "b"]).fails("already owned by package a");
    assert_eq!(read(&e.root("usr/bin/same")), "a\n");

    e.publish("c", "name: c\nversion: 1\n", &[("etc/handmade", "pkg\n", 0o644)]);
    fs::create_dir_all(e.root("etc")).unwrap();
    fs::write(e.root("etc/handmade"), "mine\n").unwrap();
    e.pkg(&["install", "c"]).fails("belongs to no package");
    assert_eq!(read(&e.root("etc/handmade")), "mine\n");
    e.pkg(&["--force", "install", "c"]).ok();
    assert_eq!(read(&e.root("etc/handmade")), "pkg\n");

    e.publish("d", "name: d\nversion: 1\nconflicts: a\n", &[("opt/d", "d\n", 0o644)]);
    e.pkg(&["install", "d"]).fails("d conflicts with a");
}

#[test]
fn local_file_install_and_setuid_bits() {
    let e = Env::new();
    let src = e.package("s", "name: suidtool\nversion: 3\n", &[("usr/bin/suidtool", "x\n", 0o4755)]);
    run(&["build", &src, &e.s("src")]).ok();
    let root = e.s("root");
    run(&["--root", &root, "install", &e.s("src/suidtool-3.fpk")]).ok();
    assert_eq!(fs::metadata(e.root("usr/bin/suidtool")).unwrap().mode() & 0o7777, 0o4755, "setuid survives installation");
    assert!(run(&["--root", &root, "verify"]).ok().stdout.contains("all files match"));
}

#[test]
fn missing_dependency_and_repo_errors_are_clear() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\ndepends: b>=2\n", &[("opt/a", "a\n", 0o644)]);
    e.publish("b", "name: b\nversion: 1\n", &[("opt/b", "b\n", 0o644)]);
    e.pkg(&["install", "a"]).fails("a 1 needs b>=2");
    e.pkg(&["install", "nothing"]).fails("not found in any repository");
    let root = e.s("root");
    run(&["--root", &root, "--repo", "/nonexistent-repo", "install", "a"]).fails("cannot read INDEX");
    run(&["--root", &root, "install", "a", "--force"]).fails("options go before the command");
    run(&["frobnicate"]).fails("unknown command");
}

#[test]
fn a_held_lock_blocks_changes() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\n", &[("opt/a", "a\n", 0o644)]);
    fs::create_dir_all(e.root("var/lib/pkg")).unwrap();
    fs::write(e.root("var/lib/pkg/lock"), "123\n").unwrap();
    e.pkg(&["install", "a"]).fails("another pkg seems to be running");
    e.pkg(&["list"]).ok();
    fs::remove_file(e.root("var/lib/pkg/lock")).unwrap();
    e.pkg(&["install", "a"]).ok();
    assert!(!e.root("var/lib/pkg/lock").exists(), "the lock is released");
}

#[test]
fn pkg_conf_repositories() {
    let e = Env::new();
    e.publish("a", "name: a\nversion: 1\n", &[("opt/a", "a\n", 0o644)]);
    fs::create_dir_all(e.root("etc/flint")).unwrap();
    fs::write(e.root("etc/flint/pkg.conf"), format!("# local repo\nrepo {}\n", e.s("repo"))).unwrap();
    let root = e.s("root");
    assert_eq!(run(&["--root", &root, "repos"]).ok().stdout, format!("{}\n", e.s("repo")));
    run(&["--root", &root, "install", "a"]).ok();
    assert!(e.root("opt/a").exists());
}

// ---------------------------------------------------------------- hostile packages
// Hand-made archives, so the tests do not depend on pkg's own writer.

fn tar_header(name: &str, typeflag: u8, size: u64, link: &str, mode: u32) -> [u8; 512] {
    let mut b = [0u8; 512];
    b[..name.len()].copy_from_slice(name.as_bytes());
    let oct = |v: u64, w: usize| format!("{v:0w$o}\0", w = w - 1);
    b[100..108].copy_from_slice(oct(u64::from(mode), 8).as_bytes());
    b[108..116].copy_from_slice(oct(0, 8).as_bytes());
    b[116..124].copy_from_slice(oct(0, 8).as_bytes());
    b[124..136].copy_from_slice(oct(size, 12).as_bytes());
    b[136..148].copy_from_slice(oct(0, 12).as_bytes());
    b[148..156].fill(b' ');
    b[156] = typeflag;
    b[157..157 + link.len()].copy_from_slice(link.as_bytes());
    b[257..263].copy_from_slice(b"ustar\0");
    b[263..265].copy_from_slice(b"00");
    let sum: u32 = b.iter().map(|&x| u32::from(x)).sum();
    b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    b
}

enum Ent<'a> {
    File(&'a str, &'a [u8]),
    Link(&'a str, &'a str),
    Dir(&'a str),
}

fn hostile(path: &Path, control: &str, entries: &[Ent], truncate_last: bool) {
    let mut out = Vec::new();
    let add_file = |out: &mut Vec<u8>, name: &str, data: &[u8]| {
        out.extend_from_slice(&tar_header(name, b'0', data.len() as u64, "", 0o644));
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    };
    add_file(&mut out, ".FLINTPKG", control.as_bytes());
    for e in entries {
        match e {
            Ent::File(n, d) => add_file(&mut out, n, d),
            Ent::Link(n, t) => out.extend_from_slice(&tar_header(n, b'2', 0, t, 0o777)),
            Ent::Dir(n) => out.extend_from_slice(&tar_header(n, b'5', 0, "", 0o755)),
        }
    }
    if truncate_last {
        out.truncate(out.len() - 400);
    } else {
        out.extend_from_slice(&[0u8; 1024]);
    }
    fs::write(path, out).unwrap();
}

fn install_file(e: &Env, file: &str) -> Out {
    run(&["--root", &e.s("root"), "install", &e.s(file)])
}

#[test]
fn path_traversal_is_refused() {
    let e = Env::new();
    hostile(&e.p("src/a-1.fpk"), "name: a\nversion: 1\n", &[Ent::File("../escape", b"x")], false);
    install_file(&e, "src/a-1.fpk").fails("escapes");
    assert!(!e.p("escape").exists());

    hostile(&e.p("src/b-1.fpk"), "name: b\nversion: 1\n", &[Ent::File("/etc/evil", b"x")], false);
    install_file(&e, "src/b-1.fpk").fails("absolute path");

    hostile(&e.p("src/c-1.fpk"), "name: c\nversion: 1\n", &[Ent::File("var/lib/pkg/installed/root", b"x")], false);
    install_file(&e, "src/c-1.fpk").fails("reserved path");
    // Only pkg's own (empty) state directory exists; no package file was written.
    let top: Vec<String> = fs::read_dir(e.p("root")).unwrap().flatten().map(|d| d.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(top, ["var"]);
    assert!(fs::read_dir(e.root("var/lib/pkg")).unwrap().next().is_none(), "no database entry, no lock left");
}

#[test]
fn writing_through_symlinks_is_refused() {
    let e = Env::new();
    let outside = e.p("outside");
    fs::create_dir_all(&outside).unwrap();

    // a symlink in the package, then a file "inside" it
    hostile(&e.p("src/a-1.fpk"), "name: a\nversion: 1\n", &[Ent::Link("usr", &outside.to_string_lossy()), Ent::File("usr/pwned", b"x")], false);
    install_file(&e, "src/a-1.fpk").fails("lies below the symlink usr");

    // a symlink already on disk that leads out of the root
    symlink(&outside, e.root("lib")).unwrap();
    hostile(&e.p("src/b-1.fpk"), "name: b\nversion: 1\n", &[Ent::File("lib/pwned", b"x")], false);
    install_file(&e, "src/b-1.fpk").fails("outside the install root");
    assert!(fs::read_dir(&outside).unwrap().next().is_none(), "nothing reached the outside directory");

    // a symlink whose *target* is outside is fine as long as nothing is written through it
    hostile(&e.p("src/c-1.fpk"), "name: c\nversion: 1\n", &[Ent::Dir("usr"), Ent::Link("usr/escape-hatch", "/etc/passwd")], false);
    install_file(&e, "src/c-1.fpk").ok();
    assert_eq!(fs::read_link(e.root("usr/escape-hatch")).unwrap(), Path::new("/etc/passwd"));
}

#[test]
fn malformed_archives_are_refused() {
    let e = Env::new();
    hostile(&e.p("src/a-1.fpk"), "name: a\nversion: 1\n", &[Ent::File("x", b"1"), Ent::File("x", b"2")], false);
    install_file(&e, "src/a-1.fpk").fails("appears twice");

    fs::write(e.p("src/junk.fpk"), b"definitely not a tar archive".repeat(40)).unwrap();
    install_file(&e, "src/junk.fpk").fails("");

    hostile(&e.p("src/b-1.fpk"), "name: wrong\nversion: 1\n", &[], false);
    fs::rename(e.p("src/b-1.fpk"), e.p("src/b.fpk")).unwrap();
    // the control data inside is what counts, not the file name
    install_file(&e, "src/b.fpk").ok();
    assert!(run(&["--root", &e.s("root"), "list"]).ok().stdout.contains("wrong 1"));
}

#[test]
fn a_failed_install_is_rolled_back() {
    let e = Env::new();
    hostile(&e.p("src/a-1.fpk"), "name: a\nversion: 1\n", &[Ent::Dir("opt"), Ent::Dir("opt/a"), Ent::File("opt/a/first", b"first file\n"), Ent::File("opt/a/second", &[b'z'; 3000])], true);
    install_file(&e, "src/a-1.fpk").fails("rolled back");
    assert!(!e.root("opt").exists(), "the files and directories it created are gone");
    assert!(run(&["--root", &e.s("root"), "list"]).ok().stdout.is_empty(), "nothing recorded");
    assert!(!e.root("var/lib/pkg/lock").exists());
}

// ---------------------------------------------------------------- remote repositories

/// A minimal HTTP server for `dir`, on 127.0.0.1, until the test ends.
fn serve(dir: PathBuf) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut r = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').trim_start_matches("repo/").to_string();
            let _ = match fs::read(dir.join(&path)) {
                Ok(body) if !path.contains("..") => conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).and_then(|_| conn.write_all(&body)),
                _ => conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            };
        }
    });
    format!("http://127.0.0.1:{port}/repo")
}

#[test]
fn remote_repository_signed_index_and_downloads() {
    let e = Env::new();
    e.publish("lib", "name: libgreet\nversion: 1.0\ndescription: Greeting library\n", &[("usr/lib/libgreet.txt", "lib\n", 0o644)]);
    e.publish("app", "name: hello\nversion: 1.0\ndescription: Says hello\ndepends: libgreet\n", &[("usr/bin/hello", "#!/bin/sh\necho hi\n", 0o755)]);
    let keys = e.p("keys");
    fs::create_dir_all(&keys).unwrap();
    run(&["key", "new", "test", &keys.to_string_lossy()]).ok();
    run(&["key", "new", "test", &keys.to_string_lossy()]).fails("already exists");
    let secret = keys.join("test.key").to_string_lossy().into_owned();
    assert!(run(&["sign", &e.s("repo"), &secret]).ok().stdout.contains("2 package(s)"));

    let url = serve(e.p("repo"));
    let root = e.s("root");
    let r = |args: &[&str]| {
        let mut all = vec!["--root", root.as_str(), "--repo", url.as_str()];
        all.extend_from_slice(args);
        run(&all)
    };

    // No trusted key yet: nothing from the network is accepted.
    r(&["update"]).fails("no trusted keys");
    r(&["install", "hello"]).fails("no trusted keys");
    fs::create_dir_all(e.root("etc/flint/keys")).unwrap();
    fs::copy(keys.join("test.pub"), e.root("etc/flint/keys/test.pub")).unwrap();

    assert!(r(&["update"]).ok().stdout.contains("2 package(s), signed by test"));
    let o = r(&["install", "hello"]);
    o.ok();
    assert!(o.stdout.contains("downloading libgreet-1.0.fpk") && o.stdout.contains("downloading hello-1.0.fpk"), "{}", o.stdout);
    assert_eq!(read(&e.root("usr/bin/hello")), "#!/bin/sh\necho hi\n");
    assert!(r(&["repos"]).ok().stdout.contains("trusted key: test "));

    // A newer version, signed: upgrade downloads it.
    let old_index = fs::read(e.p("repo/INDEX")).unwrap();
    let old_sig = fs::read(e.p("repo/INDEX.sig")).unwrap();
    e.publish("app", "name: hello\nversion: 2.0\ndescription: Says hello\ndepends: libgreet\n", &[("usr/bin/hello", "#!/bin/sh\necho v2\n", 0o755)]);
    run(&["sign", &e.s("repo"), &secret]).ok();
    r(&["upgrade"]).ok();
    assert_eq!(read(&e.root("usr/bin/hello")), "#!/bin/sh\necho v2\n");

    // An INDEX changed after signing is refused, and the last good one stays in use.
    let good_index = fs::read(e.p("repo/INDEX")).unwrap();
    fs::write(e.p("repo/INDEX"), [&good_index[..], b"\n"].concat()).unwrap();
    r(&["update"]).fails("does not match INDEX");
    assert!(r(&["search", "hello"]).ok().stdout.contains("hello 2.0"));
    fs::write(e.p("repo/INDEX"), &good_index).unwrap();

    // An old (genuinely signed) INDEX is a rollback and is refused.
    let cur_sig = fs::read(e.p("repo/INDEX.sig")).unwrap();
    fs::write(e.p("repo/INDEX"), &old_index).unwrap();
    fs::write(e.p("repo/INDEX.sig"), &old_sig).unwrap();
    r(&["update"]).fails("older than the one already accepted");
    fs::write(e.p("repo/INDEX"), &good_index).unwrap();
    fs::write(e.p("repo/INDEX.sig"), &cur_sig).unwrap();

    // A package swapped on the server does not match the signed SHA-256.
    r(&["remove", "hello"]).ok();
    fs::remove_dir_all(e.root("var/cache/pkg/remote")).ok();
    let pkgfile = e.p("repo/hello-2.0.fpk");
    let mut data = fs::read(&pkgfile).unwrap();
    let last = data.len() - 1;
    data[last] ^= 1;
    fs::write(&pkgfile, data).unwrap();
    r(&["install", "hello"]).fails("does not match the signed INDEX");
    assert!(!e.root("usr/bin/hello").exists());

    // Signed by a key nobody trusts.
    run(&["key", "new", "evil", &keys.to_string_lossy()]).ok();
    run(&["sign", &e.s("repo"), &keys.join("evil.key").to_string_lossy()]).ok();
    r(&["update"]).fails("not trusted");
}
