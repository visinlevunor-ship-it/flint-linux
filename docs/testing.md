# Testing

```sh
cargo test --workspace                          # unit + integration tests (Linux host)
(cd boot && cargo test)                         # boot menu parsing
tests/pty/jobcontrol.py target/debug/flint      # interactive sh on a real pty
tests/pty/lineedit.py   target/debug/flint
KERNEL=/path/to/bzImage tests/qemu/session.py   # full session over the serial console
```

## What the tests cover

- **`cargo test`** runs the unit tests, plus integration tests that run the real `flint` and
  `pkg` binaries. The integration tests cover:
  - hostile packages
  - remote repositories on a local HTTP server: a tampered index, a rollback, a swapped
    package, an untrusted key
  - the standards' own test vectors for SHA-1, SHA-512, HMAC, PBKDF2, the 802.11 PRF, AES key
    wrap, AES-GCM, X25519, Ed25519, HKDF and the TLS 1.3 key schedule (RFC 8448); the AES-NI
    and carry-less multiply paths are also checked against the software ones
  - a full WPA2 handshake against a simulated access point, including a wrong password
- **The pty tests** drive the interactive shell on a real pseudo-terminal.
- **`session.py`** boots the image and checks:
  - login, privilege dropping, `su` and `passwd`
  - the tools and shell job control
  - `pkg`
  - poweroff

  QEMU without KVM is slow; set `SLOW=3` to stretch the timeouts.

`cargo test` needs a Linux host.

## Running Flint in QEMU

```sh
image/build-initramfs.sh
KERNEL=/path/to/bzImage image/run-qemu.sh       # Ctrl-A then X quits
```

The dev image has two accounts, `root` and `flint`, both with the password `flint`. For a root
shell without logging in, build with `SERVICES=image/etc/flint/services.qemu-shell`.

## Tested by hand, per release

Every release was also checked in QEMU with OVMF.

**1.0** (everything on flint-libc, no musl):
- the whole workspace test suite built for `x86_64-unknown-linux-musl` and linked against
  flint-libc: 294 tests, including the end-to-end ones that run the real `pkg`, `sh` and tools
- a std program exercising the heap, threads, mutexes, thread-locals, `catch_unwind`, panics
  in threads, processes, `realpath`, TCP and DNS
- flint-libc's math against glibc's libm: within 2 ulp over 1.4 million inputs
- the USB image booting with init and every program on flint-libc: persistence across
  reboots (e2fsck clean), drivers from the boot partition, networking, `pkg install` over HTTPS
- `session.py`: 54/54

**0.8:**
- the first boot from a USB stick creating its data partition
- files surviving reboots, with e2fsck finding the data partition clean after shutdown
- `flint-install` in free space next to a FreeBSD-style layout (the existing partitions were
  byte-for-byte unchanged)
- `flint-install` on a whole disk, then booting the installed system from its UEFI boot entry
- `pkg kernels install/default/remove` with a second kernel
- `flint-update` replacing the base system and drivers
- every port installed from an HTTPS repository and run
- RAM use with drivers on the boot partition: Shmem about 16 MB (148 MB in 0.7)

**0.7:**
- `pkg install fastfetch` from an HTTPS repository
- `fetch` against GitHub

**0.6:**
- the disk image booted from NVMe, SATA, USB and virtio disks
- wired networking with e1000e and rtl8139 network cards
- Wi-Fi with `mac80211_hwsim` against `hostapd`: WPA2, a wrong password, group rekeys
