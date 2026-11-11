//! A TLS 1.3 client (RFC 8446): X25519 key exchange, TLS_AES_128_GCM_SHA256.
//!
//! Handshake integrity is checked (the server's Finished message), so the connection is
//! private against anyone who only listens. The server's certificate is not checked, so it
//! does not prove who the server is: see the crate documentation for why pkg can live with
//! that (everything it accepts is signed).

use std::io::{self, Read, Write};
use std::net::TcpStream;

use flint_crypto::curve25519::{x25519, x25519_public};
use flint_crypto::gcm::Gcm;
use flint_crypto::hkdf::{expand_label, extract};
use flint_crypto::{ct_eq, hmac_sha256, random, sha256};

const HANDSHAKE: u8 = 22;
const APPDATA: u8 = 23;
const ALERT: u8 = 21;
const CCS: u8 = 20;

/// The fixed "random" of a HelloRetryRequest (RFC 8446 4.1.3).
const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91, 0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("TLS: {}", msg.into()))
}

struct Keys {
    gcm: Gcm,
    iv: [u8; 12],
    seq: u64,
}

impl Keys {
    fn new(secret: &[u8]) -> Keys {
        let key: [u8; 16] = expand_label(secret, "key", &[], 16).try_into().unwrap();
        let iv: [u8; 12] = expand_label(secret, "iv", &[], 12).try_into().unwrap();
        Keys { gcm: Gcm::new(&key), iv, seq: 0 }
    }

    fn nonce(&mut self) -> [u8; 12] {
        let mut n = self.iv;
        for (i, b) in self.seq.to_be_bytes().iter().enumerate() {
            n[4 + i] ^= b;
        }
        self.seq += 1;
        n
    }
}

fn hash(data: &[u8]) -> [u8; 32] {
    sha256::digest(data)
}

pub struct TlsStream {
    tcp: TcpStream,
    read_keys: Keys,
    write_keys: Keys,
    server_secret: Vec<u8>,
    plain: Vec<u8>,
    plain_pos: usize,
    closed: bool,
}

fn ext(out: &mut Vec<u8>, ty: u16, body: &[u8]) {
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
}

fn client_hello(host: &str, random_bytes: &[u8; 32], session: &[u8; 32], pubkey: &[u8; 32]) -> Vec<u8> {
    let mut exts = Vec::new();
    // server_name
    let mut sn = Vec::new();
    sn.extend_from_slice(&((host.len() + 3) as u16).to_be_bytes());
    sn.push(0);
    sn.extend_from_slice(&(host.len() as u16).to_be_bytes());
    sn.extend_from_slice(host.as_bytes());
    if host.parse::<std::net::IpAddr>().is_err() {
        ext(&mut exts, 0, &sn);
    }
    ext(&mut exts, 10, &[0, 2, 0x00, 0x1d]); // supported_groups: x25519
    // signature_algorithms: what we would accept (the certificate is not checked, but
    // servers refuse a ClientHello without this list)
    let algs: [u16; 9] = [0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601, 0x0807];
    let mut sa = ((algs.len() * 2) as u16).to_be_bytes().to_vec();
    for a in algs {
        sa.extend_from_slice(&a.to_be_bytes());
    }
    ext(&mut exts, 13, &sa);
    // key_share: one x25519 share
    let mut ks = Vec::new();
    ks.extend_from_slice(&36u16.to_be_bytes());
    ks.extend_from_slice(&[0x00, 0x1d, 0x00, 0x20]);
    ks.extend_from_slice(pubkey);
    ext(&mut exts, 51, &ks);
    ext(&mut exts, 43, &[2, 0x03, 0x04]); // supported_versions: TLS 1.3
    ext(&mut exts, 45, &[1, 1]); // psk_key_exchange_modes: psk_dhe_ke (some servers want it)
    ext(&mut exts, 16, &[0, 9, 8, b'h', b't', b't', b'p', b'/', b'1', b'.', b'1']); // ALPN http/1.1

    let mut body = Vec::new();
    body.extend_from_slice(&[0x03, 0x03]);
    body.extend_from_slice(random_bytes);
    body.push(32);
    body.extend_from_slice(session);
    body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // TLS_AES_128_GCM_SHA256
    body.extend_from_slice(&[0x01, 0x00]); // no compression
    body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    body.extend_from_slice(&exts);
    let mut msg = vec![1];
    msg.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    msg.extend_from_slice(&body);
    msg
}

