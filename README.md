# Flint Linux

A small Linux distribution with a from-scratch userland written in Rust. Only the kernel and its modules are upstream; the init system, login, shell, core tools and account handling are Flint's own.

**Status: 0.3, early development.** Flint boots in QEMU to a login prompt and a working shell. It is not installable on real hardware yet (no installer, no bootloader setup, no networking, no package manager).

## What works today

| Component | What it is | State |
|---|---|---|
| `flint-init` | PID 1: early mounts, boot tasks, service supervision with respawn backoff, clean shutdown | tested in QEMU |
| `flint-power` | `poweroff` / `reboot` / `halt` | tested in QEMU |
| `flint-login` | getty + login: banner, password check, privilege drop, login shell | tested in QEMU |
| `flint-passwd` | `passwd`; setuid root | tested in QEMU |
| `flint-su` | `su`; setuid root | next |
| `flint-sh` | the shell: POSIX-style scripting, pipelines, redirections, job control | next (the image still uses busybox's shell) |
| `flint-box` | multicall toolbox: `ls cat cp mv rm mkdir mount ps chmod ...` (30 applets) | tested in QEMU |
| `flint-auth` | SHA-256, PBKDF2 password hashes, passwd/shadow/group parsing (pure Rust) | unit tested |
| `flint-core` | shared code: version, power actions, syscall wrappers, logging, tty helpers | unit tested |
| `flint-rt` | libc replacement for `no_std` programs (syscalls, `_start`, heap, `mem*`) | `flint-version` runs on it |
| `flint-version` | prints `Flint Linux 0.3 (abc1234)`; first program on `flint-rt`, no libc | tested in QEMU |

Not written yet: `flint-pkg` (package manager), `flint-modload` (module autoloading), networking, an installer.

## Quick start (QEMU)

You need Rust with the Linux target, QEMU, a static Linux `busybox`, and a Linux x86_64 kernel image.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --workspace --target x86_64-unknown-linux-musl     # std crates
(cd rt && cargo build --release --target x86_64-unknown-linux-musl)      # libc-free crates

BUSYBOX=/path/to/static-busybox image/build-initramfs.sh
KERNEL=/path/to/bzImage image/run-qemu.sh          # Ctrl-A then X quits QEMU
```

The dev image has two accounts, `root` and `flint`, both with the password `flint`. Change it with `passwd`. For a root shell without login: `SERVICES=image/etc/flint/services.qemu-shell`.

`.cargo/config.toml` and `rt/.cargo/config.toml` carry the linker flags the musl cross-build needs (needed when the host's `cc` is not GNU, e.g. FreeBSD).

## Testing

```sh
cargo test --workspace            # needs a Linux host
```

`cargo test` runs the unit tests and the integration tests that execute the real `flint-box` binary. The QEMU end-to-end session test (login, privilege dropping, `passwd`, `flint-box`, poweroff over the serial console) is not in the repo yet.

## Layout

```
crates/flint-core      shared code (std)
crates/flint-auth      accounts and password hashes (pure Rust)
crates/flint-init      PID 1
crates/flint-power     poweroff / reboot / halt
crates/flint-login     getty + login
crates/flint-passwd    passwd (setuid)
crates/flint-box       multicall toolbox
rt/                    separate workspace: flint-rt and libc-free programs
image/                 initramfs and QEMU scripts, default /etc files
VERSION                the distro version
```

## Design notes

- **Version.** `VERSION` holds `MAJOR.MINOR`. A rewrite of a component or any major architectural change bumps MAJOR; everything else bumps MINOR. `flint-version` prints it with the git hash. 1.0 is planned for when the userland no longer depends on musl.
- **Own libc, in stages.** The std crates currently link musl statically. `flint-rt` is the replacement: raw syscalls, `_start`, a heap, `mem*`, panics. It is not a C library, so it does not run C programs; it exists for Flint's own Rust programs. Porting the std crates onto it is the road to 1.0.
- **Passwords.** PBKDF2-HMAC-SHA256, 100,000 iterations, 16 random salt bytes, stored as `$flint1$<iterations>$<salt hex>$<hash hex>` in `/etc/shadow` (mode 0600). A field that is empty or starts with `!` or `*` is a locked account.
- **Setuid policy.** Only `flint-passwd` is setuid root (`flint-su` will be the second): a small single-purpose program that takes the caller's identity from the real uid. `flint-box` must never be setuid: it would run every applet as root.
- **Init configuration.** `/etc/flint/services` (supervised services: name, respawn or once, tty, command) and `/etc/flint/boot` (commands run in order before services). No shell and no quoting in either file. `flint-init --check DIR` validates them without booting.

## Known limitations

- Linux x86_64 only.
- There is no `su` yet, and the shell is still busybox's `ash`.
- The image still uses busybox for a few tools (`grep`, `sed`, `date`, ...).
- The dev image ships a known password. Do not use it for anything real.
- `cargo test` does not run on non-Linux hosts.

## License

None chosen yet.

# pls note gng

i didnt commit cuz i dont think its finished and its buggy like half of the time
i dont wanna ship an unfinished thing
