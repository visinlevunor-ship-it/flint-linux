//! DEFLATE decompression (RFC 1951).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// Order of the code-length code lengths in a dynamic block header.
pub(crate) const CLEN_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, buf: 0, n: 0 }
    }

    fn refill(&mut self) {
        while self.n <= 56 {
            let b = match self.data.get(self.pos) {
                Some(&b) => b,
                None => return,
            };
            self.buf |= u64::from(b) << self.n;
            self.pos += 1;
            self.n += 8;
        }
    }

    fn need(&mut self, k: u32) -> Result<(), Error> {
        if self.n < k {
            self.refill();
            if self.n < k {
                return Err(Error("compressed data ends too soon"));
            }
        }
        Ok(())
    }

    fn bits(&mut self, k: u32) -> Result<u32, Error> {
        if k == 0 {
            return Ok(0);
        }
        self.need(k)?;
        let v = (self.buf & ((1u64 << k) - 1)) as u32;
        self.buf >>= k;
        self.n -= k;
        Ok(v)
    }

    fn align(&mut self) {
        let drop = self.n % 8;
        self.buf >>= drop;
        self.n -= drop;
    }

    /// Bytes consumed so far (after `align`, the buffered whole bytes are given back).
    pub(crate) fn consumed(&self) -> usize {
        self.pos - (self.n / 8) as usize
    }
}

/// A canonical Huffman decoder: a fast table for codes up to 9 bits, then a slow walk.
struct Huffman {
    /// For each code length, how many codes have it.
    counts: [u16; 16],
    /// Symbols ordered by code.
    symbols: Vec<u16>,
    /// Fast table: (symbol << 4) | length, indexed by the next 9 bits (reversed code); 0 = slow.
    fast: Vec<u16>,
}

const FAST_BITS: u32 = 9;

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, Error> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // Over-subscribed code sets are invalid; incomplete ones are allowed (single-code
        // distance trees are common).
        let mut left: i32 = 1;
        for len in 1..16 {
            left <<= 1;
            left -= i32::from(counts[len]);
            if left < 0 {
                return Err(Error("invalid Huffman code"));
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        let mut h = Huffman { counts, symbols, fast: vec![0; 1 << FAST_BITS] };
        // Fill the fast table: walk codes in canonical order.
        let mut code: u32 = 0;
        let mut index = 0usize;
        for len in 1..=FAST_BITS as usize {
            for _ in 0..h.counts[len] {
                let sym = h.symbols[index];
                index += 1;
                let rev = reverse(code, len as u32);
                let mut i = rev as usize;
                while i < (1 << FAST_BITS) {
                    h.fast[i] = (sym << 4) | len as u16;
                    i += 1 << len;
                }
                code += 1;
            }
            code <<= 1;
        }
        Ok(h)
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, Error> {
        if b.n < 15 {
            b.refill();
        }
        let e = self.fast[(b.buf & ((1 << FAST_BITS) - 1)) as usize];
        if e != 0 && u32::from(e & 15) <= b.n {
            let len = u32::from(e & 15);
            b.buf >>= len;
            b.n -= len;
            return Ok(e >> 4);
        }
        // Slow path: one bit at a time (canonical decoding, as in zlib's puff).
        let (mut code, mut first, mut index): (i32, i32, i32) = (0, 0, 0);
        for len in 1..16 {
            code |= b.bits(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Error("invalid Huffman code in data"))
    }
}

pub(crate) fn reverse(mut code: u32, len: u32) -> u32 {
    let mut r = 0;
    for _ in 0..len {
        r = (r << 1) | (code & 1);
        code >>= 1;
    }
    r
}

fn fixed() -> (Huffman, Huffman) {
    let mut l = [0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    (Huffman::new(&l).expect("fixed code"), Huffman::new(&[5u8; 30]).expect("fixed code"))
}

fn dynamic(b: &mut Bits) -> Result<(Huffman, Huffman), Error> {
    let hlit = b.bits(5)? as usize + 257;
    let hdist = b.bits(5)? as usize + 1;
    let hclen = b.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(Error("bad block header"));
    }
    let mut cl = [0u8; 19];
    for &i in &CLEN_ORDER[..hclen] {
        cl[i] = b.bits(3)? as u8;
    }
    let clh = Huffman::new(&cl)?;
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = clh.decode(b)?;
        let (val, rep) = match sym {
            0..=15 => (sym as u8, 1),
            16 => {
                if i == 0 {
                    return Err(Error("repeat with nothing before it"));
                }
                (lengths[i - 1], 3 + b.bits(2)? as usize)
            }
            17 => (0, 3 + b.bits(3)? as usize),
            18 => (0, 11 + b.bits(7)? as usize),
            _ => return Err(Error("bad code length code")),
        };
        if i + rep > lengths.len() {
            return Err(Error("code lengths overflow"));
        }
        lengths[i..i + rep].fill(val);
        i += rep;
    }
    if lengths[256] == 0 {
        return Err(Error("no end-of-block code"));
    }
    Ok((Huffman::new(&lengths[..hlit])?, Huffman::new(&lengths[hlit..])?))
}

/// Decompress one raw DEFLATE stream from the start of `data`, appending to `out` (at most
/// `limit` bytes in total). Returns how many input bytes the stream used.
pub fn inflate_into(data: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    let start = out.len();
    let mut b = Bits::new(data);
    loop {
        let last = b.bits(1)? == 1;
        match b.bits(2)? {
            0 => {
                b.align();
                let len = b.bits(16)?;
                let nlen = b.bits(16)?;
                if len != !nlen & 0xffff {
                    return Err(Error("stored block length mismatch"));
                }
                // The bit buffer is byte aligned: take bytes from it first, then the input.
                for _ in 0..len {
                    out.push(b.bits(8)? as u8);
                }
                if out.len() - start > limit {
                    return Err(Error("data larger than the limit"));
                }
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 { fixed() } else { dynamic(&mut b)? };
                loop {
                    let sym = lit.decode(&mut b)?;
                    if sym < 256 {
                        out.push(sym as u8);
                        continue;
                    }
                    if sym == 256 {
                        break;
                    }
                    let i = usize::from(sym - 257);
                    if i >= 29 {
                        return Err(Error("bad length code"));
                    }
                    let len = usize::from(LEN_BASE[i]) + b.bits(u32::from(LEN_EXTRA[i]))? as usize;
                    let d = usize::from(dist.decode(&mut b)?);
                    if d >= 30 {
                        return Err(Error("bad distance code"));
                    }
                    let distance = usize::from(DIST_BASE[d]) + b.bits(u32::from(DIST_EXTRA[d]))? as usize;
                    if distance > out.len() - start {
                        return Err(Error("distance too far back"));
                    }
                    let from = out.len() - distance;
                    if distance >= len {
                        out.extend_from_within(from..from + len);
                    } else {
                        for k in 0..len {
                            let c = out[from + k];
                            out.push(c);
                        }
                    }
                    if out.len() - start > limit {
                        return Err(Error("data larger than the limit"));
                    }
                }
            }
            _ => return Err(Error("bad block type")),
        }
        if last {
            b.align();
            return Ok(b.consumed());
        }
    }
}
