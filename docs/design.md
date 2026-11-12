# Design

How Flint fits together. For using it, see the [README](../README.md).

## One binary

Every Flint program lives in `/bin/flint`, which picks what to run by the name it was started
as. `sh`, `ls`, `pkg`, `init`, `net`, `ping` and the rest are links to it, and `flint --list`
shows them all. This keeps Flint small on disk and means one build covers everything.

`su` and `passwd` are separate binaries because they are setuid root. `/bin/flint` must never
be setuid, because then it would run every program as root.

## Booting

1. **`flint-boot`** (UEFI) reads `\FLINT\BOOT.CFG` and shows the menu. The menu has a
   timeout, a default entry, reboot and firmware-setup entries. See the comments in
   `image/boot.cfg` for the format.
2. The loader starts the kernel through the firmware's LoadImage/StartImage, so the kernel's
   EFI stub does the rest. The initramfs images (`BASE.IMG` + `EARLY.IMG`) are joined in
   memory and handed to the kernel through the LoadFile2 protocol that Linux looks for.
3. The boot partition's GPT UUID goes on the kernel command line as `flint.esp=`. That is how
   `mountboot` finds the right stick even when several are plugged in.
4. **`init`** runs `/etc/flint/boot` (commands in order), then starts the services in
   `/etc/flint/services`. Services can respawn (with backoff) or run once. Neither file allows
   shell syntax or quoting. `init --check DIR` validates them without booting.

## Drivers

The kernel and modules are Ubuntu's: a well-tested build with drivers for nearly every PC.
`flint-mkimage drivers` picks modules by path from `image/drivers.conf`. It follows their
dependencies, includes the firmware each one declares (the newest version that exists), and
writes `modules.dep` and `modules.alias` itself.

Drivers are split so they cost almost no RAM:

| Where | What | Lines in `drivers.conf` |
|---|---|---|
| `\FLINT\EARLY.IMG` (in RAM, ~6 MB) | what is needed to find the boot partition: storage, USB, FAT, ext4, overlay | `early` |
| `\FLINT\DRIVERS\` (on the boot partition) | every other module, and all firmware | the rest |

Once the root filesystem is set up, `modload --attach` bind-mounts the DRIVERS tree read-only
over `/lib/modules` and `/lib/firmware`. A driver then takes memory only when it is loaded.
With this layout a booted system uses about 16 MB for the unpacked initramfs, down from 148 MB
in 0.7.

Measured in QEMU after boot (`free`): 86 MB used in total. Of that, the base system in RAM is
16 MB, Flint's running programs take 4.7 MB (AnonPages), and nearly all the rest is the kernel:
its code and data, and slab. Real machines load more drivers (amdgpu, Wi-Fi), so they will use
more; this has not been measured yet.

`devd` handles hotplugged devices (USB) and the kernel's own module requests.

Graphics:
- AMD and Intel GPUs get their drivers and firmware.
- NVIDIA's nouveau is left out: without its large GSP firmware it would blank the screen. NVIDIA
  cards keep the firmware framebuffer (`simpledrm`) instead.

## Disks and persistence

- **Boot partition:** FAT32, holding the bootloader, kernels, images and drivers.
- **Data partition:** ext4.
- Both are formatted by Flint's own code (`flint-disk`), which also writes GPT.

On the first boot from a USB stick, the free space on the stick becomes the data partition.
After that, init mounts the data partition and switches to an overlay root:

- **Lower layer:** the base system, from RAM.
- **Upper layer:** the data partition.

Everything you change goes to the data partition. The base system itself is never modified, so
`flint-update` can replace it without touching your data. On shutdown, init remounts the data
partition read-only first, so it is always clean. The `flint.persist=0` boot option (the "live"
menu entry) skips all of this.

## Kernels and updates

- **Kernels** are packages (`linux-zen`, `linux-lts`, `linux-upstream`, built by
  `ports/kernels/build.sh`). `pkg kernels` copies each one to `\FLINT\KERNELS\NAME\`, puts its
  modules beside the others in `\FLINT\DRIVERS\modules\`, and adds a boot menu entry.
- **The base system** is four packages: `flint-system`, `flint-kernel`, `flint-firmware` and
  `flint-gpu-firmware`. `flint-update` upgrades them and every other package, then copies what
  changed to the boot partition. The images are copied before the kernel that needs them.

## Networking

All of the networking code is Flint's own:
- netlink, and a DHCP client on a packet socket
- a DNS resolver
- nl80211 for Wi-Fi
- the WPA2 4-way and group-key handshakes, with SHA-1, PBKDF2 and AES key unwrap

The kernel, or the Wi-Fi card, encrypts the frames. Wired links get a lower route metric than
Wi-Fi, so a cable wins when both are up. Saved Wi-Fi networks live in `\FLINT\NETWORKS` on the
boot partition.

Wi-Fi support: open networks, WPA2-Personal, and WPA2/WPA3 mixed networks (joined with WPA2).
Not supported: WPA3-only, WEP, WPA1, enterprise (802.1X), and WPA2 networks that require
management frame protection.

`fetch` and `pkg` use Flint's own TLS 1.3 client, with AES-128-GCM and X25519. When the CPU
has AES-NI and PCLMULQDQ, encryption runs at about 600 MB/s per core; in software it is about
20 MB/s.

## Accounts

Passwords are hashed with PBKDF2-HMAC-SHA256: 100,000 iterations and 16 random salt bytes.
They are stored in `/etc/shadow` (mode 0600) as
`$flint1$<iterations>$<salt hex>$<hash hex>`. An empty field, or one starting with `!` or `*`,
is a locked account.

## Own libc, in stages

For now the std crates link musl statically. `flint-rt` is its replacement: raw syscalls,
`_start`, a heap, `mem*` and panics. It is not a C library and cannot run C programs; it exists
for Flint's own Rust programs. 1.0 is planned for when the userland no longer needs musl.

## Versioning

`VERSION` holds `MAJOR.MINOR`:
- A rewrite of a component, or any major architectural change, bumps MAJOR.
- Everything else bumps MINOR.

`flint-version` prints the version with the git hash.

## Source layout

```
crates/flint-apps      the flint binary: dispatches to every program by name
crates/flint-core      shared code
crates/flint-auth      accounts and password hashes
crates/flint-init      init (PID 1)
crates/flint-power     poweroff / reboot / halt
crates/flint-login     getty + login
crates/flint-passwd    passwd (setuid)
crates/flint-su        su (setuid)
crates/flint-sh        shell
crates/flint-box       core tools
crates/flint-pkg       package manager
crates/flint-gzip      DEFLATE and gzip
crates/flint-disk      GPT, FAT32 and ext4 formatting
crates/flint-setup     flint-install, flint-update
crates/flint-dev       driver loading, mountboot, the persistent root
crates/flint-net       netd, net, ping: DHCP, Wi-Fi (nl80211, WPA2)
crates/flint-crypto    SHA-512, X25519, Ed25519, AES-GCM, HKDF
crates/flint-web       DNS, TLS 1.3 client, HTTP downloads, fetch
crates/flint-mkimage   host tool: initramfs, drivers, disk images
boot/                  flint-boot, the UEFI bootloader (separate workspace)
rt/                    flint-rt and libc-free programs (separate workspace)
packages/              example package sources
ports/                 recipes for third-party programs
image/                 build scripts, boot menu, driver list, default /etc
tests/                 pty and QEMU end-to-end tests
```

The `.cargo/config.toml` files carry the linker flags the musl cross-build needs. They matter
when the host's `cc` is not GNU, as on FreeBSD.
