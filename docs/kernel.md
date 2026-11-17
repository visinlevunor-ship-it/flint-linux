# The Flint kernel

Flint runs on Linux today. The Flint kernel (`kernel/`) is the start of Flint's own kernel:
written in Rust, started by flint-boot like Linux, and running ordinary Linux x86-64 programs
through Linux's system calls, so Flint's userland (and other static Linux programs) run on it
unchanged.

**Version 0.1 is an early step, not a replacement yet.** It boots, takes over the machine and
runs a list of programs from the base image one after another, then stops. Linux stays the
kernel Flint boots by default.

## What 0.1 does

- **Boot**: a UEFI program. flint-boot loads it like Linux's kernel and hands it the command
  line and the base image (LoadFile2 on Linux's initrd media path). It takes the framebuffer
  and the memory map from UEFI, then leaves UEFI (ExitBootServices).
- **Memory**: its own page tables. All physical memory is mapped in the upper half; each
  program gets its own lower half. Physical pages come from the UEFI memory map; a 16 MiB
  heap for the kernel.
- **CPU**: its own GDT/TSS, an interrupt table for the CPU's exceptions (a program's page fault
  ends that program with a message, as SIGSEGV would), and the `syscall` instruction.
- **Programs**: static ELF executables (and static-PIE), loaded with the stack Linux lays out
  (argc, argv, envp, auxv with AT_PHDR, AT_RANDOM and the rest). One runs at a time.
- **Files**: the base image (cpio) as a read-only root filesystem, with directories and
  symbolic links. Writes to it answer EROFS.
- **Console**: the screen (UEFI framebuffer, 8x16 font from DejaVu Sans Mono) and COM1.
- **System calls**: read, write, open/openat, close, stat/lstat/fstat/newfstatat, lseek,
  pread64, readv/writev, sendfile, getdents64, readlink, access, getcwd, chdir, dup/dup2/dup3,
  fcntl, ioctl (terminal size), mmap/munmap/brk, execve, exit, uname, arch_prctl,
  clock_gettime, nanosleep, getrandom, prlimit, sysinfo and the identity calls. Anything else
  answers ENOSYS and is named once on the console.

## Trying it

```sh
kernel/build.sh            # needs the rust-src component; builds core and alloc for UEFI
image/build-disk.sh        # adds "Flint kernel (experimental)" to the boot menu
```

Choose **Flint kernel (experimental)** in the boot menu. It runs `uname -a`,
`cat /etc/os-release`, `ls -l /`, a shell command and `echo`. Other programs:
`flint.run=PROG,ARG,ARG;PROG,ARG` on the command line (an entry's `options` line).

## What comes next

In order:

1. **Processes**: fork, wait, pipes, a scheduler with a timer (APIC), so `/init`, `login` and
   `sh` run.
2. **Keyboard**: i8042 (PS/2, the keyboard of most laptops) and the serial port, so the shell
   is interactive.
3. **Storage**: NVMe and AHCI, and a filesystem for the data partition.
4. **USB** (xHCI): keyboards, mice, sticks.
5. **Networking**: a NIC driver and a TCP/IP stack.
6. **Linux drivers for the big hardware.** A GPU driver like amdgpu or Wi-Fi like rtw89 is
   millions of lines; writing them again is not realistic. The proven way is a compatibility
   layer that builds Linux's own driver sources for another kernel (FreeBSD runs amdgpu and
   Intel Wi-Fi this way, through LinuxKPI).
7. **flint-glibc** on the Flint kernel, for Firefox and the other glibc programs.
