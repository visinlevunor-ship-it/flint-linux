<div align="center">

```
                                      .     '
                                   /    .'
                    .====.        /   .'     _.'
                .=#@@@ @@@%=.    /  .'   _.-'
            .=+@@@@@@= +@@@#==. / .' _.-'  __.-'
          .@@@@@@@@@%  +=====+. ..-'' __.--''
         =##  %@@@@@% ==+++++= :---'''   ----
        =####  %@@@#  =+++++++  `-._
       =######  #@  =++++++++=      `-._   '
      =########   ====++++++            `-.
     =##+=======  =========
    .====+######  ========
    =###########+  ======
     =###########  ====
     .###########=  ==
      =###########  =
       =#######+==
        ++==
```

# Flint Linux

**A Linux distro with its entire userland written from scratch in Rust.**

The kernel is Linux. Everything else (bootloader, init, shell, core tools,
package manager, networking, TLS, installer) is Flint's own code.

[![Patreon](https://img.shields.io/badge/Patreon-support%20Flint-f96854?logo=patreon&logoColor=white)](https://www.patreon.com/c/FlintLinux/membership)
![version](https://img.shields.io/badge/version-0.8-orange)
![status](https://img.shields.io/badge/status-early%20development-yellow)

</div>

---

## Why Flint

- **Tiny.** About 16 MB of RAM after boot. Every program is one static binary, `/bin/flint`.
- **Boots anywhere.** Its own UEFI bootloader, and drivers load automatically for whatever
  hardware it finds, including AMD and Intel graphics.
- **Online by itself.** Wired networks work with no setup, and Wi-Fi works with WPA2. Flint
  has its own DHCP, DNS, WPA2 and TLS 1.3 code, with hardware AES.
- **Your USB stick keeps your stuff.** Packages, files and settings survive reboots.
- **Installs next to your other OS**, or takes over the whole disk.
- **Updates in one command.** `flint-update` updates the system and every package.
- **Signed packages.** Packages come over HTTPS, and the package lists are Ed25519-signed.
  Kernels are packages too: `zen`, `lts` and `upstream`.

## Try it

You need a 64-bit PC with UEFI, with **Secure Boot turned off**.

**1. Build the image.** This works on FreeBSD or Linux.

```sh
rustup target add x86_64-unknown-linux-musl x86_64-unknown-uefi
cargo build --release --workspace --target x86_64-unknown-linux-musl
(cd rt   && cargo build --release --target x86_64-unknown-linux-musl)
(cd boot && cargo build --release --target x86_64-unknown-uefi)
cargo build --release -p flint-mkimage -p flint-pkg

image/fetch-kernel.sh     # Linux 7.0 + firmware, checksummed
image/build-disk.sh       # -> target/flint.img
```

**2. Write it to a USB stick.** This erases the stick.

```sh
dd if=target/flint.img of=/dev/da0 bs=1m    # FreeBSD (geom disk list)
dd if=target/flint.img of=/dev/sdX bs=1M    # Linux   (lsblk)
```

**3. Boot from the stick.** The boot menu key is usually F12, F10 or Esc.
Log in as `root` with the password `flint`, then change the password right away with `passwd`.

Or try it in a VM:

```sh
qemu-system-x86_64 -m 1G -machine q35 -bios /path/to/OVMF.fd -drive file=target/flint.img,format=raw
```

## Using it

```sh
net wifi scan                     # Wi-Fi (a cable just works)
net wifi connect "My Network"
net                               # status

pkg install neovim tmux git       # install software
pkg upgrade                       # upgrade packages
flint-update                      # update everything, base system included

pkg kernels                       # list kernels
pkg kernels install zen           # add one (it gets its own boot menu entry)
pkg kernels default zen           # make it the default

flint-install                     # put Flint on a disk
```

The repository currently has **neovim, tmux, git, curl, nano, micro, htop, btop,
fastfetch** and **ca-certificates**. They are all static builds from [`ports/`](ports/).

`flint-install` asks which disk to use, then gives you two choices:
- **Free space:** Flint adds two partitions and doesn't touch the existing ones, so it can
  sit next to FreeBSD or Windows.
- **Whole disk:** the disk is erased, after you type its name to confirm.

The boot menu is `\FLINT\BOOT.CFG` on the stick, a plain text file you can edit on any computer.
To boot without saving anything to the stick, pick **live** in the boot menu.

## What's inside

| | |
|---|---|
| **Boot** | `flint-boot` (UEFI loader with a menu) · `init` (PID 1, supervises services) · `modload`/`devd` (drivers, hotplug) |
| **Shell & tools** | `sh` (job control, history, Tab, Ctrl-R) · `ls cat grep sed awk find vi less` and more (`flint --list`) |
| **Users** | `login` · `su` · `passwd` (PBKDF2 password hashes) |
| **Network** | `netd` (DHCP, Wi-Fi, WPA2) · `net` · `ping` · `fetch` (HTTP/HTTPS) |
| **System** | `pkg` · `flint-install` · `flint-update` · `gzip` |
| **Crypto** | SHA-2, HMAC, PBKDF2, AES-GCM (AES-NI), X25519, Ed25519, HKDF, TLS 1.3, all in pure Rust |

## Status

**0.8, early development.** Flint is tested in QEMU, and reports from real hardware are very welcome.

Not there yet:
- WPA3-only and enterprise Wi-Fi.
- IPv6 address setup.
- NVIDIA graphics beyond the basic framebuffer.
- `sh` has no `trap`, aliases, `set -e`, arrays or `[[ ]]`.
- Secure Boot.

## Docs

- [Design](docs/design.md): how booting, drivers, persistence, networking and the one-binary layout work
- [Packages](docs/packages.md): `pkg`, running your own signed repository, how packages are kept safe
- [Ports](ports/README.md): building third-party software for Flint
- [Testing](docs/testing.md): unit, pty and QEMU tests

## Support

If you like where Flint is going, you can back it on
[Patreon](https://www.patreon.com/c/FlintLinux/membership).

## License

MIT
