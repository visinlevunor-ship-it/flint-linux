//! HKDF-SHA256 (RFC 5869) and the TLS 1.3 labels on top of it (RFC 8446 section 7.1).

use crate::hmac_sha256;

pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    let zero = [0u8; 32];
    hmac_sha256(if salt.is_empty() { &zero } else { salt }, ikm)
}

pub fn expand(prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 32);
    let mut t: Vec<u8> = Vec::new();
    let mut i = 1u8;
    while out.len() < len {
        let mut msg = t.clone();
        msg.extend_from_slice(info);
        msg.push(i);
        t = hmac_sha256(prk, &msg).to_vec();
        out.extend_from_slice(&t);
        i += 1;
    }
    out.truncate(len);
    out
}

pub fn expand_label(secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let full = format!("tls13 {label}");
    let mut info = Vec::with_capacity(4 + full.len() + context.len());
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push(full.len() as u8);
    info.extend_from_slice(full.as_bytes());
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    expand(secret, &info, len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hex, unhex};

    #[test]
    fn rfc5869_case1() {
        let ikm = [0x0b; 22];
        let salt = unhex("000102030405060708090a0b0c").unwrap();
        let info = unhex("f0f1f2f3f4f5f6f7f8f9").unwrap();
        let prk = extract(&salt, &ikm);
        assert_eq!(hex(&prk), "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5");
        assert_eq!(hex(&expand(&prk, &info, 42)), "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865");
    }
}
