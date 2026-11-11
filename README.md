# Flint Linux

A small Linux distribution with a from-scratch userland written in Rust. Only the kernel and its modules are upstream; the init system, login, shell, core tools, package manager and account handling are Flint's own.

[![Support Flint on Patreon](https://img.shields.io/badge/Patreon-support%20Flint-f96854?logo=patreon&logoColor=white)](https://www.patreon.com/c/FlintLinux/membership)

**Status: 0.5, early development.** Flint boots in QEMU to a login prompt, a working shell with job control, and a package manager that installs from a local repository. It is not installable on real hardware yet (no installer, no bootloader setup, no networking).

## What works today

| Component | What it is | State |
|---|---|---|
| `flint-init` | PID 1: early mounts, boot tasks, service supervision with respawn backoff, clean shutdown | tested in QEMU |
| `flint-power` | `poweroff` / `reboot` / `halt` | tested in QEMU |
| `flint-login` | getty + login: banner, password check, privilege drop, login shell | tested in QEMU |
| `flint-passwd` | `passwd`; setuid root | tested in QEMU |
| `flint-su` | `su`; setuid root | tested in QEMU |
| `flint-sh` | the shell (`/bin/sh`): POSIX-style scripting, pipelines, redirections, job control | tested in QEMU and on a pty |
| `flint-pkg` | package manager: dependencies, upgrades, config-file protection, verify | tested in QEMU |
| `flint-box` | multicall toolbox: `ls cat cp mv rm mkdir mount ps chmod id ...` (30 applets) | tested in QEMU |
| `flint-auth` | SHA-256, PBKDF2 password hashes, passwd/shadow/group parsing (pure Rust) | unit tested |
| `flint-core` | shared code: version, power actions, syscall wrappers, logging, tty helpers | unit tested |
| `flint-rt` | libc replacement for `no_std` programs (syscalls, `_start`, heap, `mem*`) | `flint-version` runs on it |
| `flint-version` | prints `Flint Linux 0.5 (abc1234)`; first program on `flint-rt`, no libc | tested in QEMU |

Not written yet: network repositories for `flint-pkg`, `flint-modload` (module autoloading), networking, a line editor for `flint-sh`, an installer.

## Quick start (QEMU)

You need Rust with the Linux target, QEMU, a static Linux `busybox`, and a Linux x86_64 kernel image.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --workspace --target x86_64-unknown-linux-musl     # std crates
(cd rt && cargo build --release --target x86_64-unknown-linux-musl)      # libc-free crates
cargo build --release -p flint-pkg                                        # a flint-pkg for this host

BUSYBOX=/path/to/static-busybox image/build-initramfs.sh
KERNEL=/path/to/bzImage image/run-qemu.sh          # Ctrl-A then X quits QEMU
```

The dev image has two accounts, `root` and `flint`, both with the password `flint`. Change it with `passwd`. For a root shell without login: `SERVICES=image/etc/flint/services.qemu-shell`.

The host `flint-pkg` builds the example packages in `packages/` into a repository inside the image, so `flint-pkg install hello` works as root. Without it the image still builds, just without that repository.

`.cargo/config.toml` and `rt/.cargo/config.toml` carry the linker flags the musl cross-build needs (needed when the host's `cc` is not GNU, e.g. FreeBSD).

## Packages

```sh
flint-pkg build packages/hello repo/          # NAME-VERSION.fpk from control + files/
flint-pkg index repo/                         # write repo/INDEX (checksums)
flint-pkg --repo repo/ install hello          # also installs greet-data, its dependency
flint-pkg list | info hello | files hello | owns /usr/bin/hello | search greet
flint-pkg upgrade                             # newest versions from the repositories
flint-pkg verify                              # compare files with recorded checksums
flint-pkg remove hello && flint-pkg autoremove
```

Repositories come from `--repo DIR` and from `/etc/flint/pkg.conf` (`repo /path/to/dir` lines). `--root DIR` installs into another tree, so packages can be tested without root. `-n` shows what would happen. See `packages/README.md` for the control file format.

How it stays safe:

- A package is a plain ustar archive with a `.FLINTPKG` control entry first; the repository `INDEX` records each file's size and SHA-256, and a package that does not match is refused.
- Paths are checked before anything is written: no absolute paths, no `..`, nothing below a symlink from the same package, and nothing written through a symlink that leads out of the install root.
- A file owned by another package, or an existing file that belongs to no package, is a conflict (`--force` overrides the second).
- Each file is written to a temporary name and renamed into place; a fresh install that fails half way is rolled back; the database record is written last.
- Config files listed in `config:` that the user edited are never overwritten: an upgrade saves the new version as `FILE.flint-new`, and a removal keeps the file.

## Testing

```sh
cargo test --workspace                          # unit and integration tests; needs a Linux host
tests/pty/jobcontrol.py target/debug/flint-sh   # interactive flint-sh on a real pty (Linux)
KERNEL=/path/to/bzImage tests/qemu/session.py   # full QEMU session over the serial console
```

- `cargo test` runs the unit tests plus integration tests that execute the real `flint-box`, `flint-sh` and `flint-pkg` binaries. The `flint-pkg` tests include hostile packages: path traversal, writing through symlinks, tampered downloads, duplicate entries and a truncated archive that must roll back.
- `tests/pty/jobcontrol.py` drives an interactive `flint-sh` on a pseudo-terminal: prompt, multi-line input, Ctrl-C, Ctrl-Z, `fg`, `bg`, `jobs`, `kill %N`, pipelines.
- `tests/qemu/session.py` boots the image and checks login (good and bad), privilege dropping, `su`, `passwd`, `flint-box`, shell job control, `flint-pkg` and poweroff. It needs the image built first (see Quick start). QEMU without KVM is slow; set `SLOW=3` to stretch its timeouts.

## Layout

```
crates/flint-core      shared code (std)
crates/flint-auth      accounts and password hashes (pure Rust)
crates/flint-init      PID 1
crates/flint-power     poweroff / reboot / halt
crates/flint-login     getty + login
crates/flint-passwd    passwd (setuid)
crates/flint-su        su (setuid)
crates/flint-sh        shell
crates/flint-pkg       package manager
crates/flint-box       multicall toolbox
rt/                    separate workspace: flint-rt and libc-free programs
packages/              example package sources
image/                 initramfs and QEMU scripts, default /etc files
tests/                 pty and QEMU end-to-end tests
VERSION                the distro version
```

## Design notes

- **Version.** `VERSION` holds `MAJOR.MINOR`. A rewrite of a component or any major architectural change bumps MAJOR; everything else bumps MINOR. `flint-version` prints it with the git hash. 1.0 is planned for when the userland no longer depends on musl.
- **Own libc, in stages.** The std crates currently link musl statically. `flint-rt` is the replacement: raw syscalls, `_start`, a heap, `mem*`, panics. It is not a C library, so it does not run C programs; it exists for Flint's own Rust programs. Porting the std crates onto it is the road to 1.0.
- **Passwords.** PBKDF2-HMAC-SHA256, 100,000 iterations, 16 random salt bytes, stored as `$flint1$<iterations>$<salt hex>$<hash hex>` in `/etc/shadow` (mode 0600). A field that is empty or starts with `!` or `*` is a locked account.
- **Setuid policy.** Only `flint-passwd` and `flint-su` are setuid root: small single-purpose programs that take the caller's identity from the real uid. `su` honours a `wheel` group if one exists, delays a wrong password by 2 seconds, and lets only root choose the shell. `flint-box` must never be setuid: it would run every applet as root.
- **Init configuration.** `/etc/flint/services` (supervised services: name, respawn or once, tty, command) and `/etc/flint/boot` (commands run in order before services). No shell and no quoting in either file. `flint-init --check DIR` validates them without booting.
- **Package database.** One text file per installed package in `/var/lib/flint-pkg/installed/`, listing every file with its mode and SHA-256. `/var/lib/flint-pkg/lock` keeps two `flint-pkg` runs from changing the system at once.

## Known limitations

- Linux x86_64 only.
- **flint-pkg** reads local directory repositories only (no network yet), does not compress packages, and does not sign them: the INDEX checksums protect against damaged or swapped files, not against someone who can rewrite the INDEX too. The base system itself is not packaged. An upgrade that fails half way is not rolled back (a fresh install is).
- **flint-sh** has no line editor yet: no history or tab completion (the terminal's own line editing works). Not supported: `trap`, aliases, `set -e`/`-u`/`-x`, arrays, `[[ ]]`, process substitution, `${x/a/b}`. A script is parsed completely before it runs.
- `flint-box` has 30 applets; some options are missing (for example `head -c`). The image still uses busybox for `grep`, `sed`, `date` and a few others.
- The dev image ships a known password. Do not use it for anything real.
- `cargo test` does not run on non-Linux hosts.

## Support

Flint is built in the open. If you want to help it along, you can support it on Patreon: https://www.patreon.com/c/FlintLinux/membership

## License

MIT License
