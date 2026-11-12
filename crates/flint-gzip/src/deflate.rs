//! DEFLATE compression (RFC 1951): LZ77 with hash chains and lazy matching, then dynamic
//! Huffman blocks with optimal length-limited codes (package-merge), or stored blocks for
//! data that does not compress.

use crate::inflate::{reverse, CLEN_ORDER};

const WINDOW: usize = 32768;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
/// Tokens per block: a new block lets the Huffman codes follow the data.
const BLOCK_TOKENS: usize = 1 << 15;

/// How hard to look for matches: (longest chain walked, length that ends the search).
#[derive(Clone, Copy)]
pub struct Level {
    chain: usize,
    nice: usize,
}

impl Level {
    pub const FAST: Level = Level { chain: 8, nice: 32 };
    pub const DEFAULT: Level = Level { chain: 128, nice: 128 };
    pub const BEST: Level = Level { chain: 4096, nice: MAX_MATCH };

    /// gzip's -1 .. -9.
    pub fn from_number(n: u32) -> Level {
        match n {
            0..=3 => Level::FAST,
            4..=7 => Level::DEFAULT,
            _ => Level::BEST,
        }
    }
}

#[derive(Clone, Copy)]
enum Token {
    Lit(u8),
    Match { len: u16, dist: u16 },
}

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, k: u32) {
        self.buf |= u64::from(v) << self.n;
        self.n += k;
        while self.n >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.n -= 8;
        }
    }

    fn align(&mut self) {
        if self.n > 0 {
            self.out.push(self.buf as u8);
            self.buf = 0;
            self.n = 0;
        }
    }
}

fn len_code(len: usize) -> (usize, u32, u32) {
    // (symbol index 0..29, extra bits, extra value)
    const BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
    const EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    let i = if len == 258 { 28 } else { BASE.iter().rposition(|&b| usize::from(b) <= len).unwrap_or(0).min(27) };
    (i, u32::from(EXTRA[i]), (len - usize::from(BASE[i])) as u32)
}

fn dist_code(dist: usize) -> (usize, u32, u32) {
    const BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
    const EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
    let i = BASE.iter().rposition(|&b| usize::from(b) <= dist).unwrap_or(0);
    (i, u32::from(EXTRA[i]), (dist - usize::from(BASE[i])) as u32)
}

/// Optimal code lengths no longer than `limit` (package-merge). The code is complete
/// whenever two or more symbols are used; one used symbol gets length 1.
pub(crate) fn code_lengths(freqs: &[u32], limit: u32) -> Vec<u8> {
    let mut lengths = vec![0u8; freqs.len()];
    let mut leaves: Vec<(u64, usize)> = freqs.iter().enumerate().filter(|(_, &f)| f > 0).map(|(i, &f)| (u64::from(f), i)).collect();
    if leaves.is_empty() {
        return lengths;
    }
    if leaves.len() == 1 {
        lengths[leaves[0].1] = 1;
        return lengths;
    }
    leaves.sort_unstable();
    let n = leaves.len();
    // An item is (weight, the leaves it contains); a package holds the leaves of both halves.
    let leaf_items: Vec<(u64, Vec<u16>)> = leaves.iter().map(|&(w, i)| (w, vec![i as u16])).collect();
    let mut cur = leaf_items.clone();
    for _ in 1..limit {
        let mut packages = Vec::with_capacity(cur.len() / 2);
        let mut it = cur.chunks_exact(2);
        for pair in &mut it {
            let mut syms = pair[0].1.clone();
            syms.extend_from_slice(&pair[1].1);
            packages.push((pair[0].0 + pair[1].0, syms));
        }
        // merge the leaves with the packages, by weight (leaves first on ties)
        let mut merged = Vec::with_capacity(leaf_items.len() + packages.len());
        let (mut a, mut b) = (0, 0);
        while a < leaf_items.len() || b < packages.len() {
            if b >= packages.len() || (a < leaf_items.len() && leaf_items[a].0 <= packages[b].0) {
                merged.push(leaf_items[a].clone());
                a += 1;
            } else {
                merged.push(std::mem::take(&mut packages[b]));
                b += 1;
            }
        }
        cur = merged;
    }
    for item in cur.iter().take(2 * n - 2) {
        for &s in &item.1 {
            lengths[s as usize] += 1;
        }
    }
    lengths
}

/// Canonical codes (already bit-reversed for LSB-first output) for the given lengths.
fn codes(lengths: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &l in lengths {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            reverse(c, u32::from(l))
        })
        .collect()
}

