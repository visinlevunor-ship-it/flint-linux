//! gzip (RFC 1952) over DEFLATE (RFC 1951), written for Flint: packages are gzip-compressed
//! tar archives, and gzip/gunzip/zcat use the same code.

// Links flint-libc, Flint's C library, into every program built for Linux (no musl).
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
extern crate flint_libc as _;

mod deflate;
mod inflate;

pub use deflate::{deflate, Level};
pub use inflate::{inflate_into, Error};

/// CRC-32 (IEEE), as used by gzip.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    let mut c = !crc;
    for &b in data {
        c = t[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

pub const MAGIC: [u8; 2] = [0x1f, 0x8b];

pub fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 3 && data[..2] == MAGIC && data[2] == 8
}

/// A gzip file holding `data` (no file name, time 0: the same input gives the same bytes).
pub fn compress(data: &[u8], level: Level) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    out.extend_from_slice(&deflate(data, level));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Decompress a gzip file (all members, as gunzip does), at most `limit` bytes of output.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(data.len().saturating_mul(3).min(limit));
    let mut pos = 0;
    loop {
        let d = &data[pos..];
        if d.len() < 18 || !is_gzip(d) {
            return Err(Error("not gzip data"));
        }
        let flags = d[3];
        if flags & 0xe0 != 0 {
            return Err(Error("unknown gzip flags"));
        }
        let mut p = 10;
        if flags & 4 != 0 {
            let xlen = usize::from(u16::from_le_bytes([*d.get(p).ok_or(Error("truncated"))?, *d.get(p + 1).ok_or(Error("truncated"))?]));
            p += 2 + xlen;
        }
        for bit in [8u8, 16] {
            if flags & bit != 0 {
                let end = d.get(p..).and_then(|r| r.iter().position(|&b| b == 0)).ok_or(Error("truncated gzip header"))?;
                p += end + 1;
            }
        }
        if flags & 2 != 0 {
            p += 2;
        }
        if p > d.len() {
            return Err(Error("truncated gzip header"));
        }
        let start = out.len();
        let used = inflate_into(&d[p..], &mut out, limit - start.min(limit))?;
        let t = p + used;
        let trailer = d.get(t..t + 8).ok_or(Error("gzip trailer missing"))?;
        let crc = u32::from_le_bytes(trailer[..4].try_into().unwrap());
        let size = u32::from_le_bytes(trailer[4..].try_into().unwrap());
        if crc32(&out[start..]) != crc {
            return Err(Error("gzip checksum mismatch: the data is damaged"));
        }
        if (out.len() - start) as u32 != size {
            return Err(Error("gzip size mismatch: the data is damaged"));
        }
        pos += t + 8;
        // trailing zero padding (dd conv=sync, tape blocks) is ignored, like gzip does
        if pos >= data.len() || data[pos..].iter().all(|&b| b == 0) {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Vec<u8>> {
        let mut v = vec![Vec::new(), b"a".to_vec(), b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(), b"hello hello hello world".to_vec()];
        // pseudo-random (incompressible) and structured data, larger than a block
        let mut x: u32 = 1;
        v.push((0..200_000).map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        }).collect());
        v.push((0..300_000u32).flat_map(|i| format!("line {} of {}\n", i % 977, i / 3).into_bytes()).collect());
        v.push(vec![0u8; 1_000_000]);
        v
    }

    #[test]
    fn round_trip() {
        for s in samples() {
            for level in [Level::FAST, Level::DEFAULT, Level::BEST] {
                let z = compress(&s, level);
                assert_eq!(decompress(&z, usize::MAX).unwrap(), s, "len {}", s.len());
            }
        }
    }

    #[test]
    fn known_answers() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // `printf hello | gzip -n` (zlib, dynamic Huffman off: a fixed block)
        let z = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x86, 0xa6, 0x10, 0x36, 5, 0, 0, 0];
        assert_eq!(decompress(&z, 100).unwrap(), b"hello");
        // two members concatenated
        let mut two = z.to_vec();
        two.extend_from_slice(&z);
        assert_eq!(decompress(&two, 100).unwrap(), b"hellohello");
    }

    #[test]
    fn damage_and_limits() {
        let s: Vec<u8> = (0..50_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut z = compress(&s, Level::DEFAULT);
        assert!(decompress(&z, 1000).is_err(), "limit");
        let n = z.len();
        z[n / 2] ^= 0x10;
        assert!(decompress(&z, usize::MAX).is_err(), "damage is noticed");
        assert!(decompress(b"not gzip at all, really", 100).is_err());
        assert!(decompress(&compress(b"abc", Level::FAST)[..15], 100).is_err(), "truncated");
    }
}
