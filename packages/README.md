# Example packages

Package sources for `pkg build`. Each directory holds a `control` file and a `files/`
tree that is installed relative to `/`.

```sh
pkg build packages/hello repo/
pkg build packages/greet-data repo/
pkg index repo/
pkg --root /tmp/test-root --repo repo/ install hello
```

`image/build-initramfs.sh` builds these into `/var/cache/pkg/repo` inside the image and
points `/etc/flint/pkg.conf` at it, so `pkg install hello` works in QEMU.

## control file

```
name: hello                  # lowercase letters, digits, + . _ -
version: 1.0                 # compared numerically: 1.10 > 1.9, 1.0rc1 < 1.0
description: one line
arch: any                    # or x86_64
depends: greet-data>=1.0     # comma separated; operators >= <= > < =
conflicts: other-hello       # optional
config: etc/greet.conf       # files an upgrade must not overwrite once the user edits them
```
