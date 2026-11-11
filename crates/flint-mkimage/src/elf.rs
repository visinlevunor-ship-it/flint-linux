//! Just enough ELF64 to read a kernel module's `.modinfo` section.

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// The contents of the section called `name`.
pub fn section<'a>(elf: &'a [u8], name: &str) -> Option<&'a [u8]> {
    if elf.get(..4)? != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return None; // not little-endian ELF64
    }
    let shoff = u64le(elf, 0x28)? as usize;
    let shentsize = u16le(elf, 0x3a)? as usize;
    let shnum = u16le(elf, 0x3c)? as usize;
    let shstrndx = u16le(elf, 0x3e)? as usize;
    let sh = |i: usize| -> Option<(u32, usize, usize)> {
        let o = shoff + i * shentsize;
        Some((u32le(elf, o)?, u64le(elf, o + 0x18)? as usize, u64le(elf, o + 0x20)? as usize))
    };
    let (_, stroff, strsize) = sh(shstrndx)?;
    let strtab = elf.get(stroff..stroff + strsize)?;
    for i in 0..shnum {
        let (nameoff, off, size) = sh(i)?;
        let n = strtab.get(nameoff as usize..)?;
        let n = &n[..n.iter().position(|&c| c == 0)?];
        if n == name.as_bytes() {
            return elf.get(off..off + size);
        }
    }
    None
}

/// `key=value` pairs from a `.modinfo` section, in order.
pub fn modinfo(elf: &[u8]) -> Vec<(String, String)> {
    let Some(sec) = section(elf, ".modinfo") else { return Vec::new() };
    sec.split(|&b| b == 0)
        .filter_map(|s| {
            let s = std::str::from_utf8(s).ok()?;
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 with a .modinfo section, built by hand.
    fn tiny_elf(modinfo: &[u8]) -> Vec<u8> {
        let shstr = b"\0.modinfo\0.shstrtab\0";
        let mut e = vec![0u8; 64];
        e[..4].copy_from_slice(b"\x7fELF");
        e[4] = 2;
        e[5] = 1;
        let mi_off = e.len();
        e.extend_from_slice(modinfo);
        let ss_off = e.len();
        e.extend_from_slice(shstr);
        while e.len() % 8 != 0 {
            e.push(0);
        }
        let shoff = e.len();
        let mut sh = |name: u32, off: usize, size: usize| {
            let mut h = vec![0u8; 64];
            h[0..4].copy_from_slice(&name.to_le_bytes());
            h[0x18..0x20].copy_from_slice(&(off as u64).to_le_bytes());
            h[0x20..0x28].copy_from_slice(&(size as u64).to_le_bytes());
            e.extend_from_slice(&h);
        };
        sh(0, 0, 0);
        sh(1, mi_off, modinfo.len());
        sh(10, ss_off, shstr.len());
        e[0x28..0x30].copy_from_slice(&(shoff as u64).to_le_bytes());
        e[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        e[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        e[0x3e..0x40].copy_from_slice(&2u16.to_le_bytes());
        e
    }

    #[test]
    fn reads_modinfo() {
        let e = tiny_elf(b"alias=pci:v00008086d*\0depends=mac80211,cfg80211\0firmware=iwlwifi-cc-a0-89.ucode\0\0");
        let m = modinfo(&e);
        assert_eq!(m[0], ("alias".into(), "pci:v00008086d*".into()));
        assert_eq!(m[1].1, "mac80211,cfg80211");
        assert_eq!(m[2].0, "firmware");
        assert!(modinfo(b"not elf").is_empty());
        assert!(section(&e, ".nothing").is_none());
    }
}