/// Run-length encode the code lengths with symbols 16, 17 and 18: (symbol, extra value).
fn rle(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let l = lengths[i];
        let mut run = 1;
        while i + run < lengths.len() && lengths[i + run] == l {
            run += 1;
        }
        let mut left = run;
        if l == 0 {
            while left >= 11 {
                let k = left.min(138);
                out.push((18, (k - 11) as u8));
                left -= k;
            }
            if left >= 3 {
                out.push((17, (left - 3) as u8));
                left = 0;
            }
        } else {
            out.push((l, 0));
            left -= 1;
            while left >= 3 {
                let k = left.min(6);
                out.push((16, (k - 3) as u8));
                left -= k;
            }
        }
        for _ in 0..left {
            out.push((l, 0));
        }
        i += run;
    }
    out
}

fn write_block(w: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let mut lf = vec![0u32; 286];
    let mut df = vec![0u32; 30];
    for t in tokens {
        match *t {
            Token::Lit(b) => lf[b as usize] += 1,
            Token::Match { len, dist } => {
                lf[257 + len_code(len as usize).0] += 1;
                df[dist_code(dist as usize).0] += 1;
            }
        }
    }
    lf[256] = 1;
    let mut ll = code_lengths(&lf, 15);
    let mut dl = code_lengths(&df, 15);
    // A complete literal code needs two symbols; there is always EOB, so add one more.
    if ll.iter().filter(|&&l| l > 0).count() == 1 {
        ll[0] = 1;
        ll[256] = 1;
    }
    if dl.iter().all(|&l| l == 0) {
        dl[0] = 1; // no matches: one unused distance code, as zlib does
    }
    let hlit = 257 + ll[257..].iter().rposition(|&l| l > 0).map_or(0, |p| p + 1);
    let hdist = 1 + dl.iter().rposition(|&l| l > 0).unwrap_or(0);
    let mut all = ll[..hlit].to_vec();
    all.extend_from_slice(&dl[..hdist]);
    let runs = rle(&all);
    let mut cf = [0u32; 19];
    for &(s, _) in &runs {
        cf[s as usize] += 1;
    }
    let cl = code_lengths(&cf, 7);
    let hclen = 4 + CLEN_ORDER.iter().rposition(|&i| cl[i] > 0).map_or(0, |p| p + 1).saturating_sub(4);

    // Cost in bits of a dynamic block versus a stored one.
    let mut cost: u64 = 3 + 5 + 5 + 4 + 3 * hclen as u64;
    for &(s, _) in &runs {
        cost += u64::from(cl[s as usize]) + match s {
            16 => 2,
            17 => 3,
            18 => 7,
            _ => 0,
        };
    }
    for (i, &f) in lf.iter().enumerate() {
        let extra = if i >= 257 { u64::from(len_code_extra(i - 257)) } else { 0 };
        cost += u64::from(f) * (u64::from(ll[i]) + extra);
    }
    for (i, &f) in df.iter().enumerate() {
        cost += u64::from(f) * (u64::from(dl[i]) + u64::from(dist_code_extra(i)));
    }
    let stored_cost = (raw.len() as u64 + 5 * (raw.len() as u64 / 65535 + 1)) * 8 + 8;
    if stored_cost <= cost {
        let chunks: Vec<&[u8]> = if raw.is_empty() { vec![raw] } else { raw.chunks(65535).collect() };
        let n = chunks.len();
        for (k, chunk) in chunks.into_iter().enumerate() {
            w.put(u32::from(last && k + 1 == n), 1);
            w.put(0, 2);
            w.align();
            let len = chunk.len() as u32;
            w.put(len, 16);
            w.put(!len & 0xffff, 16);
            w.out.extend_from_slice(chunk);
        }
        return;
    }

    w.put(u32::from(last), 1);
    w.put(2, 2);
    w.put((hlit - 257) as u32, 5);
    w.put((hdist - 1) as u32, 5);
    w.put((hclen - 4) as u32, 4);
    for &i in &CLEN_ORDER[..hclen] {
        w.put(u32::from(cl[i]), 3);
    }
    let cc = codes(&cl);
    for &(s, extra) in &runs {
        w.put(cc[s as usize], u32::from(cl[s as usize]));
        match s {
            16 => w.put(u32::from(extra), 2),
            17 => w.put(u32::from(extra), 3),
            18 => w.put(u32::from(extra), 7),
            _ => {}
        }
    }
    let lc = codes(&ll);
    let dc = codes(&dl);
    for t in tokens {
        match *t {
            Token::Lit(b) => w.put(lc[b as usize], u32::from(ll[b as usize])),
            Token::Match { len, dist } => {
                let (i, eb, ev) = len_code(len as usize);
                w.put(lc[257 + i], u32::from(ll[257 + i]));
                w.put(ev, eb);
                let (j, eb, ev) = dist_code(dist as usize);
                w.put(dc[j], u32::from(dl[j]));
                w.put(ev, eb);
            }
        }
    }
    w.put(lc[256], u32::from(ll[256]));
}

