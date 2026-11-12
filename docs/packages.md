# Packages

## Using pkg

```sh
pkg install fastfetch          # fetch the signed list, download, check, install
pkg remove hello && pkg autoremove
pkg upgrade                    # newest versions from every repository
pkg update                     # refresh the package lists (install and upgrade do it too)
pkg list                       # installed packages
pkg info hello
pkg files hello
pkg owns /usr/bin/hello
pkg search greet
pkg verify                     # compare installed files with their recorded checksums
```

Useful options:
- `-n` shows what would happen without doing it.
- `--repo DIR-OR-URL` adds a repository for one command.
- `--root DIR` installs into another tree, so you can test packages without root.

Repositories are listed in `/etc/flint/pkg.conf`, one per line: `repo /path/to/dir` or
`repo https://host/path`. Packages may be gzip-compressed. Config files a package declares are
never overwritten once you have edited them: an upgrade saves the new version as
`FILE.flint-new`, and a removal keeps your file.

## Making packages

A package source is a directory with a `control` file and a `files/` tree, which is installed
relative to `/`. See [`packages/README.md`](../packages/README.md) for the control file format.

```sh
pkg build packages/hello repo/       # -> repo/hello-VERSION.fpk
pkg index repo/                      # -> repo/INDEX (sizes and checksums)
pkg --root /tmp/t --repo repo/ install hello
```

Third-party programs are built by the recipes in [flint-packages](https://github.com/visinlevunor-ship-it/flint-packages) (`ports/`), next to the
packages they make:

```sh
ports/neovim/build.sh /tmp/nvim && pkg build /tmp/nvim x86_64/   # in flint-packages
```

## Running your own repository

A remote repository is a directory served over HTTPS (or HTTP) that holds the packages,
`INDEX` and `INDEX.sig`.

```sh
pkg key new flint ~/keys                     # once: flint.key (secret!) and flint.pub
cp ~/keys/flint.pub image/etc/flint/keys/    # images built from now on trust it

pkg build SRC repo/x86_64/
pkg index repo/x86_64/
pkg sign repo/x86_64/ ~/keys/flint.key       # sign again after every index
```

The rest comes from the build machine:

```sh
image/build-system-packages.sh repo/x86_64/                              # base system, for flint-update
FLINT=$PWD ../flint-packages/ports/kernels/build.sh zen /tmp/zen     # zen, lts, upstream
pkg build /tmp/zen repo/x86_64/
```

## How it stays safe

- **Signed lists.** A remote `INDEX` is used only if `INDEX.sig` is a valid Ed25519 signature
  by a key in `/etc/flint/keys/`. Trust comes from that signature, not from the TLS certificate.
- **No rollback.** An `INDEX` older than the last one accepted is refused (the signed header
  carries a timestamp). A mirror cannot hand out an old list with old packages.
- **Checked packages.** `INDEX` records each package's size and SHA-256. A package that doesn't
  match is refused.
- **Safe paths.** Every path is checked before anything is written. Absolute paths and `..` are
  rejected. Nothing is written below a symlink from the same package, or through a symlink that
  leads out of the install root.
- **Conflicts.** A file owned by another package is a conflict. So is an existing file that
  belongs to no package (`--force` overrides only this second case).
- **Atomic writes.** Each file is written under a temporary name and renamed into place. A fresh
  install that fails halfway is rolled back. The database record is written last.
- **Database.** There is one text file per installed package in `/var/lib/pkg/installed/`,
  listing every file with its mode and SHA-256. `/var/lib/pkg/lock` stops two `pkg` runs from
  changing the system at once.

The package format itself is a plain ustar archive with a `.FLINTPKG` control entry first.
