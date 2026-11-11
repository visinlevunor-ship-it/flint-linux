//! nl80211 (the kernel's Wi-Fi interface) over generic netlink: scanning, connecting,
//! installing keys, and the events that report what happened.

use std::io;
use std::time::{Duration, Instant};

use crate::netlink::{self, Builder, Socket, NLM_F_DUMP};

const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const CTRL_ATTR_MCAST_GROUPS: u16 = 7;
const CTRL_ATTR_MCAST_GRP_NAME: u16 = 1;
const CTRL_ATTR_MCAST_GRP_ID: u16 = 2;

pub const CMD_GET_INTERFACE: u8 = 5;
pub const CMD_NEW_KEY: u8 = 11;
pub const CMD_SET_STATION: u8 = 18;
pub const CMD_GET_SCAN: u8 = 32;
pub const CMD_TRIGGER_SCAN: u8 = 33;
pub const CMD_NEW_SCAN_RESULTS: u8 = 34;
pub const CMD_SCAN_ABORTED: u8 = 35;
pub const CMD_CONNECT: u8 = 46;
pub const CMD_DISCONNECT: u8 = 48;

const ATTR_IFINDEX: u16 = 3;
const ATTR_IFTYPE: u16 = 5;
pub const IFTYPE_STATION: u32 = 2;
const ATTR_MAC: u16 = 6;
const ATTR_KEY_DATA: u16 = 7;
const ATTR_KEY_IDX: u16 = 8;
const ATTR_KEY_CIPHER: u16 = 9;
const ATTR_KEY_SEQ: u16 = 10;
const ATTR_WIPHY_FREQ: u16 = 38;
const ATTR_IE: u16 = 42;
const ATTR_SCAN_SSIDS: u16 = 45;
const ATTR_BSS: u16 = 47;
const ATTR_SSID: u16 = 52;
const ATTR_AUTH_TYPE: u16 = 53;
const ATTR_REASON_CODE: u16 = 54;
const ATTR_KEY_TYPE: u16 = 55;
const ATTR_STA_FLAGS2: u16 = 67;
const ATTR_CONTROL_PORT: u16 = 68;
const ATTR_PRIVACY: u16 = 70;
const ATTR_STATUS_CODE: u16 = 72;
const ATTR_CIPHER_SUITES_PAIRWISE: u16 = 73;
const ATTR_CIPHER_SUITE_GROUP: u16 = 74;
const ATTR_WPA_VERSIONS: u16 = 75;
const ATTR_AKM_SUITES: u16 = 76;

const BSS_BSSID: u16 = 1;
const BSS_FREQUENCY: u16 = 2;
const BSS_CAPABILITY: u16 = 5;
const BSS_INFORMATION_ELEMENTS: u16 = 6;
const BSS_SIGNAL_MBM: u16 = 7;
const BSS_STATUS: u16 = 9;

const KEYTYPE_GROUP: u32 = 0;
const KEYTYPE_PAIRWISE: u32 = 1;
const STA_FLAG_AUTHORIZED: u32 = 1 << 1;

pub const CIPHER_TKIP: u32 = 0x000F_AC02;
pub const CIPHER_CCMP: u32 = 0x000F_AC04;
pub const AKM_8021X: u32 = 0x000F_AC01;
pub const AKM_PSK: u32 = 0x000F_AC02;
pub const AKM_FT_PSK: u32 = 0x000F_AC04;
pub const AKM_PSK_SHA256: u32 = 0x000F_AC06;
pub const AKM_SAE: u32 = 0x000F_AC08;
/// RSN capabilities: management frame protection required.
pub const RSN_MFPR: u16 = 1 << 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rsn {
    pub group: u32,
    pub pairwise: Vec<u32>,
    pub akm: Vec<u32>,
    pub caps: u16,
}

