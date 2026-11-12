//! Check what flint-disk writes with the standard tools (e2fsck, debugfs, fsck.vfat,
//! mtools). Each check is skipped when its tool is not installed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use flint_disk::{ext4, fat, gpt};

fn have(tool: &str) -> bool {
    Command::new("sh").arg("-c").arg(format!("command -v {tool}")).output().map(|o| o.status.success()).unwrap_or(false)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("flint-disk-{}-{name}", std::process::id()));
    let _ = fs::remove_file(&d);
    d
}

fn run(cmd: &str, args: &[&str]) -> (bool, String) {
    let o = Command::new(cmd).args(args).output().unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn mkfs_file(path: &Path, mib: u64) {
    let f = fs::OpenOptions::new().create(true).write(true).truncate(true).open(path).unwrap();
    f.set_len(mib << 20).unwrap();
    let mut f = f;
    ext4::mkfs(&mut f, 0, mib << 20, &ext4::Options { label: "FLINTDATA".into(), uuid: None, time: 1_800_000_000 }).unwrap();
}

#[test]
fn ext4_passes_e2fsck_at_many_sizes() {
    if !have("e2fsck") {
        eprintln!("skipped: no e2fsck");
        return;
    }
    // one group, a partial last group, many groups (backups at 1, 3, 5, 7, 9, 25, 27)
    for mib in [16u64, 100, 129, 1000, 3500, 7000] {
        let p = tmp(&format!("ext4-{mib}"));
        mkfs_file(&p, mib);
        let (ok, out) = run("e2fsck", &["-fn", p.to_str().unwrap()]);
        assert!(ok, "e2fsck on {mib} MiB:\n{out}");
        if have("debugfs") {
            let (_, ls) = run("debugfs", &["-R", "ls -l /", p.to_str().unwrap()]);
            assert!(ls.contains("lost+found"), "{ls}");
        }
        fs::remove_file(&p).unwrap();
    }
}

#[test]
fn fat_with_long_names_passes_fsck_and_mtools() {
    let p = tmp("fat");
    let tree = vec![
        ("EFI".to_string(), fat::Node::Dir(vec![("BOOT".to_string(), fat::Node::Dir(vec![("BOOTX64.EFI".to_string(), fat::Node::File(fat::Data::Bytes(vec![1; 5000])))]))])),
        ("FLINT".to_string(), fat::Node::Dir(vec![
            ("kernel-7.0.0-42-generic".to_string(), fat::Node::File(fat::Data::Bytes(b"kernel".repeat(1000)))),
            ("modules-7.0.0-42-generic.img".to_string(), fat::Node::File(fat::Data::Bytes(vec![7; 100_000]))),
            ("modules-7.0.0-42-generica.img".to_string(), fat::Node::File(fat::Data::Bytes(vec![8; 10]))),
            ("empty".to_string(), fat::Node::File(fat::Data::Bytes(Vec::new()))),
        ])),
    ];
    let mut t = gpt::Table::new(512, 200 * 2048, [3; 16]).unwrap();
    t.add(gpt::ESP, [4; 16], 2048, 2048 * 150 - 1, "boot").unwrap();
    let mut f = fs::OpenOptions::new().create(true).write(true).read(true).truncate(true).open(&p).unwrap();
    f.set_len(200 << 20).unwrap();
    t.write(&mut f, true).unwrap();
    fat::format(&mut f, 2048 * 512, 2048 * 149, 2048, "FLINT", 0x1234_5678, &tree).unwrap();
    drop(f);
    let part = format!("{}@@1M", p.display());
    if have("mdir") {
        let (ok, out) = run("mdir", &["-i", &part, "-/", "::"]);
        assert!(ok, "{out}");
        assert!(out.contains("modules-7.0.0-42-generic.img") && out.contains("modules-7.0.0-42-generica.img") && out.contains("kernel-7.0.0-42-generic"), "{out}");
        let (ok, out) = run("mtype", &["-i", &part, "::/FLINT/kernel-7.0.0-42-generic"]);
        assert!(ok && out == "kernel".repeat(1000), "content");
    }
    if have("fsck.vfat") {
        // fsck.vfat wants the partition alone
        let data = fs::read(&p).unwrap();
        let q = tmp("fatpart");
        fs::write(&q, &data[2048 * 512..2048 * 512 + 2048 * 149 * 512]).unwrap();
        let (ok, out) = run("fsck.vfat", &["-n", "-v", q.to_str().unwrap()]);
        assert!(ok && !out.contains("differ"), "{out}");
        fs::remove_file(&q).unwrap();
    }
    fs::remove_file(&p).unwrap();
}
