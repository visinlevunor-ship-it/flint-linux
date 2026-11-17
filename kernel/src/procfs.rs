//! /proc: processes and system information, made up when a file is opened, in Linux's
//! formats (ps, top, free, uptime, htop and fastfetch read them). Writes are accepted and
//! dropped. Also a few files of /sys.

use crate::proc;
use crate::syscall::now_ns;
use crate::vfs::{MemNode, Node};
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};
use core::cell::RefCell;

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const HZ: u64 = 100;

fn node(mode: u32, data: Vec<u8>) -> Node {
    Node::Mem(Rc::new(RefCell::new(MemNode { mode, ino: 0x7000_0000 + data.len() as u64, mtime: crate::vfs::now(), rdev: 0, data })))
}
fn dir() -> Node {
    node(S_IFDIR | 0o555, Vec::new())
}
fn file(text: String) -> Node {
    node(S_IFREG | 0o444, text.into_bytes())
}
fn wfile(text: String) -> Node {
    node(S_IFREG | 0o644, text.into_bytes())
}
fn link(target: String) -> Node {
    node(S_IFLNK | 0o777, target.into_bytes())
}

static mut HOSTNAME: Option<String> = None;

pub fn hostname() -> String {
    // SAFETY: one CPU.
    unsafe { (*core::ptr::addr_of!(HOSTNAME)).clone().unwrap_or_else(|| "flint".to_string()) }
}

pub fn set_hostname(name: &str) {
    // SAFETY: one CPU.
    unsafe { *core::ptr::addr_of_mut!(HOSTNAME) = Some(name.to_string()) };
}

fn kib(pages: u64) -> u64 {
    pages * 4
}

fn meminfo() -> String {
    // SAFETY: counters read on one CPU.
    let (total, used) = unsafe { (crate::mem::TOTAL_PAGES, crate::mem::USED_PAGES) };
    let free = total.saturating_sub(used);
    format!(
        "MemTotal:       {:8} kB\nMemFree:        {:8} kB\nMemAvailable:   {:8} kB\nBuffers:               0 kB\nCached:                0 kB\nSwapCached:            0 kB\nActive:         {:8} kB\nInactive:              0 kB\nShmem:                 0 kB\nSReclaimable:          0 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n",
        kib(total),
        kib(free),
        kib(free),
        kib(used)
    )
}