fn suite(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The RSN element body (after id and length).
pub fn parse_rsn(b: &[u8]) -> Option<Rsn> {
    if b.len() < 2 || u16::from_le_bytes([b[0], b[1]]) != 1 {
        return None;
    }
    let mut r = Rsn { group: CIPHER_CCMP, pairwise: vec![CIPHER_CCMP], akm: vec![AKM_8021X], caps: 0 };
    let mut at = 2;
    if b.len() >= at + 4 {
        r.group = suite(&b[at..]);
        at += 4;
    } else {
        return Some(r);
    }
    for which in 0..2 {
        if b.len() < at + 2 {
            return Some(r);
        }
        let n = usize::from(u16::from_le_bytes([b[at], b[at + 1]]));
        at += 2;
        let body = b.get(at..at + 4 * n)?;
        let list: Vec<u32> = body.chunks_exact(4).map(suite).collect();
        if which == 0 {
            r.pairwise = list;
        } else {
            r.akm = list;
        }
        at += 4 * n;
    }
    if b.len() >= at + 2 {
        r.caps = u16::from_le_bytes([b[at], b[at + 1]]);
    }
    Some(r)
}

/// Information elements as (id, body).
pub fn elements(ies: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 2 <= ies.len() {
        let len = usize::from(ies[at + 1]);
        let Some(body) = ies.get(at + 2..at + 2 + len) else { break };
        out.push((ies[at], body));
        at += 2 + len;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    /// WPA2-Personal (also WPA2/WPA3 transition networks): supported.
    Wpa2,
    Wep,
    WpaOld,
    Wpa3Only,
    Enterprise,
    /// Requires management frame protection, which Flint does not do yet.
    Wpa2Pmf,
}

impl Security {
    pub fn name(self) -> &'static str {
        match self {
            Security::Open => "open",
            Security::Wpa2 => "WPA2",
            Security::Wep => "WEP (not supported)",
            Security::WpaOld => "WPA1 (not supported)",
            Security::Wpa3Only => "WPA3-only (not supported)",
            Security::Enterprise => "Enterprise (not supported)",
            Security::Wpa2Pmf => "WPA2 + required PMF (not supported)",
        }
    }

    pub fn supported(self) -> bool {
        matches!(self, Security::Open | Security::Wpa2)
    }
}

#[derive(Debug, Clone)]
pub struct Bss {
    pub bssid: [u8; 6],
    pub freq: u32,
    /// dBm
    pub signal: i32,
    pub ssid: Vec<u8>,
    pub rsn: Option<Rsn>,
    pub security: Security,
    pub associated: bool,
}

pub fn classify(privacy: bool, rsn: Option<&Rsn>, wpa1: bool) -> Security {
    match rsn {
        Some(r) => {
            if r.akm.contains(&AKM_PSK) && r.pairwise.contains(&CIPHER_CCMP) {
                if r.caps & RSN_MFPR != 0 {
                    Security::Wpa2Pmf
                } else {
                    Security::Wpa2
                }
            } else if r.akm.contains(&AKM_SAE) {
                Security::Wpa3Only
            } else if r.akm.contains(&AKM_PSK) || r.akm.contains(&AKM_FT_PSK) || r.akm.contains(&AKM_PSK_SHA256) {
                Security::WpaOld
            } else {
                Security::Enterprise
            }
        }
        None if wpa1 => Security::WpaOld,
        None if privacy => Security::Wep,
        None => Security::Open,
    }
}

pub fn parse_bss(attrs: &[(u16, &[u8])]) -> Option<Bss> {
    let b = netlink::attrs(netlink::find(attrs, ATTR_BSS)?);
    let bssid: [u8; 6] = netlink::find(&b, BSS_BSSID)?.try_into().ok()?;
    let ies = netlink::find(&b, BSS_INFORMATION_ELEMENTS).unwrap_or(&[]);
    let mut ssid = Vec::new();
    let mut rsn = None;
    let mut wpa1 = false;
    for (id, body) in elements(ies) {
        match id {
            0 if ssid.is_empty() => ssid = body.to_vec(),
            48 => rsn = parse_rsn(body),
            221 if body.starts_with(&[0x00, 0x50, 0xF2, 0x01]) => wpa1 = true,
            _ => {}
        }
    }
    let cap = netlink::get_u16(&b, BSS_CAPABILITY).unwrap_or(0);
    let security = classify(cap & 0x10 != 0, rsn.as_ref(), wpa1);
    Some(Bss {
        bssid,
        freq: netlink::get_u32(&b, BSS_FREQUENCY).unwrap_or(0),
        signal: netlink::get_u32(&b, BSS_SIGNAL_MBM).map_or(-100, |m| m as i32 / 100),
        ssid,
        rsn,
        security,
        associated: netlink::get_u32(&b, BSS_STATUS).is_some_and(|s| s == 1 || s == 2),
    })
}

/// Our RSN element for WPA2-Personal: CCMP pairwise, PSK, the AP's group cipher.
pub fn rsn_ie(group: u32) -> Vec<u8> {
    let mut b = vec![48, 20, 1, 0];
    b.extend_from_slice(&group.to_be_bytes());
    b.extend_from_slice(&[1, 0]);
    b.extend_from_slice(&CIPHER_CCMP.to_be_bytes());
    b.extend_from_slice(&[1, 0]);
    b.extend_from_slice(&AKM_PSK.to_be_bytes());
    b.extend_from_slice(&[0, 0]);
    b
}

#[derive(Debug)]
pub enum Event {
    ScanDone(u32),
    ScanAborted(u32),
    /// ifindex, IEEE status code (0 = success), AP address
    Connected(u32, u16, Option<[u8; 6]>),
    /// ifindex, reason code
    Disconnected(u32, u16),
}

pub struct Nl80211 {
    pub sock: Socket,
    pub family: u16,
    groups: Vec<(String, u32)>,
}

fn genl(cmd: u8) -> [u8; 4] {
    [cmd, 1, 0, 0]
}

impl Nl80211 {
    pub fn open() -> io::Result<Nl80211> {
        let mut sock = Socket::open(libc::NETLINK_GENERIC)?;
        let mut b = Builder::new(&genl(CTRL_CMD_GETFAMILY));
        b.put_str(CTRL_ATTR_FAMILY_NAME, "nl80211");
        let msgs = sock.request(GENL_ID_CTRL, 0, &b.buf).map_err(|e| {
            if e.raw_os_error() == Some(libc::ENOENT) {
                io::Error::new(io::ErrorKind::NotFound, "the kernel has no Wi-Fi support loaded (cfg80211)")
            } else {
                e
            }
        })?;
        let m = msgs.first().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nl80211 not found"))?;
        let a = netlink::attrs(&m.payload[4..]);
        let family = netlink::get_u16(&a, CTRL_ATTR_FAMILY_ID).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no nl80211 family id"))?;
        let mut groups = Vec::new();
        if let Some(g) = netlink::find(&a, CTRL_ATTR_MCAST_GROUPS) {
            for (_, grp) in netlink::attrs(g) {
                let ga = netlink::attrs(grp);
                if let (Some(n), Some(id)) = (netlink::get_str(&ga, CTRL_ATTR_MCAST_GRP_NAME), netlink::get_u32(&ga, CTRL_ATTR_MCAST_GRP_ID)) {
                    groups.push((n, id));
                }
            }
        }
        Ok(Nl80211 { sock, family, groups })
    }

    /// A separate socket that receives scan and connection events.
    pub fn events() -> io::Result<Nl80211> {
        let n = Nl80211::open()?;
        for g in ["scan", "mlme"] {
            if let Some((_, id)) = n.groups.iter().find(|(name, _)| name == g) {
                n.sock.join(*id)?;
            }
        }
        Ok(n)
    }

    pub fn next_event(&self, timeout: Duration) -> io::Result<Option<Event>> {
        let msgs = match self.sock.recv(Some(timeout)) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => return Ok(None),
            Err(e) => return Err(e),
        };
        for m in msgs {
            if m.ty != self.family || m.payload.len() < 4 {
                continue;
            }
            let a = netlink::attrs(&m.payload[4..]);
            let ifindex = netlink::get_u32(&a, ATTR_IFINDEX).unwrap_or(0);
            let ev = match m.payload[0] {
                CMD_NEW_SCAN_RESULTS => Event::ScanDone(ifindex),
                CMD_SCAN_ABORTED => Event::ScanAborted(ifindex),
                CMD_CONNECT => Event::Connected(ifindex, netlink::get_u16(&a, ATTR_STATUS_CODE).unwrap_or(1), netlink::find(&a, ATTR_MAC).and_then(|m| m.try_into().ok())),
                CMD_DISCONNECT => Event::Disconnected(ifindex, netlink::get_u16(&a, ATTR_REASON_CODE).unwrap_or(0)),
                _ => continue,
            };
            return Ok(Some(ev));
        }
        Ok(None)
    }

    fn cmd(&mut self, cmd: u8, b: &Builder) -> io::Result<()> {
        let mut buf = genl(cmd).to_vec();
        buf.extend_from_slice(&b.buf);
        self.sock.request(self.family, 0, &buf).map(|_| ())
    }

    /// The interface type: 2 = station (a normal client), 3 = access point, ...
    pub fn iftype(&mut self, ifindex: u32) -> io::Result<u32> {
        let mut b = Builder::new(&genl(CMD_GET_INTERFACE));
        b.put_u32(ATTR_IFINDEX, ifindex);
        let msgs = self.sock.request(self.family, 0, &b.buf)?;
        let m = msgs.first().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such Wi-Fi interface"))?;
        netlink::get_u32(&netlink::attrs(&m.payload[4..]), ATTR_IFTYPE).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no interface type"))
    }

    pub fn trigger_scan(&mut self, ifindex: u32) -> io::Result<()> {
        let mut b = Builder::default();
        b.put_u32(ATTR_IFINDEX, ifindex).begin(ATTR_SCAN_SSIDS).put(1, &[]).end();
        self.cmd(CMD_TRIGGER_SCAN, &b)
    }

    pub fn scan_results(&mut self, ifindex: u32) -> io::Result<Vec<Bss>> {
        let mut b = Builder::new(&genl(CMD_GET_SCAN));
        b.put_u32(ATTR_IFINDEX, ifindex);
        let msgs = self.sock.request(self.family, NLM_F_DUMP, &b.buf)?;
        Ok(msgs.iter().filter(|m| m.payload.len() > 4).filter_map(|m| parse_bss(&netlink::attrs(&m.payload[4..]))).collect())
    }

    /// Trigger a scan, wait for it (using `events`), and return what was found, strongest first.
    pub fn scan(&mut self, events: &Nl80211, ifindex: u32) -> io::Result<Vec<Bss>> {
        match self.trigger_scan(ifindex) {
            Ok(()) => {}
            // A scan already running (or the card just refusing for a moment): use its results.
            Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {}
            Err(e) if e.raw_os_error() == Some(libc::ENETDOWN) => return Err(io::Error::new(e.kind(), "the interface is down")),
            Err(e) => return Err(e),
        }
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            match events.next_event(Duration::from_millis(500))? {
                Some(Event::ScanDone(i)) | Some(Event::ScanAborted(i)) if i == ifindex => break,
                _ => {}
            }
        }
        let mut r = self.scan_results(ifindex)?;
        r.sort_by(|a, b| b.signal.cmp(&a.signal));
        Ok(r)
    }

    pub fn connect(&mut self, ifindex: u32, bss: &Bss, ie: Option<&[u8]>) -> io::Result<()> {
        let mut b = Builder::default();
        b.put_u32(ATTR_IFINDEX, ifindex).put(ATTR_SSID, &bss.ssid).put(ATTR_MAC, &bss.bssid).put_u32(ATTR_WIPHY_FREQ, bss.freq).put_u32(ATTR_AUTH_TYPE, 0);
        if let (Some(ie), Some(rsn)) = (ie, &bss.rsn) {
            b.put(ATTR_IE, ie)
                .put_flag(ATTR_PRIVACY)
                .put_u32(ATTR_WPA_VERSIONS, 2)
                .put_u32(ATTR_CIPHER_SUITES_PAIRWISE, CIPHER_CCMP)
                .put_u32(ATTR_CIPHER_SUITE_GROUP, rsn.group)
                .put_u32(ATTR_AKM_SUITES, AKM_PSK)
                .put_flag(ATTR_CONTROL_PORT);
        }
        self.cmd(CMD_CONNECT, &b)
    }

    pub fn disconnect(&mut self, ifindex: u32) -> io::Result<()> {
        let mut b = Builder::default();
        b.put_u32(ATTR_IFINDEX, ifindex).put_u16(ATTR_REASON_CODE, 3);
        match self.cmd(CMD_DISCONNECT, &b) {
            Err(e) if e.raw_os_error() == Some(libc::ENOTCONN) => Ok(()),
            r => r,
        }
    }

    pub fn new_key(&mut self, ifindex: u32, pairwise_with: Option<&[u8; 6]>, idx: u8, cipher: u32, key: &[u8], seq: &[u8]) -> io::Result<()> {
        let mut b = Builder::default();
        b.put_u32(ATTR_IFINDEX, ifindex).put(ATTR_KEY_DATA, key).put_u8(ATTR_KEY_IDX, idx).put_u32(ATTR_KEY_CIPHER, cipher);
        if !seq.is_empty() {
            b.put(ATTR_KEY_SEQ, seq);
        }
        match pairwise_with {
            Some(mac) => b.put(ATTR_MAC, mac).put_u32(ATTR_KEY_TYPE, KEYTYPE_PAIRWISE),
            None => b.put_u32(ATTR_KEY_TYPE, KEYTYPE_GROUP),
        };
        self.cmd(CMD_NEW_KEY, &b)
    }

    /// Open the data path to the AP once the keys are in.
    pub fn authorize(&mut self, ifindex: u32, ap: &[u8; 6]) -> io::Result<()> {
        let mut flags = [0u8; 8];
        flags[..4].copy_from_slice(&STA_FLAG_AUTHORIZED.to_ne_bytes());
        flags[4..].copy_from_slice(&STA_FLAG_AUTHORIZED.to_ne_bytes());
        let mut b = Builder::default();
        b.put_u32(ATTR_IFINDEX, ifindex).put(ATTR_MAC, ap).put(ATTR_STA_FLAGS2, &flags);
        self.cmd(CMD_SET_STATION, &b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsn_elements() {
        let ie = rsn_ie(CIPHER_CCMP);
        let r = parse_rsn(&ie[2..]).unwrap();
        assert_eq!(r, Rsn { group: CIPHER_CCMP, pairwise: vec![CIPHER_CCMP], akm: vec![AKM_PSK], caps: 0 });
        assert_eq!(classify(true, Some(&r), false), Security::Wpa2);
        // WPA2/WPA3 transition: PSK and SAE offered, PMF capable but not required.
        let mixed = Rsn { group: CIPHER_CCMP, pairwise: vec![CIPHER_CCMP], akm: vec![AKM_PSK, AKM_SAE], caps: 0x80 };
        assert_eq!(classify(true, Some(&mixed), false), Security::Wpa2);
        let wpa3 = Rsn { akm: vec![AKM_SAE], caps: RSN_MFPR | 0x80, ..mixed.clone() };
        assert_eq!(classify(true, Some(&wpa3), false), Security::Wpa3Only);
        let ent = Rsn { akm: vec![AKM_8021X], ..mixed.clone() };
        assert_eq!(classify(true, Some(&ent), false), Security::Enterprise);
        assert_eq!(classify(false, None, false), Security::Open);
        assert_eq!(classify(true, None, false), Security::Wep);
        assert_eq!(classify(true, None, true), Security::WpaOld);
        assert!(parse_rsn(&[2, 0]).is_none());
        assert!(parse_rsn(&[1, 0, 0, 0x0F, 0xAC, 4, 5, 0]).is_none(), "count larger than the element");
        let ies = [0u8, 4, b'h', b'o', b'm', b'e', 48, 2, 1, 0, 9, 200];
        let e = elements(&ies);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0], (0, &b"home"[..]));
    }
}