/// One TLS record: (type, payload).
fn read_record(tcp: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    tcp.read_exact(&mut h)?;
    let len = usize::from(u16::from_be_bytes([h[3], h[4]]));
    if len > 16384 + 256 {
        return Err(err("record too long"));
    }
    let mut body = vec![0u8; len];
    tcp.read_exact(&mut body)?;
    Ok((h[0], body))
}

fn alert_text(code: u8) -> &'static str {
    match code {
        0 => "close notify",
        10 => "unexpected message",
        20 => "bad record MAC",
        40 => "handshake failure",
        42 | 43 | 44 | 45 | 46 => "certificate problem",
        47 => "illegal parameter",
        50 => "decode error",
        51 => "decrypt error",
        70 => "protocol version not supported",
        71 => "insufficient security",
        80 => "internal error",
        109 => "missing extension",
        110 => "unsupported extension",
        112 => "unrecognized server name",
        _ => "error",
    }
}

/// Decrypt a record into (inner content type, content).
fn open_record(keys: &mut Keys, header_len: usize, mut body: Vec<u8>) -> io::Result<(u8, Vec<u8>)> {
    if body.len() < 17 {
        return Err(err("short encrypted record"));
    }
    let aad = [APPDATA, 0x03, 0x03, (header_len >> 8) as u8, header_len as u8];
    let tag = body.split_off(body.len() - 16);
    let nonce = keys.nonce();
    if !keys.gcm.open(&nonce, &aad, &mut body, &tag) {
        return Err(err("a record failed to decrypt (corrupted or tampered with)"));
    }
    // Strip padding: the content type is the last non-zero byte.
    while body.last() == Some(&0) {
        body.pop();
    }
    let ty = body.pop().ok_or_else(|| err("empty record"))?;
    Ok((ty, body))
}

fn seal_record(keys: &mut Keys, ty: u8, data: &[u8]) -> Vec<u8> {
    let mut inner = data.to_vec();
    inner.push(ty);
    let len = inner.len() + 16;
    let aad = [APPDATA, 0x03, 0x03, (len >> 8) as u8, len as u8];
    let nonce = keys.nonce();
    let tag = keys.gcm.seal(&nonce, &aad, &mut inner);
    let mut rec = aad.to_vec();
    rec.extend_from_slice(&inner);
    rec.extend_from_slice(&tag);
    rec
}

/// Split complete handshake messages off the front of `buf`.
fn take_messages(buf: &mut Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 4 {
            return out;
        }
        let len = (usize::from(buf[1]) << 16) | (usize::from(buf[2]) << 8) | usize::from(buf[3]);
        if buf.len() < 4 + len {
            return out;
        }
        let msg: Vec<u8> = buf.drain(..4 + len).collect();
        out.push((msg[0], msg));
    }
}

