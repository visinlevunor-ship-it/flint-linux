# Flint Linux

A small Linux distribution with a from-scratch userland written in Rust. Only the kernel and its modules are upstream; the init system, login, shell, core tools, package manager and account handling are Flint's own.

[![Support Flint on Patreon](https://img.shields.io/badge/Patreon-support%20Flint-f96854?logo=patreon&logoColor=white)](https://www.patreon.com/c/FlintLinux/membership)

**Status: 0.8, early development.** Flint boots on real PCs and laptops from a USB stick with its own UEFI bootloader, loads the drivers for whatever hardware it finds (graphics included), and gets on the network by itself: wired with DHCP, or Wi-Fi with WPA2. The USB stick keeps everything you change; `flint-install` puts Flint on a disk, next to other systems or on its own, and `flint-update` keeps it current. Packages install over HTTPS from signed repositories: Flint's own (`pkg install neovim`) and other kernels (`pkg kernels install zen`). Every program in the base system is Flint's own.

## What works today

Every Flint program is one binary, `/bin/flint`, installed under normal names: `sh`, `ls`, `pkg`, `init`, `net`, `ping` and the rest are links to it (`flint --list` shows them all). Only `su` and `passwd` are separate, because they are setuid root.

| Part | What it is | State |
|---|---|---|
| `flint-boot` | UEFI bootloader: menu from `\FLINT\BOOT.CFG`, timeout, reboot and firmware-setup entries, loads the kernel and initramfs images | tested in QEMU (OVMF) |
| `init` | PID 1: early mounts, boot tasks, service supervision with respawn backoff, clean shutdown | tested in QEMU |
| `modload`, `devd`, `modprobe`, `lsmod` | driver loading: every device present at boot, hotplugged ones (USB), and the kernel's own module requests | tested in QEMU, Linux 7.0 |
| `mountboot` | mounts the USB stick or disk Flint booted from at `/boot` (found by GPT partition UUID) | tested in QEMU |
| `netd` | network daemon: DHCP on wired interfaces, joins saved Wi-Fi networks, WPA2 handshake and rekeys, DNS servers | tested in QEMU (wired, simulated Wi-Fi) |
| `net`, `ping` | network status, `net wifi scan / connect / list / forget`; ping with Flint's own DNS resolver | tested in QEMU |
| `login`, `su`, `passwd` | getty + login, privilege switching, password changes | tested in QEMU |
| `sh` | the shell: POSIX-style scripting, pipelines, redirections, job control; line editor with history, Ctrl-R search and Tab completion | tested in QEMU and on a pty |
| `pkg` | package manager: dependencies, upgrades, config-file protection, verify; HTTPS repositories with Ed25519-signed indexes; gzip-compressed packages; kernels as packages | tested in QEMU |
| `flint-install` | installs Flint on a disk: in free space next to other systems, or on the whole disk; adds a UEFI boot entry | tested in QEMU (OVMF, NVMe) |
| `flint-update` | updates the base system (kernel, drivers, firmware, base image, bootloader) and every package | tested in QEMU |
| `gzip`, `gunzip`, `zcat` | Flint's own DEFLATE | compared with GNU gzip |
| `fetch` | downloads over HTTP and HTTPS (Flint's own TLS 1.3 client) | tested in QEMU and against GitHub |
| `poweroff`, `reboot`, `halt` | ask init to shut down (or `-f` to do it directly) | tested in QEMU |
| `ls cat grep sed awk find vi less ...` | core tools, including `grep`/`sed` with full regular expressions, `awk`, `vi` and `less` | tested in QEMU, compared with GNU grep/sed and mawk |
| `flint-mkimage` | host tool: initramfs archives, driver and firmware selection, GPT + FAT32 disk images | unit tested, images checked with sgdisk, fsck.vfat and e2fsck |
| `flint-rt`, `flint-version` | libc replacement for `no_std` programs, and the first program on it | tested in QEMU |

Not written yet: WPA3-only and enterprise Wi-Fi.

## Boot it on a PC

Flint runs on any 64-bit x86 PC or laptop with UEFI (most made since about 2012). **Secure Boot must be off**: Flint is not signed. Hardware support is that of the Linux 7.0 kernel and the firmware that comes with it.

You need Rust (`rustup`), `zstd`, and on the build machine either FreeBSD or Linux.

```sh
rustup target add x86_64-unknown-linux-musl x86_64-unknown-uefi
cargo build --release --workspace --target x86_64-unknown-linux-musl   # the Flint programs
(cd rt && cargo build --release --target x86_64-unknown-linux-musl)    # libc-free programs
cargo build --release -p flint-mkimage -p flint-pkg                     # host tools
(cd boot && cargo build --release --target x86_64-unknown-uefi)         # the bootloader

image/fetch-kernel.sh                                    # Linux 7.0 + firmware (Ubuntu packages, checksummed)
image/build-disk.sh                                      # -> target/flint.img (512 MiB)
```

Write `target/flint.img` to a USB stick (this erases the stick), then boot from it (the boot menu key is often F12, F10 or Esc):

```sh
dd if=target/flint.img of=/dev/da0 bs=1m       # FreeBSD (check the device with: geom disk list)
dd if=target/flint.img of=/dev/sdX bs=1M       # Linux (check with: lsblk)
```

Log in as `root`, password `flint` (change it with `passwd`). A cable is used automatically. For Wi-Fi:

```sh
net wifi scan
net wifi connect "My Network"      # asks for the password, saves the network on the USB stick
net                                # status
ping example.org
```

Saved networks are stored on the USB stick (`/boot/FLINT/NETWORKS`).

On the first boot, Flint turns the free space of the USB stick into a data partition (ext4) and from then on keeps everything you change there: installed packages, your files, settings. The base system itself stays as it came (it is the lower layer of an overlay), so `flint-update` can replace it without touching your data. The boot menu entry "live" (`flint.persist=0`) starts without any of this and changes nothing on the stick.

### Install on a disk

```sh
flint-install               # asks: which disk, free space or whole disk, copy your changes, default boot entry
```

In free space, Flint adds two partitions (boot and data) and leaves every existing one alone, so it sits next to FreeBSD or Windows; pick "Flint Linux" in the firmware boot menu (or let `--default` make it the first entry). On a whole disk, everything on it is erased after you type its name.

### Keep it up to date

```sh
flint-update                # base system + every package; restart to use a new base
pkg kernels                 # kernels: installed and available
pkg kernels install zen     # also lts and upstream; each gets a boot menu entry
pkg kernels default zen     # start it by default
```

Supported Wi-Fi security: open networks, WPA2-Personal, and WPA2/WPA3 mixed networks (joined with WPA2). Not supported yet: WPA3-only, WEP, old WPA1, enterprise (802.1X) networks, and networks that require management frame protection with WPA2.

The boot menu is `\FLINT\BOOT.CFG` on the stick; it is a plain text file you can edit on any computer (see the comments in `image/boot.cfg`). Which drivers and firmware go onto the stick is set by `image/drivers.conf`; its `early` lines pick the few that must be in memory to find the stick (storage, USB, filesystems).

## Quick start (QEMU)

```sh
image/build-initramfs.sh
KERNEL=/path/to/bzImage image/run-qemu.sh          # Ctrl-A then X quits QEMU
```

To boot the full disk image in QEMU with UEFI firmware (OVMF):

```sh
qemu-system-x86_64 -m 1G -machine q35 -bios /path/to/OVMF.fd -drive file=target/flint.img,format=raw
```

The dev image has two accounts, `root` and `flint`, both with the password `flint`. For a root shell without login: `SERVICES=image/etc/flint/services.qemu-shell`.

`.cargo/config.toml` and `rt/.cargo/config.toml` carry the linker flags the musl cross-build needs (needed when the host's `cc` is not GNU, e.g. FreeBSD).

## Packages

```sh
pkg build packages/hello repo/          # NAME-VERSION.fpk from control + files/
pkg index repo/                         # write repo/INDEX (checksums)
pkg --repo repo/ install hello          # also installs greet-data, its dependency
pkg list | info hello | files hello | owns /usr/bin/hello | search greet
pkg upgrade                             # newest versions from the repositories
pkg verify                              # compare files with recorded checksums
pkg remove hello && pkg autoremove
```

Repositories come from `--repo DIR-OR-URL` and from `/etc/flint/pkg.conf` (`repo /path/to/dir` or `repo https://host/path` lines). `--root DIR` installs into another tree, so packages can be tested without root. `-n` shows what would happen. See `packages/README.md` for the control file format.

### Repositories on the internet

The image points at the Flint package repository on GitHub, so on a running system:

```sh
pkg install fastfetch       # fetches the signed package list, downloads, checks, installs
fastfetch
pkg update                  # refresh the package lists by hand (install and upgrade do it too)
```

A remote repository is a directory served over HTTPS (or HTTP) with the packages, `INDEX` and `INDEX.sig`. To publish one:

```sh
pkg key new flint ~/keys            # once: ~/keys/flint.key (secret, keep it off the repo) and flint.pub
cp ~/keys/flint.pub image/etc/flint/keys/   # images built from now on trust it
pkg build SRC repo/x86_64/ && pkg index repo/x86_64/
pkg sign repo/x86_64/ ~/keys/flint.key      # writes INDEX.sig; sign again after every index
```

`ports/` holds recipes that build third-party programs as static binaries: fastfetch, neovim, tmux, git, curl, nano, micro, htop, btop and ca-certificates (`ports/NAME/build.sh OUT && pkg build OUT repo/x86_64/`; see `ports/README.md`).

What else goes into the repository, from the build machine:

```sh
image/build-system-packages.sh repo/x86_64/            # the base system, for flint-update
ports/kernels/build.sh zen /tmp/zen && pkg build /tmp/zen repo/x86_64/   # zen, lts, upstream
```

How it stays safe:

- A package is a plain ustar archive with a `.FLINTPKG` control entry first; the repository `INDEX` records each file's size and SHA-256, and a package that does not match is refused.
- A remote `INDEX` is only used if `INDEX.sig` is a valid Ed25519 signature by a key in `/etc/flint/keys/`, and never if it is older than the last one accepted (a timestamp in the signed header), so a mirror cannot hand out an old list with old packages. HTTPS protects privacy in transit, but the trust comes from the signature: the TLS certificate is not what pkg relies on.
- Paths are checked before anything is written: no absolute paths, no `..`, nothing below a symlink from the same package, and nothing written through a symlink that leads out of the install root.
- A file owned by another package, or an existing file that belongs to no package, is a conflict (`--force` overrides the second).
- Each file is written to a temporary name and renamed into place; a fresh install that fails half way is rolled back; the database record is written last.
- Config files listed in `config:` that the user edited are never overwritten: an upgrade saves the new version as `FILE.flint-new`, and a removal keeps the file.

## Testing

```sh
cargo test --workspace                          # unit and integration tests; needs a Linux host
(cd boot && cargo test)                         # boot menu parsing (runs on the host)
tests/pty/jobcontrol.py target/debug/flint      # interactive sh on a real pty (Linux)
KERNEL=/path/to/bzImage tests/qemu/session.py   # full QEMU session over the serial console
```

- `cargo test` runs the unit tests plus integration tests that execute the real `flint` and `pkg` binaries. They include hostile packages, remote repositories over a local HTTP server (tampered index, rollback, swapped package, untrusted key), the cryptography test vectors from the standards (SHA-1, SHA-512, HMAC, PBKDF2, the 802.11 PRF, AES key wrap, AES-GCM, X25519, Ed25519, HKDF, the TLS 1.3 key schedule of RFC 8448), and a full WPA2 handshake against a simulated access point, including a wrong password.
- `tests/pty/lineedit.py` and `tests/pty/jobcontrol.py` drive the interactive shell on a real pseudo-terminal.
- `tests/qemu/session.py` boots the image and checks login, privilege dropping, `su`, `passwd`, the tools, shell job control, `pkg` and poweroff. QEMU without KVM is slow; set `SLOW=3` to stretch its timeouts.
- For 0.8, in QEMU with OVMF: the first boot from a USB stick making its data partition, files surviving reboots (and e2fsck finding it clean after shutdown), `flint-install` in free space next to a FreeBSD-style layout (existing partitions byte-for-byte unchanged) and on a whole disk, booting the installed system from its UEFI boot entry, `pkg kernels install/default/remove` with a second kernel, `flint-update` replacing the base system, and every port installed from an HTTPS repository and run. With drivers and firmware on the boot partition, a booted system uses about 16 MB of RAM for the unpacked initramfs (148 MB in 0.7's layout).
- For 0.7, `pkg install fastfetch` was tested in QEMU against an HTTPS repository, and `fetch` against GitHub.
- For 0.6 the disk image was also booted under OVMF from NVMe, SATA, USB and virtio disks; wired networking was tested with e1000e and rtl8139 network cards; Wi-Fi with `mac80211_hwsim` against `hostapd` (WPA2, wrong password, group rekeys).

## Layout

```
crates/flint-apps      the flint binary: dispatches to every program by name
crates/flint-core      shared code
crates/flint-auth      accounts and password hashes (pure Rust)
crates/flint-init      init (PID 1)
crates/flint-power     poweroff / reboot / halt
crates/flint-login     getty + login
crates/flint-passwd    passwd (setuid)
crates/flint-su        su (setuid)
crates/flint-sh        shell
crates/flint-pkg       package manager
crates/flint-gzip      DEFLATE and gzip
crates/flint-disk      GPT, FAT32 and ext4 formatting
crates/flint-setup     flint-install, flint-update
crates/flint-box       core tools
crates/flint-dev       driver loading, mountboot, the persistent root
crates/flint-net       netd, net, ping: DHCP, Wi-Fi (nl80211, WPA2)
crates/flint-crypto    SHA-512, X25519, Ed25519, AES-GCM, HKDF (pure Rust)
crates/flint-web       DNS, TLS 1.3 client, HTTP downloads, fetch
crates/flint-mkimage   host tool: initramfs, drivers, disk images
boot/                  separate workspace: flint-boot, the UEFI bootloader
rt/                    separate workspace: flint-rt and libc-free programs
packages/              example package sources
ports/                 recipes that build third-party programs into package sources
image/                 build scripts, boot menu, driver list, default /etc files
tests/                 pty and QEMU end-to-end tests
VERSION                the distro version
```

## Design notes

- **Version.** `VERSION` holds `MAJOR.MINOR`. A rewrite of a component or any major architectural change bumps MAJOR; everything else bumps MINOR. `flint-version` prints it with the git hash. 1.0 is planned for when the userland no longer depends on musl.
- **One binary.** `/bin/flint` holds every program and picks one by the name it was started as. Small on disk, one build, and nothing named `flint-*` to remember. It must never be setuid: it would run every program as root. `su` and `passwd` are separate for that reason.
- **Booting.** `flint-boot` loads the kernel with the firmware's LoadImage/StartImage, so the kernel's EFI stub does the rest. The initramfs images are joined in memory and handed over through the LoadFile2 protocol that Linux looks for. The boot partition's GPT UUID goes on the command line as `flint.esp=`, which is how `mountboot` finds the right USB stick even with several plugged in.
- **Drivers.** The kernel and modules are Ubuntu's (a well-tested build with drivers for nearly every PC). `flint-mkimage drivers` picks modules by path, follows their dependencies, and includes the firmware each one declares, choosing the newest version that exists; it generates `modules.dep` and `modules.alias` itself. Only the drivers needed to reach the boot partition are in memory (`EARLY.IMG`, about 6 MB); the rest, and all firmware, stay in `\FLINT\DRIVERS` on the boot partition and are mounted read-only over `/lib/modules` and `/lib/firmware` once the root is set up (`modload --attach`), so they cost no RAM until loaded. AMD and Intel graphics drivers come with their firmware; NVIDIA's nouveau is left out, because without its large GSP firmware it would blank the screen, so NVIDIA cards keep the firmware framebuffer (`simpledrm`).
- **Disks.** The boot partition is FAT32 (the bootloader, kernel and images), the data partition ext4, both formatted by Flint's own code (`flint-disk`). At boot, init mounts the data partition and switches to an overlay: the base system from RAM below (about 16 MB, with the drivers left on the boot partition), the data partition above. Shutdown remounts the data partition read-only, so it is always clean.
- **Networking.** All Flint's own code: netlink, a DHCP client on a packet socket, a DNS resolver, nl80211 for Wi-Fi, and the WPA2 4-way and group key handshakes with SHA-1, PBKDF2 and AES key unwrap. The kernel (or the Wi-Fi card) does the frame encryption. Wired links get a lower route metric than Wi-Fi, so a cable wins when both are up.
- **Own libc, in stages.** The std crates currently link musl statically. `flint-rt` is the replacement: raw syscalls, `_start`, a heap, `mem*`, panics. It is not a C library, so it does not run C programs; it exists for Flint's own Rust programs.
- **Passwords.** PBKDF2-HMAC-SHA256, 100,000 iterations, 16 random salt bytes, stored as `$flint1$<iterations>$<salt hex>$<hash hex>` in `/etc/shadow` (mode 0600). A field that is empty or starts with `!` or `*` is a locked account.
- **Init configuration.** `/etc/flint/services` (supervised services: name, respawn or once, tty, command) and `/etc/flint/boot` (commands run in order before services). No shell and no quoting in either file. `init --check DIR` validates them without booting.
- **Package database.** One text file per installed package in `/var/lib/pkg/installed/`, listing every file with its mode and SHA-256. `/var/lib/pkg/lock` keeps two `pkg` runs from changing the system at once.

## Known limitations

- x86-64 with UEFI only; Secure Boot must be off. Tested in QEMU, not yet on a range of real machines: reports are welcome.
- **Wi-Fi:** one network at a time, no WPA3-only or enterprise networks, no IPv6 address configuration yet (IPv6 works on links where the kernel configures it itself).
- **pkg** downloads one package at a time. Flint's TLS uses the CPU's AES and carry-less multiply instructions when present (about 600 MB/s per core); without them AES-GCM runs in software at about 20 MB/s.
- Arch kernels (`zen`, `lts`, `upstream`) were only tested with a stand-in kernel package, not with Arch's own files.
- NVIDIA graphics: firmware framebuffer only.
- **sh** does not support `trap`, aliases, `set -e`/`-u`/`-x`, arrays, `[[ ]]`, process substitution, `${x/a/b}`.
- The image ships a known password. Change it with `passwd` before connecting to networks you do not trust.
- `cargo test` does not run on non-Linux hosts.

## Support

Flint is built in the open. If you want to help it along, you can support it on Patreon: https://www.patreon.com/c/FlintLinux/membership

## License

MIT License
