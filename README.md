# Flint Linux

A from-scratch Linux userland written in Rust. Only the kernel and its modules are upstream; init, shell, login and package manager are Flint's own.

Version lives in `VERSION` (`MAJOR.MINOR`). A rewrite of a component or a major architectural change bumps MAJOR; everything else bumps MINOR. `flint-version` prints it.

## Crates

| crate | what | state |
|---|---|---|
| `flint-core` | shared: version, power actions, syscall wrappers, logging | done |
| `flint-rt` (`rt/`) | libc replacement: syscalls, `_start`, allocator, `mem*`, panic, stdio (no_std, x86_64 Linux) | foundation done, tested in QEMU |
| `flint-version` (`rt/`) | prints `Flint Linux 0.2 (abc1234)`; first program on flint-rt, no libc | done |
| `flint-init` | PID 1: early mounts, boot tasks, service supervision, clean shutdown | written, needs boot test |
| `flint-power` | `poweroff` / `reboot` / `halt` (signals init, or `-f` for direct) | written, needs boot test |
| `flint-login` | getty + login replacement | planned |
| `flint-sh` | shell | planned |
| `flint-modload` | loads modules from `/sys/**/modalias` | planned |
| `flint-pkg` | package manager | planned |

## Build and test

Flint targets Linux. On a FreeBSD/macOS host, cross-build or use a Linux VM.

```sh
# std crates (flint-core, flint-init, flint-power); needs a Linux host for `cargo test`
cargo test --workspace
rustup target add x86_64-unknown-linux-musl
cargo build --release --workspace --target x86_64-unknown-linux-musl

# libc-free crates (flint-rt, flint-version): separate workspace, panic=abort
cd rt && cargo build --release --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/flint-version --rt-selftest   # on Linux
```

## Boot it in QEMU

```sh
BUSYBOX=/path/to/static-busybox image/build-initramfs.sh
KERNEL=/path/to/bzImage image/run-qemu.sh          # Ctrl-A X quits
```

`OVERLAY=dir` copies extra files over the image; `SERVICES=file` swaps the services list.

`flint-init --check image/etc/flint` validates the config files without booting.

## Config

`/etc/flint/services` and `/etc/flint/boot`, examples in `image/etc/flint/`.