impl TlsStream {
    pub fn connect(mut tcp: TcpStream, host: &str) -> io::Result<TlsStream> {
        let mut secret = [0u8; 32];
        let mut rnd = [0u8; 32];
        let mut session = [0u8; 32];
        random(&mut secret)?;
        random(&mut rnd)?;
        random(&mut session)?;
        let public = x25519_public(&secret);

        let ch = client_hello(host, &rnd, &session, &public);
        let mut rec = vec![HANDSHAKE, 0x03, 0x01];
        rec.extend_from_slice(&(ch.len() as u16).to_be_bytes());
        rec.extend_from_slice(&ch);
        tcp.write_all(&rec)?;
        let mut transcript = ch;

        // ServerHello (plaintext).
        let mut hs_buf = Vec::new();
        let server_hello = loop {
            let (ty, body) = read_record(&mut tcp)?;
            match ty {
                HANDSHAKE => {
                    hs_buf.extend_from_slice(&body);
                    if let Some(m) = take_messages(&mut hs_buf).into_iter().next() {
                        break m;
                    }
                }
                ALERT => return Err(err(format!("the server refused the connection: {}", alert_text(*body.get(1).unwrap_or(&80))))),
                _ => return Err(err("unexpected record before ServerHello")),
            }
        };
        let (ty, sh) = server_hello;
        if ty != 2 {
            return Err(err("expected ServerHello"));
        }
        let b = &sh[4..];
        if b.len() < 38 || b[2..34] == HRR_RANDOM {
            return Err(err("the server asked for a different key exchange (not supported)"));
        }
        let sid_len = usize::from(b[34]);
        let mut at = 35 + sid_len;
        let suite = u16::from_be_bytes([*b.get(at).ok_or_else(|| err("short ServerHello"))?, *b.get(at + 1).ok_or_else(|| err("short ServerHello"))?]);
        if suite != 0x1301 {
            return Err(err(format!("unexpected cipher suite {suite:#06x}")));
        }
        at += 3; // suite + compression
        let ext_len = usize::from(u16::from_be_bytes([b[at], b[at + 1]]));
        at += 2;
        let exts = b.get(at..at + ext_len).ok_or_else(|| err("short ServerHello extensions"))?;
        let (mut version_ok, mut server_key) = (false, None);
        let mut e = 0;
        while e + 4 <= exts.len() {
            let et = u16::from_be_bytes([exts[e], exts[e + 1]]);
            let el = usize::from(u16::from_be_bytes([exts[e + 2], exts[e + 3]]));
            let body = exts.get(e + 4..e + 4 + el).ok_or_else(|| err("bad extension"))?;
            match et {
                43 => version_ok = body == [0x03, 0x04],
                51 if body.len() == 36 && body[..4] == [0x00, 0x1d, 0x00, 0x20] => server_key = Some(<[u8; 32]>::try_from(&body[4..]).unwrap()),
                _ => {}
            }
            e += 4 + el;
        }
        if !version_ok {
            return Err(err("the server does not speak TLS 1.3"));
        }
        let server_key = server_key.ok_or_else(|| err("no X25519 key from the server"))?;
        let shared = x25519(&secret, &server_key);
        if shared == [0; 32] {
            return Err(err("invalid server key"));
        }
        transcript.extend_from_slice(&sh);

        // Key schedule up to the handshake secrets.
        let early = extract(&[], &[0; 32]);
        let derived = expand_label(&early, "derived", &hash(&[]), 32);
        let hs = extract(&derived, &shared);
        let th = hash(&transcript);
        let c_hs = expand_label(&hs, "c hs traffic", &th, 32);
        let s_hs = expand_label(&hs, "s hs traffic", &th, 32);
        let mut s_keys = Keys::new(&s_hs);
        let mut c_keys = Keys::new(&c_hs);

        // EncryptedExtensions, Certificate, CertificateVerify, Finished (encrypted).
        let mut got_finished = false;
        while !got_finished {
            let (ty, body) = read_record(&mut tcp)?;
            match ty {
                CCS => continue, // middlebox compatibility; ignored
                APPDATA => {
                    let len = body.len();
                    let (inner, content) = open_record(&mut s_keys, len, body)?;
                    match inner {
                        HANDSHAKE => hs_buf.extend_from_slice(&content),
                        ALERT => return Err(err(format!("the server ended the handshake: {}", alert_text(*content.get(1).unwrap_or(&80))))),
                        _ => return Err(err("unexpected record in the handshake")),
                    }
                }
                ALERT => return Err(err(format!("the server ended the handshake: {}", alert_text(*body.get(1).unwrap_or(&80))))),
                _ => return Err(err("unexpected record in the handshake")),
            }
            for (mt, msg) in take_messages(&mut hs_buf) {
                if mt == 20 {
                    let finished_key = expand_label(&s_hs, "finished", &[], 32);
                    let expect = hmac_sha256(&finished_key, &hash(&transcript));
                    if !ct_eq(&msg[4..], &expect) {
                        return Err(err("the server's Finished message does not verify"));
                    }
                    transcript.extend_from_slice(&msg);
                    got_finished = true;
                    break;
                }
                transcript.extend_from_slice(&msg);
            }
        }

        // Application secrets, then our Finished.
        let th = hash(&transcript);
        let derived = expand_label(&hs, "derived", &hash(&[]), 32);
        let master = extract(&derived, &[0; 32]);
        let c_ap = expand_label(&master, "c ap traffic", &th, 32);
        let s_ap = expand_label(&master, "s ap traffic", &th, 32);
        let finished_key = expand_label(&c_hs, "finished", &[], 32);
        let verify = hmac_sha256(&finished_key, &th);
        let mut fin = vec![20, 0, 0, 32];
        fin.extend_from_slice(&verify);
        let mut out = vec![CCS, 0x03, 0x03, 0x00, 0x01, 0x01];
        out.extend(seal_record(&mut c_keys, HANDSHAKE, &fin));
        tcp.write_all(&out)?;
        Ok(TlsStream { tcp, read_keys: Keys::new(&s_ap), write_keys: Keys::new(&c_ap), server_secret: s_ap, plain: Vec::new(), plain_pos: 0, closed: false })
    }