fn cpu_brand() -> String {
    let mut b = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004].iter().enumerate() {
        let (a, bx, c, d): (u32, u32, u32, u32);
        // SAFETY: cpuid is harmless; rbx is saved around it.
        unsafe {
            core::arch::asm!("mov {t:r}, rbx", "cpuid", "xchg {t:r}, rbx", t = out(reg) bx, inout("eax") *leaf => a, inout("ecx") 0 => c, out("edx") d, options(nostack));
        }
        for (j, r) in [a, bx, c, d].iter().enumerate() {
            b[i * 16 + j * 4..i * 16 + j * 4 + 4].copy_from_slice(&r.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&b).trim_matches(|c: char| c == '\0' || c == ' ').to_string()
}

fn cpuinfo() -> String {
    let mhz = crate::syscall::tsc_hz() / 1_000_000;
    format!(
        "processor\t: 0\nvendor_id\t: x86-64\nmodel name\t: {}\ncpu MHz\t\t: {}.000\ncache size\t: 0 KB\nphysical id\t: 0\nsiblings\t: 1\ncore id\t\t: 0\ncpu cores\t: 1\nflags\t\t: fpu sse sse2\n\n",
        cpu_brand(),
        mhz
    )
}

fn uptime_ns() -> u64 {
    now_ns()
}

fn stat() -> String {
    let up = uptime_ns() / (1_000_000_000 / HZ);
    // SAFETY: a counter read on one CPU.
    let busy = unsafe { proc::BUSY_NS } / (1_000_000_000 / HZ);
    let idle = up.saturating_sub(busy);
    let pids = proc::all_pids();
    let running = pids.iter().filter_map(|p| proc::info(*p)).filter(|i| i.state == 'R').count();
    let btime = crate::syscall::wall_seconds().saturating_sub(uptime_ns() / 1_000_000_000);
    format!(
        "cpu  {busy} 0 0 {idle} 0 0 0 0 0 0\ncpu0 {busy} 0 0 {idle} 0 0 0 0 0 0\nintr 0\nctxt 0\nbtime {btime}\nprocesses {}\nprocs_running {running}\nprocs_blocked 0\n",
        pids.len()
    )
}

fn mounts() -> String {
    let mut s = String::from("rootfs / rootfs rw 0 0\nproc /proc proc rw,nosuid,nodev,noexec 0 0\nsysfs /sys sysfs rw,nosuid,nodev,noexec 0 0\ndevtmpfs /dev devtmpfs rw,nosuid,mode=755 0 0\ndevpts /dev/pts devpts rw,nosuid,noexec,mode=620,ptmxmode=666 0 0\ntmpfs /dev/shm tmpfs rw,nosuid,nodev 0 0\ntmpfs /run tmpfs rw,nosuid,nodev,mode=755 0 0\ntmpfs /tmp tmpfs rw,nosuid,nodev 0 0\n");
    if crate::persist::available() {
        s.push_str("bootfs /boot vfat rw 0 0\n");
    }
    s
}

fn pid_stat(i: &proc::Info) -> String {
    let ticks = i.cpu_ns / (1_000_000_000 / HZ);
    let start = i.start_ns / (1_000_000_000 / HZ);
    let rss = crate::mem::user_pages(i.pml4);
    let tpgid = crate::tty::tty().fg_pgrp;
    format!(
        "{} ({}) {} {} {} {} 1025 {} 4194304 0 0 0 0 {} 0 0 0 20 0 {} 0 {} {} {} 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        i.pid,
        i.name,
        i.state,
        i.ppid,
        i.pgid,
        i.sid,
        tpgid,
        ticks,
        i.threads,
        start,
        rss * 4096,
        rss
    )
}

fn pid_status(i: &proc::Info) -> String {
    let rss = kib(crate::mem::user_pages(i.pml4));
    let state = match i.state {
        'R' => "R (running)",
        'Z' => "Z (zombie)",
        _ => "S (sleeping)",
    };
    format!(
        "Name:\t{}\nUmask:\t0022\nState:\t{state}\nTgid:\t{}\nNgid:\t0\nPid:\t{}\nPPid:\t{}\nTracerPid:\t0\nUid:\t{u}\t{u}\t{u}\t{u}\nGid:\t{g}\t{g}\t{g}\t{g}\nFDSize:\t64\nGroups:\t\nVmPeak:\t{rss} kB\nVmSize:\t{rss} kB\nVmRSS:\t{rss} kB\nRssAnon:\t{rss} kB\nThreads:\t{}\nSigPnd:\t{:016x}\nShdPnd:\t0000000000000000\nSigBlk:\t{:016x}\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\nCpus_allowed:\t1\nCpus_allowed_list:\t0\n",
        i.name,
        i.pid,
        i.pid,
        i.ppid,
        i.threads,
        i.pending,
        i.blocked,
        u = i.uid,
        g = i.gid,
    )
}

/// The node for canonical path `path` ("proc" or "proc/..."), if there is one.
pub fn lookup(path: &str) -> Option<Node> {
    let rest = path.strip_prefix("proc").unwrap_or(path);
    let rest = rest.trim_start_matches('/');
    let parts: Vec<&str> = if rest.is_empty() { Vec::new() } else { rest.split('/').collect() };
    match parts.as_slice() {
        [] => Some(dir()),
        ["self"] => Some(link(format!("{}", proc::current().tgid))),
        ["thread-self"] => Some(link(format!("{}/task/{}", proc::current().tgid, proc::pid()))),
        ["meminfo"] => Some(file(meminfo())),
        ["cpuinfo"] => Some(file(cpuinfo())),
        ["stat"] => Some(file(stat())),
        ["uptime"] => {
            let up = uptime_ns();
            // SAFETY: a counter read on one CPU.
            let idle = up.saturating_sub(unsafe { proc::BUSY_NS });
            Some(file(format!("{}.{:02} {}.{:02}\n", up / 1_000_000_000, up / 10_000_000 % 100, idle / 1_000_000_000, idle / 10_000_000 % 100)))
        }
        ["loadavg"] => {
            let pids = proc::all_pids();
            let running = pids.iter().filter_map(|p| proc::info(*p)).filter(|i| i.state == 'R').count();
            Some(file(format!("0.00 0.00 0.00 {}/{} {}\n", running, pids.len(), pids.last().copied().unwrap_or(1))))
        }
        ["version"] => Some(file(format!("Flint version {} (flint@flint) (rustc) #1 Flint kernel\n", env!("CARGO_PKG_VERSION")))),
        ["cmdline"] => Some(file(format!("{}\n", crate::cmdline()))),
        ["mounts"] => Some(file(mounts())),
        ["filesystems"] => Some(file("nodev\tproc\nnodev\tsysfs\nnodev\ttmpfs\nnodev\tdevtmpfs\nnodev\tdevpts\n\tvfat\n\text4\n".to_string())),
        ["modules"] => Some(file(String::new())),
        ["swaps"] => Some(file("Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n".to_string())),
        ["devices"] => Some(file("Character devices:\n  1 mem\n  4 tty\n  5 /dev/tty\n\nBlock devices:\n".to_string())),
        ["net"] => Some(dir()),
        ["net", "dev"] => Some(file(net_dev())),
        ["net", "route"] => Some(file(net_route())),
        ["sys"] | ["sys", "kernel"] | ["sys", "net"] | ["sys", "net", "ipv4"] | ["sys", "vm"] => Some(dir()),
        ["sys", "kernel", "hostname"] => Some(wfile(format!("{}\n", hostname()))),
        ["sys", "kernel", "osrelease"] => Some(file(format!("{}\n", env!("CARGO_PKG_VERSION")))),
        ["sys", "kernel", "ostype"] => Some(file("Flint\n".to_string())),
        ["sys", "kernel", "pid_max"] => Some(file("4194304\n".to_string())),
        ["sys", "kernel", "ctrl-alt-del"] => Some(wfile("0\n".to_string())),
        ["sys", "kernel", "printk"] => Some(wfile("4\t4\t1\t7\n".to_string())),
        ["sys", "net", "ipv4", "ping_group_range"] => Some(wfile("0\t2147483647\n".to_string())),
        ["sys", "vm", "overcommit_memory"] => Some(wfile("0\n".to_string())),
        [pid, more @ ..] => {
            let pid: i32 = pid.parse().ok()?;
            pid_lookup(pid, more)
        }
    }
}

fn pid_lookup(pid: i32, more: &[&str]) -> Option<Node> {
    let i = proc::info(pid)?;
    Some(match more {
        [] => dir(),
        ["stat"] => file(pid_stat(&i)),
        ["status"] => file(pid_status(&i)),
        ["statm"] => {
            let rss = crate::mem::user_pages(i.pml4);
            file(format!("{rss} {rss} 0 0 0 {rss} 0\n"))
        }
        ["cmdline"] => node(S_IFREG | 0o444, i.cmdline.clone()),
        ["comm"] => wfile(format!("{}\n", i.name)),
        ["exe"] => link(i.exe.clone()),
        ["cwd"] => link(format!("/{}", i.cwd)),
        ["root"] => link("/".to_string()),
        ["environ"] | ["maps"] | ["limits"] | ["mountinfo"] => file(String::new()),
        ["mounts"] => file(mounts()),
        ["oom_score_adj"] => wfile("0\n".to_string()),
        ["fd"] | ["fdinfo"] | ["task"] => dir(),
        ["fd", n] => {
            let n: usize = n.parse().ok()?;
            let (_, path) = i.fds.iter().find(|(k, _)| *k == n)?;
            link(if path.starts_with('/') { path.clone() } else { format!("{path}:[{n}]") })
        }
        ["task", t] => {
            let t: i32 = t.parse().ok()?;
            if !proc::all_tasks_of(pid).contains(&t) {
                return None;
            }
            dir()
        }
        ["task", t, "stat"] if t.parse::<i32>().is_ok() => file(pid_stat(&i)),
        ["task", t, "status"] if t.parse::<i32>().is_ok() => file(pid_status(&i)),
        ["task", t, "comm"] if t.parse::<i32>().is_ok() => wfile(format!("{}\n", i.name)),
        _ => return None,
    })
}

/// The entries of directory `path` under /proc.
pub fn list(path: &str) -> Vec<(String, Node)> {
    let names: Vec<String> = {
        let rest = path.strip_prefix("proc").unwrap_or(path).trim_start_matches('/');
        let parts: Vec<&str> = if rest.is_empty() { Vec::new() } else { rest.split('/').collect() };
        match parts.as_slice() {
            [] => {
                let mut v: Vec<String> = proc::all_pids().iter().map(|p| p.to_string()).collect();
                for n in ["self", "thread-self", "meminfo", "cpuinfo", "stat", "uptime", "loadavg", "version", "cmdline", "mounts", "filesystems", "modules", "swaps", "devices", "net", "sys"] {
                    v.push(n.to_string());
                }
                v
            }
            ["net"] => vec!["dev".into(), "route".into()],
            ["sys"] => vec!["kernel".into(), "net".into(), "vm".into()],
            ["sys", "kernel"] => ["hostname", "osrelease", "ostype", "pid_max", "ctrl-alt-del", "printk"].iter().map(|s| s.to_string()).collect(),
            ["sys", "net"] => vec!["ipv4".into()],
            ["sys", "net", "ipv4"] => vec!["ping_group_range".into()],
            ["sys", "vm"] => vec!["overcommit_memory".into()],
            [pid] if pid.parse::<i32>().is_ok() => ["stat", "status", "statm", "cmdline", "comm", "exe", "cwd", "root", "environ", "maps", "limits", "mounts", "mountinfo", "oom_score_adj", "fd", "fdinfo", "task"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            [pid, "fd"] => proc::info(pid.parse().ok().unwrap_or(-1)).map(|i| i.fds.iter().map(|(n, _)| n.to_string()).collect()).unwrap_or_default(),
            [pid, "task"] => proc::all_tasks_of(pid.parse().ok().unwrap_or(-1)).iter().map(|t| t.to_string()).collect(),
            [_, "task", _] => vec!["stat".into(), "status".into(), "comm".into()],
            _ => Vec::new(),
        }
    };
    names
        .into_iter()
        .filter_map(|n| {
            let full = format!("{}/{}", path, n);
            lookup(&full).map(|node| (n, node))
        })
        .collect()
}

fn net_dev() -> String {
    let (rx, tx, card) = crate::net::with(|n| (n.rx_packets, n.tx_packets, n.has_card()));
    let mut s = String::from("Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n");
    s.push_str("    lo:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0\n");
    if card {
        s.push_str(&format!("  eth0:       0 {rx:7}    0    0    0     0          0         0        0 {tx:7}    0    0    0     0       0          0\n"));
    }
    s
}

fn net_route() -> String {
    let (ip, mask, gw) = crate::net::with(|n| (n.ip, n.mask, n.gw));
    let mut s = String::from("Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n");
    let h = |v: u32| format!("{:08X}", v.swap_bytes());
    if ip != 0 {
        if gw != 0 {
            s.push_str(&format!("eth0\t00000000\t{}\t0003\t0\t0\t0\t00000000\t0\t0\t0\n", h(gw)));
        }
        s.push_str(&format!("eth0\t{}\t00000000\t0001\t0\t0\t0\t{}\t0\t0\t0\n", h(ip & mask), h(mask)));
    }
    s
}

// ---------------------------------------------------------------------------------------
// /sys: just enough for programs that look at the machine's basics

pub fn sys_lookup(path: &str) -> Option<Node> {
    let rest = path.strip_prefix("sys").unwrap_or(path).trim_start_matches('/');
    match rest {
        "" | "class" | "class/net" | "devices" | "kernel" | "class/net/lo" | "class/net/eth0" | "firmware" | "fs" | "block" | "bus" | "module" => {
            if rest == "class/net/eth0" && !crate::net::with(|n| n.has_card()) {
                return None;
            }
            Some(dir())
        }
        "class/net/lo/operstate" => Some(file("unknown\n".to_string())),
        "class/net/eth0/operstate" => Some(file(if crate::net::with(|n| n.link_up()) { "up\n" } else { "down\n" }.to_string())),
        "class/net/eth0/address" => {
            let m = crate::net::with(|n| n.mac());
            Some(file(format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n", m[0], m[1], m[2], m[3], m[4], m[5])))
        }
        "class/net/lo/address" => Some(file("00:00:00:00:00:00\n".to_string())),
        _ => None,
    }
}

pub fn sys_list(path: &str) -> Vec<(String, Node)> {
    let rest = path.strip_prefix("sys").unwrap_or(path).trim_start_matches('/');
    let names: Vec<&str> = match rest {
        "" => vec!["class", "devices", "kernel", "firmware", "fs", "block", "bus", "module"],
        "class" => vec!["net"],
        "class/net" => vec!["lo", "eth0"],
        "class/net/lo" | "class/net/eth0" => vec!["operstate", "address"],
        _ => Vec::new(),
    };
    names.into_iter().filter_map(|n| sys_lookup(&format!("{path}/{n}")).map(|node| (n.to_string(), node))).collect()
}