fn len_code_extra(i: usize) -> u8 {
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0][i.min(28)]
}

fn dist_code_extra(i: usize) -> u8 {
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13][i]
}

struct Matcher<'a> {
    data: &'a [u8],
    head: Vec<i32>,
    prev: Vec<i32>,
    level: Level,
}

impl<'a> Matcher<'a> {
    fn hash(&self, i: usize) -> usize {
        let v = u32::from(self.data[i]) | u32::from(self.data[i + 1]) << 8 | u32::from(self.data[i + 2]) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    fn insert(&mut self, i: usize) {
        if i + MIN_MATCH <= self.data.len() {
            let h = self.hash(i);
            self.prev[i % WINDOW] = self.head[h];
            self.head[h] = i as i32;
        }
    }

    /// Longest earlier match for position `i`: (length, distance), length 0 if none.
    fn find(&self, i: usize, at_least: usize) -> (usize, usize) {
        let d = self.data;
        if i + MIN_MATCH > d.len() {
            return (0, 0);
        }
        let max = (d.len() - i).min(MAX_MATCH);
        let mut best = (at_least.max(MIN_MATCH - 1), 0);
        let mut cand = self.head[self.hash(i)];
        let mut chain = self.level.chain;
        while cand >= 0 && chain > 0 {
            let c = cand as usize;
            if c >= i || i - c > WINDOW {
                break;
            }
            // cheap reject: the byte that would make this match longer than the best
            if d[c + best.0.min(max - 1)] == d[i + best.0.min(max - 1)] {
                let mut l = 0;
                while l < max && d[c + l] == d[i + l] {
                    l += 1;
                }
                if l > best.0 {
                    best = (l, i - c);
                    if l >= self.level.nice || l == max {
                        break;
                    }
                }
            }
            let p = self.prev[c % WINDOW];
            if p >= cand {
                break;
            }
            cand = p;
            chain -= 1;
        }
        if best.1 == 0 {
            (0, 0)
        } else {
            best
        }
    }
}

/// Compress `data` into a raw DEFLATE stream.
pub fn deflate(data: &[u8], level: Level) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 2 + 64), buf: 0, n: 0 };
    let mut m = Matcher { data, head: vec![-1; 1 << HASH_BITS], prev: vec![-1; WINDOW], level };
    let mut tokens = Vec::with_capacity(BLOCK_TOKENS + 2);
    let mut block_start = 0;
    let mut i = 0;
    while i < data.len() {
        let (len, dist) = m.find(i, 0);
        if len >= MIN_MATCH {
            // Lazy matching: a longer match at the next byte wins over this one.
            m.insert(i);
            let (len2, _) = if len < level.nice && i + 1 < data.len() { m.find(i + 1, len) } else { (0, 0) };
            if len2 > len {
                tokens.push(Token::Lit(data[i]));
                i += 1;
                continue;
            }
            tokens.push(Token::Match { len: len as u16, dist: dist as u16 });
            for k in i + 1..i + len {
                m.insert(k);
            }
            i += len;
        } else {
            m.insert(i);
            tokens.push(Token::Lit(data[i]));
            i += 1;
        }
        if tokens.len() >= BLOCK_TOKENS {
            write_block(&mut w, &tokens, &data[block_start..i], false);
            tokens.clear();
            block_start = i;
        }
    }
    write_block(&mut w, &tokens, &data[block_start..], true);
    w.align();
    w.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_limited_and_complete() {
        // Fibonacci frequencies force very long unlimited codes.
        let mut f = vec![1u32, 1];
        while f.len() < 30 {
            let n = f[f.len() - 1] + f[f.len() - 2];
            f.push(n);
        }
        for limit in [7, 15] {
            let l = code_lengths(&f, limit);
            assert!(l.iter().all(|&x| x >= 1 && u32::from(x) <= limit));
            let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(i32::from(x))).sum();
            assert!((kraft - 1.0).abs() < 1e-12, "complete code, got {kraft}");
        }
        assert_eq!(code_lengths(&[0, 5, 0], 15), [0, 1, 0]);
    }
}