    /// Read the next record's application data into the buffer. False at the end.
    fn fill(&mut self) -> io::Result<bool> {
        loop {
            if self.closed {
                return Ok(false);
            }
            let (ty, body) = match read_record(&mut self.tcp) {
                Ok(r) => r,
                // Many servers just close the connection instead of sending close_notify.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    self.closed = true;
                    return Ok(false);
                }
                Err(e) => return Err(e),
            };
            if ty == CCS {
                continue;
            }
            if ty != APPDATA {
                return Err(err("unexpected plaintext record"));
            }
            let len = body.len();
            let (inner, content) = open_record(&mut self.read_keys, len, body)?;
            match inner {
                APPDATA => {
                    self.plain = content;
                    self.plain_pos = 0;
                    if !self.plain.is_empty() {
                        return Ok(true);
                    }
                }
                HANDSHAKE => {
                    // NewSessionTicket (ignored) or KeyUpdate.
                    let mut buf = content;
                    for (mt, msg) in take_messages(&mut buf) {
                        if mt == 24 {
                            self.server_secret = expand_label(&self.server_secret, "traffic upd", &[], 32);
                            self.read_keys = Keys::new(&self.server_secret);
                            if msg.get(4) == Some(&1) {
                                return Err(err("the server asked for a key update (not supported)"));
                            }
                        }
                    }
                }
                ALERT => {
                    if content.get(1) == Some(&0) {
                        self.closed = true;
                        return Ok(false);
                    }
                    return Err(err(format!("the server sent an alert: {}", alert_text(*content.get(1).unwrap_or(&80)))));
                }
                _ => return Err(err("unexpected record type")),
            }
        }
    }
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.plain_pos >= self.plain.len() && !self.fill()? {
            return Ok(0);
        }
        let n = buf.len().min(self.plain.len() - self.plain_pos);
        buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
        self.plain_pos += n;
        Ok(n)
    }
}

impl Write for TlsStream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        for chunk in data.chunks(16000) {
            let rec = seal_record(&mut self.write_keys, APPDATA, chunk);
            self.tcp.write_all(&rec)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flint_crypto::{hex, unhex};

    /// RFC 8448 (TLS 1.3 example handshakes), "Simple 1-RTT Handshake": the key schedule
    /// from the shared secret and transcript hashes given there.
    #[test]
    fn rfc8448_key_schedule() {
        let early = extract(&[], &[0; 32]);
        assert_eq!(hex(&early), "33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a");
        let derived = expand_label(&early, "derived", &hash(&[]), 32);
        assert_eq!(hex(&derived), "6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba");
        let shared = unhex("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d").unwrap();
        let hs = extract(&derived, &shared);
        assert_eq!(hex(&hs), "1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac");
        let th = unhex("860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8").unwrap();
        let c_hs = expand_label(&hs, "c hs traffic", &th, 32);
        assert_eq!(hex(&c_hs), "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21");
        let s_hs = expand_label(&hs, "s hs traffic", &th, 32);
        assert_eq!(hex(&s_hs), "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38");
        assert_eq!(hex(&expand_label(&s_hs, "key", &[], 16)), "3fce516009c21727d0f2e4e86ee403bc");
        assert_eq!(hex(&expand_label(&s_hs, "iv", &[], 12)), "5d313eb2671276ee13000b30");
    }

    #[test]
    fn handshake_messages_are_split() {
        let mut b = vec![8, 0, 0, 2, 0, 0, 11, 0, 0, 1];
        let m = take_messages(&mut b);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, 8);
        assert_eq!(b, [11, 0, 0, 1], "an incomplete message stays buffered");
        b.push(7);
        assert_eq!(take_messages(&mut b).len(), 1);
    }

    #[test]
    fn records_round_trip() {
        let secret = [5u8; 32];
        let mut a = Keys::new(&secret);
        let mut b = Keys::new(&secret);
        let rec = seal_record(&mut a, APPDATA, b"hello");
        let (ty, body) = (rec[0], rec[5..].to_vec());
        assert_eq!(ty, APPDATA);
        let (inner, data) = open_record(&mut b, body.len(), body).unwrap();
        assert_eq!((inner, data.as_slice()), (APPDATA, &b"hello"[..]));
    }
}
