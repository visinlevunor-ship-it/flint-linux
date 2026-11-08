# Flint Linux

A from-scratch Linux userland written in Rust. Only the kernel and its modules are upstream; init, shell, login and package manager are Flint's own.

Version lives in `VERSION` (`MAJOR.MINOR`). A rewrite of a component or a major architectural change bumps MAJOR; everything else bumps MINOR. `flint-version` prints it.

## Crates

| crate | what | state |
|---|---|---|
| `flint-core` | shared: version, power actions, syscall wrappers, logging | done |
| `flint-version` | prints `Flint Linux 0.1 (abc1234)` | done |
| `flint-init` | PID 1: early mounts, boot tasks, service supervision, clean shutdown | written, needs boot test |
| `flint-power` | `poweroff` / `reboot` / `halt` (signals init, or `-f` for direct) | written, needs boot test |
| `flint-login` | getty + login replacement | planned |
| `flint-sh` | shell | planned |
| `flint-modload` | loads modules from `/sys/**/modalias` | planned |
| `flint-pkg` | package manager | planned |

## Build and test

```sh
cargo test --workspace
rustup target add x86_64-unknown-linux-musl
cargo build --release --workspace --target x86_64-unknown-linux-musl
```

`flint-init --check image/etc/flint` validates the config files without booting.

## Config

`/etc/flint/services` and `/etc/flint/boot`, examples in `image/etc/flint/`.
