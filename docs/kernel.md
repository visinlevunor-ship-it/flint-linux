# The Flint kernel

Flint runs on Linux today. The Flint kernel (`kernel/`) is the start of Flint's own kernel:
written in Rust, started by flint-boot like Linux, and running ordinary Linux x86-64 programs
through Linux's system calls, so Flint's userland (and other static Linux programs) run on it
unchanged.

**Version 0.2 runs Flint's shell interactively**, with processes, pipes and a writable RAM
filesystem. It is still early: no disks, network, USB or GPU drivers yet. Linux stays the
kernel Flint boots by default.

## What 0.2 does

- **Boot**: a UEFI program. flint-boot loads it like Linux's kernel and hands it the command
  line and the base image (LoadFile2 on Linux's initrd media path). It takes the framebuffer
  and the memory map from UEFI, then leaves UEFI (ExitBootServices).
- **Memory**: its own page tables. All physical memory is mapped in the upper half; each
  process gets its own lower half. Stacks grow on demand.
- **Processes**: fork, vfork, clone (without threads), execve (also `#!` scripts), exit,
  wait4/waitid, process groups and sessions. Each process has its own kernel stack; a timer
  (PIT, 100 Hz) gives each a 50 ms slice. Signals have their default actions: Ctrl-C ends
  the foreground program, kill works, a fault ends only the program that caused it. Signal
  handlers are not called yet.
- **Files**: the base image (cpio) with a writable layer in RAM on top, like an overlay mount:
  create, write, append, truncate, mkdir, rm, rename, symlinks, chmod. `/dev/null`, `zero`,
  `random`, `urandom`, `tty`, `console`. Pipes, dup, fcntl, poll and select.
- **Terminal**: the screen (UEFI framebuffer, 8x16 font, the cursor and erase sequences line
  editors use) and COM1. Input from the keyboard (i8042: PS/2, which is how most laptops
  connect their own keyboard) and the serial port, with Linux's termios: line editing, echo,
  raw mode, Ctrl-C, Ctrl-\\, Ctrl-D.
- **Clock**: the real-time clock for the date, the TSC (measured against the PIT) for time.
- **System calls**: about 130 of Linux's. Anything else answers ENOSYS and is named once on
  the console.

## Trying it

```sh
kernel/build.sh            # needs the rust-src component; builds core and alloc for UEFI
image/build-disk.sh        # adds "Flint kernel (experimental)" to the boot menu
```

Choose **Flint kernel (experimental)** in the boot menu: Flint's shell starts on the screen
(and on the serial port). Everything you create lives in RAM and is gone after a restart.
`init=PROGRAM,ARG,...` on the command line (an entry's `options` line) starts something else
first. Leaving that first program (`exit`) stops the machine.

## What comes next

In order:

1. **Signal handlers and threads** (clone with CLONE_VM, futex), so more programs run.
2. **Flint's own init**: /proc, /sys and /dev the way Flint's init and devd expect them.
3. **Storage**: NVMe and AHCI, and a filesystem for the data partition.
4. **USB** (xHCI): keyboards, mice, sticks.
5. **Networking**: a NIC driver and a TCP/IP stack.
6. **Linux drivers for the big hardware.** A GPU driver like amdgpu or Wi-Fi like rtw89 is
   millions of lines; writing them again is not realistic. The proven way is a compatibility
   layer that builds Linux's own driver sources for another kernel (FreeBSD runs amdgpu and
   Intel Wi-Fi this way, through LinuxKPI).
7. **flint-glibc** on the Flint kernel, for Firefox and the other glibc programs.
