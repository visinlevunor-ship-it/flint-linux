# Ports

Recipes that build third-party programs into `pkg` package sources: `ports/NAME/build.sh
OUTDIR` stages `OUTDIR/control` and `OUTDIR/files/`, then `pkg build OUTDIR REPO`.

| Port | What | How |
|---|---|---|
| fastfetch | system information | static musl (musl-gcc) |
| nano, htop, tmux | editor, process viewer, terminal multiplexer | static, ncurses with the common terminals built in |
| curl, git | downloads, version control | static, TLS by mbedTLS; depend on ca-certificates |
| ca-certificates | Mozilla's CA list as `/etc/ssl/certs/ca-certificates.crt` | from Ubuntu's package |
| micro | editor | static Go build |
| btop | resource monitor | static C++ (zig's libc++) |
| neovim | editor | static; its tree-sitter parsers compiled in |
| kernels | `linux-zen`, `linux-lts`, `linux-upstream` | from Arch Linux (see `kernels/build.sh`) |

Sources are pinned: release tarballs as Ubuntu's archive keeps them (by SHA-256), or GitHub
at fixed commits. Everything but fastfetch and the kernels is built with zig
(`zig cc -target x86_64-linux-musl`, or `pip install ziglang`) on a Linux build host, since
configure scripts run what they compile; `common.sh` lists the other tools needed.

```sh
ports/nano/build.sh /tmp/nano && pkg build /tmp/nano repo/x86_64/
pkg index repo/x86_64/ && pkg sign repo/x86_64/ ~/flint-keys/flint.key
```
