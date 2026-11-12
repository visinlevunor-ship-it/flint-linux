//! flint-mkimage: build tool for Flint images. Runs on the build machine.

mod cpio;
mod disk;
mod drivers;
mod elf;

use std::fs;
use std::io::BufWriter;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: flint-mkimage cpio DIR OUTPUT
         pack DIR as a newc cpio archive (everything owned by root)
       flint-mkimage drivers CONFIG KERNEL_ROOT FIRMWARE_DIR EARLY_OUT TREE_OUT
         pick modules (lib/modules/VERSION under KERNEL_ROOT) and firmware by CONFIG and
         generate modules.dep/alias/softdep; write all of them as files under TREE_OUT
         (modules/VERSION/..., firmware/...) and the early ones (CONFIG's early lines, what
         is needed to reach the boot partition) as the cpio archive EARLY_OUT
       flint-mkimage disk OUTPUT SIZE_MIB DIR [LABEL]
         a GPT disk image with one EFI system partition (FAT32, label FLINT by default)
         holding DIR (long file names are fine)";

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("cpio") => {
            let [_, dir, out] = args else { return Err(USAGE.into()) };
            let f = fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
            let mut w = cpio::Writer::new(BufWriter::new(f));
            cpio::add_tree(&mut w, Path::new(dir), "").map_err(|e| format!("{dir}: {e}"))?;
            w.finish().map_err(|e| format!("{out}: {e}"))?;
            Ok(())
        }
        Some("drivers") => {
            let [_, conf, kroot, fw, early_out, tree_out] = args else { return Err(USAGE.into()) };
            // Walk the real directories, even if the paths given are symlinks to them.
            let real = |p: &String| fs::canonicalize(p).map(|r| r.to_string_lossy().into_owned()).map_err(|e| format!("{p}: {e}"));
            let (kroot, fw) = (&real(kroot)?, &real(fw)?);
            let text = fs::read_to_string(conf).map_err(|e| format!("{conf}: {e}"))?;
            let cfg = drivers::Config::parse(&text).map_err(|e| format!("{conf}: {e}"))?;
            let (version, moddir) = drivers::find_module_dir(Path::new(kroot))?;
            let sel = drivers::select_modules(&moddir, &version, &cfg)?;
            let choice = drivers::select_firmware(Path::new(fw), &sel.firmware_wanted, &cfg.firmware)?;
            let tree = Path::new(tree_out);
            let msize = drivers::write_modules_tree(&moddir, &sel, &tree.join("modules"))?;
            let fsize = drivers::write_firmware_tree(Path::new(fw), &choice.files, &tree.join("firmware"))?;
            let early = drivers::subset(&sel, &cfg.early);
            let early_fw = drivers::select_firmware(Path::new(fw), &early.firmware_wanted, &[])?;
            let esize = drivers::write_early_img(&moddir, Path::new(fw), &early, &early_fw.files, Path::new(early_out))?;
            println!(
                "kernel {version}: {} modules ({} KiB), {} firmware files ({} KiB); early: {} modules, {} firmware files ({} KiB)",
                sel.modules.len(),
                msize / 1024,
                choice.files.len(),
                fsize / 1024,
                early.modules.len(),
                early_fw.files.len(),
                esize / 1024
            );
            if !choice.missing.is_empty() {
                println!("{} declared firmware names not found (drivers that need them will say so in dmesg)", choice.missing.len());
            }
            Ok(())
        }
        Some("disk") => {
            let (out, size, dir, label) = match args {
                [_, o, s, d] => (o, s, d, "FLINT"),
                [_, o, s, d, l] => (o, s, d, l.as_str()),
                _ => return Err(USAGE.into()),
            };
            let size: u64 = size.parse().map_err(|_| format!("size '{size}': a number of MiB"))?;
            let tree = disk::read_tree(Path::new(dir))?;
            // The GUIDs come from the contents: the same files give the same image.
            let mut seed = flint_auth::sha256::Sha256::new();
            hash_tree(&mut seed, &tree);
            let made = disk::make(Path::new(out), size, label, &seed.finalize(), &tree)?;
            println!(
                "{out}: {size} MiB, EFI partition {} ({} MiB used of {} MiB)",
                made.part_guid,
                made.used_bytes.div_ceil(1024 * 1024),
                made.partition_bytes / (1024 * 1024)
            );
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn hash_tree(h: &mut flint_auth::sha256::Sha256, tree: &[(String, disk::Node)]) {
    for (name, node) in tree {
        h.update(name.as_bytes());
        match node {
            disk::Node::File(flint_disk::fat::Data::Bytes(d)) => {
                h.update(&(d.len() as u64).to_le_bytes());
                h.update(d);
            }
            disk::Node::File(flint_disk::fat::Data::File(p, len)) => {
                h.update(&len.to_le_bytes());
                if let Ok(d) = std::fs::read(p) {
                    h.update(&d);
                }
            }
            disk::Node::Dir(c) => {
                h.update(b"/");
                hash_tree(h, c);
                h.update(b"..");
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flint-mkimage: {e}");
            ExitCode::FAILURE
        }
    }
}
