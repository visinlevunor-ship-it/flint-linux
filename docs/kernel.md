# The Flint kernel

Flint runs on Linux today. The Flint kernel (`kernel/`) is Flint's own kernel: written in
Rust, started by flint-boot like Linux, and running ordinary Linux x86-64 programs through
Linux's system calls, so Flint's userland (and other Linux programs, static or built for glibc)
run on it unchanged.

**Version 0.3 runs Flint's shell and programs with disks, USB keyboards and wired
networking**, keeps your files across restarts, and runs threaded and dynamically linked
programs. It is still experimental: no Wi-Fi, no GPU acceleration, no /proc yet. Linux stays
the kernel Flint boots by default.

## What 0.3 does

- **Boot**: a UEFI program. flint-boot loads it like Linux's kernel and hands it the command
  line and the base image (LoadFile2 on Linux's initrd media path). It takes the framebuffer
  and the memory map from UEFI, then leaves UEFI (ExitBootServices).
- **Memory**: its own page tables. All physical memory is mapped in the upper half; each
  process gets its own lower half. Stacks grow on demand.
- **Processes and threads**: fork, vfork, clone with threads (shared memory, files, signal
  handlers; CLONE_SETTLS, CLONE_CHILD_CLEARTID), futexes, execve (also `#!` scripts), exit,
  wait4/waitid, process groups and sessions. A timer (PIT, 100 Hz) gives each a 50 ms slice.
- **Signals**: handlers with Linux's signal frame (siginfo, ucontext, FPU state), sigaltstack,
  masks, SA_RESTART (or EINTR) for interrupted calls, SIGCHLD, Ctrl-C to the foreground
  program, a fault ends only the program that caused it.
- **Programs built for glibc**: PT_INTERP is honoured, so flint-glibc's loader runs dynamically
  linked programs.
- **Disks**: NVMe, SATA (AHCI) and virtio, with GPT partitions. The boot partition (FAT32) is
  read and written; the **Flint data** partition (ext4) is read, so installed packages and
  their files are there.
- **Files that stay**: the base image, the data partition and a writable layer on top, like an
  overlay mount. Changes are saved to the boot partition (`\FLINT\STATE.IMG`) about two
  seconds after you make them, on `sync`, and before a restart or power-off, and come back at
  the next boot. `/dev`, `/tmp`, `/run`, `/proc` and `/sys` are not kept.
- **USB** (xHCI): keyboards, and network adapters and phones (below), plugged straight into
  the computer's ports (hubs are not supported yet).
- **Networking**:
  - Cards: Intel e1000/e1000e (most Intel wired cards, and what QEMU and VirtualBox emulate),
    virtio-net, and over USB: CDC ECM, CDC NCM and RNDIS, which covers USB tethering from a
    phone and many USB network adapters.
  - IPv4 with ARP, ICMP (it answers pings), UDP and TCP (retransmission, flow control, a clean
    close), and loopback (127.0.0.1).
  - A DHCP client in the kernel: it configures the card at boot and writes
    `/run/resolv.conf` and the status `net` shows. `ip=ADDR/BITS,GATEWAY,DNS` on the command
    line sets the address by hand, `ip=off` leaves the card alone.
  - Sockets: TCP, UDP, ping sockets (ICMP), raw IPv4, and unix sockets (stream, datagram,
    seqpacket; socketpair, and bind/listen/connect by path or abstract name). poll and select
    on all of them, nonblocking connect, SO_RCVTIMEO/SO_SNDTIMEO, the ifconfig ioctls.
  - `ping`, `fetch`, name lookups and other programs using sockets work.
    `netd` sees that the kernel does the work and stays idle; `net` shows the card.
- **Terminal**: the screen (UEFI framebuffer, 8x16 font, the escape sequences line editors
  use) and COM1. Keyboards: PS/2 (i8042, how most laptops connect their own keyboard), USB,
  and the serial port, with Linux's termios: line editing, echo, raw mode, Ctrl-C, Ctrl-\\,
  Ctrl-D.
- **Clock**: the real-time clock for the date, the TSC (measured against the PIT) for time.
- **System calls**: about 170 of Linux's. Anything else answers ENOSYS and is named once on
  the console.

## Trying it

```sh
kernel/build.sh            # needs the rust-src component; builds core and alloc for UEFI
image/build-disk.sh        # adds "Flint kernel (experimental)" to the boot menu
```

Choose **Flint kernel (experimental)** in the boot menu: Flint's shell starts on the screen
(and on the serial port). With a cable plugged in (or a phone sharing its connection over
USB), the network comes up by itself; `net` shows the address. `init=PROGRAM,ARG,...` on the
command line (an entry's `options` line) starts something else first. Leaving that first
program (`exit`) saves your files and stops the machine.

## Not there yet

- **Wi-Fi and GPU acceleration.** A Wi-Fi driver like rtw89 or a GPU driver like amdgpu is
  hundreds of thousands of lines; writing them again is not realistic. The proven way is a
  compatibility layer that builds Linux's own driver sources for another kernel (FreeBSD runs
  amdgpu and Intel Wi-Fi this way, through LinuxKPI). Until then: a cable, a USB network
  adapter, or USB tethering, and the plain framebuffer.
- **A desktop and Firefox.** They also need /dev/dri (or /dev/fb0), input devices and shared
  memory for the X server; the sockets X clients use are there.
- **/proc and /sys**, so `ps`, `free` and Flint's init and devd work as on Linux.
- **More**: USB hubs and storage, writing to the data partition, other network cards (Realtek
  r8169), IPv6, more than one processor.
