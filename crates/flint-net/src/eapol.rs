//! The WPA2 4-way handshake and group key handshake (IEEE 802.11-2016 12.7), supplicant
//! side, for WPA2-Personal with CCMP: key descriptor version 2 (HMAC-SHA1 MIC, AES key wrap).

use crate::crypto::{aes_unwrap, hmac_sha1, prf};

const INFO_VERSION_2: u16 = 2;
const INFO_PAIRWISE: u16 = 1 << 3;
const INFO_INSTALL: u16 = 1 << 6;
const INFO_ACK: u16 = 1 << 7;
const INFO_MIC: u16 = 1 << 8;
const INFO_SECURE: u16 = 1 << 9;
const INFO_ENC_DATA: u16 = 1 << 12;

const MIC_AT: usize = 81;
const DATA_AT: usize = 99;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFrame {
    pub version: u8,
    pub info: u16,
    pub key_len: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub rsc: [u8; 8],
    pub mic: [u8; 16],
    pub data: Vec<u8>,
}

pub fn parse(frame: &[u8]) -> Option<KeyFrame> {
    if frame.len() < DATA_AT || frame[1] != 3 {
        return None;
    }
    let body_len = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
    if body_len + 4 > frame.len() || frame[4] != 2 {
        return None; // not an RSN key descriptor
    }
    let data_len = usize::from(u16::from_be_bytes([frame[97], frame[98]]));
    let data = frame.get(DATA_AT..DATA_AT + data_len)?.to_vec();
    Some(KeyFrame {
        version: frame[0],
        info: u16::from_be_bytes([frame[5], frame[6]]),
        key_len: u16::from_be_bytes([frame[7], frame[8]]),
        replay: u64::from_be_bytes(frame[9..17].try_into().unwrap()),
        nonce: frame[17..49].try_into().unwrap(),
        rsc: frame[65..73].try_into().unwrap(),
        mic: frame[MIC_AT..MIC_AT + 16].try_into().unwrap(),
        data,
    })
}

/// Serialize with a zero MIC; `sign` fills it in.
pub fn build(k: &KeyFrame) -> Vec<u8> {
    let mut f = vec![k.version, 3, 0, 0, 2];
    f.extend_from_slice(&k.info.to_be_bytes());
    f.extend_from_slice(&k.key_len.to_be_bytes());
    f.extend_from_slice(&k.replay.to_be_bytes());
    f.extend_from_slice(&k.nonce);
    f.extend_from_slice(&[0; 16]); // IV
    f.extend_from_slice(&k.rsc);
    f.extend_from_slice(&[0; 8]); // reserved
    f.extend_from_slice(&[0; 16]); // MIC
    f.extend_from_slice(&(k.data.len() as u16).to_be_bytes());
    f.extend_from_slice(&k.data);
    let body = (f.len() - 4) as u16;
    f[2..4].copy_from_slice(&body.to_be_bytes());
    f
}

pub fn mic(kck: &[u8; 16], frame: &[u8]) -> [u8; 16] {
    let mut copy = frame.to_vec();
    copy[MIC_AT..MIC_AT + 16].fill(0);
    hmac_sha1(kck, &[&copy])[..16].try_into().unwrap()
}

pub fn sign(kck: &[u8; 16], frame: &mut [u8]) {
    let m = mic(kck, frame);
    frame[MIC_AT..MIC_AT + 16].copy_from_slice(&m);
}

/// Constant-time comparison: a MIC check must not leak how many bytes matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

pub fn derive_ptk(pmk: &[u8; 32], aa: &[u8; 6], spa: &[u8; 6], anonce: &[u8; 32], snonce: &[u8; 32]) -> Ptk {
    let mut data = Vec::with_capacity(76);
    let (lo, hi) = if aa < spa { (aa, spa) } else { (spa, aa) };
    data.extend_from_slice(lo);
    data.extend_from_slice(hi);
    let (lo, hi) = if anonce < snonce { (anonce, snonce) } else { (snonce, anonce) };
    data.extend_from_slice(lo);
    data.extend_from_slice(hi);
    let k = prf(pmk, "Pairwise key expansion", &data, 48);
    Ptk { kck: k[..16].try_into().unwrap(), kek: k[16..32].try_into().unwrap(), tk: k[32..48].try_into().unwrap() }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Send(Vec<u8>),
    InstallPairwise([u8; 16]),
    /// key index, key, receive sequence counter (6 bytes)
    InstallGroup(u8, Vec<u8>, [u8; 6]),
    Authorize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a frame we act on (or a replay); ignore it.
    Ignored,
    /// The MIC did not verify: with message 3 this usually means a wrong password.
    BadMic,
    Malformed,
}

pub struct Supplicant {
    pmk: [u8; 32],
    aa: [u8; 6],
    spa: [u8; 6],
    our_ie: Vec<u8>,
    snonce: [u8; 32],
    anonce: Option<[u8; 32]>,
    ptk: Option<Ptk>,
    replay: Option<u64>,
    pub complete: bool,
    /// How many message 1s arrived: the AP repeats it when it rejects message 2.
    pub msg1_count: u32,
}

/// The GTK from decrypted key data: (key index, key).
pub fn find_gtk(data: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut at = 0;
    while at + 2 <= data.len() {
        let (id, len) = (data[at], usize::from(data[at + 1]));
        if id == 0xDD && len == 0 {
            break; // padding
        }
        let body = data.get(at + 2..at + 2 + len)?;
        if id == 0xDD && body.len() >= 6 && body[..4] == [0x00, 0x0F, 0xAC, 0x01] {
            return Some((body[4] & 3, body[6..].to_vec()));
        }
        at += 2 + len;
    }
    None
}

impl Supplicant {
    pub fn new(pmk: [u8; 32], aa: [u8; 6], spa: [u8; 6], our_ie: Vec<u8>, snonce: [u8; 32]) -> Supplicant {
        Supplicant { pmk, aa, spa, our_ie, snonce, anonce: None, ptk: None, replay: None, complete: false, msg1_count: 0 }
    }

    fn check_replay(&self, k: &KeyFrame) -> Result<(), Error> {
        match self.replay {
            Some(r) if k.replay <= r => Err(Error::Ignored),
            _ => Ok(()),
        }
    }

    fn verified(&self, frame: &[u8], k: &KeyFrame) -> Result<Ptk, Error> {
        let ptk = self.ptk.clone().ok_or(Error::Ignored)?;
        if !same(&mic(&ptk.kck, frame), &k.mic) {
            return Err(Error::BadMic);
        }
        Ok(ptk)
    }

    fn reply(&self, k: &KeyFrame, info: u16, nonce: [u8; 32], data: Vec<u8>, kck: &[u8; 16]) -> Vec<u8> {
        let mut f = build(&KeyFrame { version: k.version, info, key_len: 0, replay: k.replay, nonce, rsc: [0; 8], mic: [0; 16], data });
        sign(kck, &mut f);
        f
    }

    fn group_key(&self, k: &KeyFrame, kek: &[u8; 16]) -> Result<(u8, Vec<u8>, [u8; 6]), Error> {
        if k.info & INFO_ENC_DATA == 0 {
            return Err(Error::Malformed);
        }
        let plain = aes_unwrap(kek, &k.data).ok_or(Error::Malformed)?;
        let (idx, gtk) = find_gtk(&plain).ok_or(Error::Malformed)?;
        let mut rsc = [0u8; 6];
        rsc.copy_from_slice(&k.rsc[..6]);
        Ok((idx, gtk, rsc))
    }

    /// Handle one EAPOL-Key frame from the AP.
    pub fn handle(&mut self, frame: &[u8]) -> Result<Vec<Action>, Error> {
        let k = parse(frame).ok_or(Error::Malformed)?;
        if k.info & 7 != INFO_VERSION_2 || k.info & INFO_ACK == 0 {
            return Err(Error::Ignored);
        }
        let pairwise = k.info & INFO_PAIRWISE != 0;
        if pairwise && k.info & INFO_MIC == 0 {
            // Message 1: the AP's nonce. Derive the keys and answer with ours.
            self.check_replay(&k)?;
            self.msg1_count += 1;
            let ptk = derive_ptk(&self.pmk, &self.aa, &self.spa, &k.nonce, &self.snonce);
            self.anonce = Some(k.nonce);
            let msg2 = self.reply(&k, INFO_VERSION_2 | INFO_PAIRWISE | INFO_MIC, self.snonce, self.our_ie.clone(), &ptk.kck);
            self.ptk = Some(ptk);
            self.replay = Some(k.replay);
            return Ok(vec![Action::Send(msg2)]);
        }
        if pairwise {
            // Message 3: keys are confirmed; install them.
            self.check_replay(&k)?;
            if Some(k.nonce) != self.anonce || k.info & INFO_INSTALL == 0 {
                return Err(Error::Malformed);
            }
            let ptk = self.verified(frame, &k)?;
            let (idx, gtk, rsc) = self.group_key(&k, &ptk.kek)?;
            self.replay = Some(k.replay);
            let msg4 = self.reply(&k, INFO_VERSION_2 | INFO_PAIRWISE | INFO_MIC | INFO_SECURE, [0; 32], Vec::new(), &ptk.kck);
            self.complete = true;
            return Ok(vec![Action::Send(msg4), Action::InstallPairwise(ptk.tk), Action::InstallGroup(idx, gtk, rsc), Action::Authorize]);
        }
        // Group key handshake message 1: a new group key.
        if !self.complete {
            return Err(Error::Ignored);
        }
        self.check_replay(&k)?;
        let ptk = self.verified(frame, &k)?;
        let (idx, gtk, rsc) = self.group_key(&k, &ptk.kek)?;
        self.replay = Some(k.replay);
        let reply = self.reply(&k, INFO_VERSION_2 | INFO_MIC | INFO_SECURE, [0; 32], Vec::new(), &ptk.kck);
        Ok(vec![Action::Send(reply), Action::InstallGroup(idx, gtk, rsc)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{aes_wrap, wpa_psk};
    use crate::nl80211::{rsn_ie, CIPHER_CCMP};

    const AA: [u8; 6] = [2, 0, 0, 0, 0, 1];
    const SPA: [u8; 6] = [2, 0, 0, 0, 0, 2];

    fn gtk_data(idx: u8, gtk: &[u8]) -> Vec<u8> {
        let mut d = rsn_ie(CIPHER_CCMP);
        d.extend_from_slice(&[0xDD, (6 + gtk.len()) as u8, 0x00, 0x0F, 0xAC, 0x01, idx, 0]);
        d.extend_from_slice(gtk);
        // Key data is padded to a multiple of 8: 0xDD, then zeros.
        if d.len() % 8 != 0 || d.len() < 16 {
            d.push(0xDD);
            while d.len() % 8 != 0 || d.len() < 16 {
                d.push(0);
            }
        }
        d
    }

    /// The access point's side, enough to drive the supplicant.
    fn ap_frame(info: u16, replay: u64, nonce: [u8; 32], data: Vec<u8>, rsc: [u8; 8], kck: Option<&[u8; 16]>) -> Vec<u8> {
        let mut f = build(&KeyFrame { version: 2, info, key_len: 16, replay, nonce, rsc, mic: [0; 16], data });
        if let Some(k) = kck {
            sign(k, &mut f);
        }
        f
    }

    fn handshake(ap_password: &[u8]) -> (Supplicant, Result<Vec<Action>, Error>, Ptk) {
        let pmk = wpa_psk(b"correct horse", b"home");
        let mut s = Supplicant::new(pmk, AA, SPA, rsn_ie(CIPHER_CCMP), [7; 32]);
        let anonce = [9u8; 32];
        let msg1 = ap_frame(INFO_VERSION_2 | INFO_PAIRWISE | INFO_ACK, 1, anonce, Vec::new(), [0; 8], None);
        let acts = s.handle(&msg1).unwrap();
        let Action::Send(msg2) = &acts[0] else { panic!() };
        // The AP checks message 2 with the key from ITS password.
        let ap_ptk = derive_ptk(&wpa_psk(ap_password, b"home"), &AA, &SPA, &anonce, &parse(msg2).unwrap().nonce);
        let k2 = parse(msg2).unwrap();
        let mic_ok = same(&mic(&ap_ptk.kck, msg2), &k2.mic);
        assert_eq!(mic_ok, ap_password == b"correct horse");
        assert_eq!(k2.data, rsn_ie(CIPHER_CCMP));
        assert_eq!(k2.replay, 1);
        let gtk = [0x55u8; 16];
        let data = aes_wrap(&ap_ptk.kek, &gtk_data(1, &gtk));
        let msg3 = ap_frame(INFO_VERSION_2 | INFO_PAIRWISE | INFO_ACK | INFO_MIC | INFO_INSTALL | INFO_SECURE | INFO_ENC_DATA, 2, anonce, data, [3, 0, 0, 0, 0, 0, 0, 0], Some(&ap_ptk.kck));
        let r = s.handle(&msg3);
        (s, r, ap_ptk)
    }

    #[test]
    fn four_way_handshake() {
        let (mut s, r, ptk) = handshake(b"correct horse");
        let acts = r.unwrap();
        assert_eq!(acts.len(), 4);
        let Action::Send(msg4) = &acts[0] else { panic!() };
        let k4 = parse(msg4).unwrap();
        assert!(same(&mic(&ptk.kck, msg4), &k4.mic));
        assert_eq!(k4.info, INFO_VERSION_2 | INFO_PAIRWISE | INFO_MIC | INFO_SECURE);
        assert_eq!(acts[1], Action::InstallPairwise(ptk.tk));
        assert_eq!(acts[2], Action::InstallGroup(1, vec![0x55; 16], [3, 0, 0, 0, 0, 0]));
        assert_eq!(acts[3], Action::Authorize);
        assert!(s.complete);

        // Group rekey.
        let data = aes_wrap(&ptk.kek, &gtk_data(2, &[0x66; 16]));
        let g1 = ap_frame(INFO_VERSION_2 | INFO_ACK | INFO_MIC | INFO_SECURE | INFO_ENC_DATA, 3, [0; 32], data, [0; 8], Some(&ptk.kck));
        let acts = s.handle(&g1).unwrap();
        assert_eq!(acts[1], Action::InstallGroup(2, vec![0x66; 16], [0; 6]));
        assert_eq!(s.handle(&g1), Err(Error::Ignored), "a replayed frame is ignored");
    }

    #[test]
    fn wrong_password_is_detected() {
        let (s, r, _) = handshake(b"wrong horse");
        assert_eq!(r, Err(Error::BadMic));
        assert!(!s.complete);
    }

    #[test]
    fn junk_is_rejected() {
        let mut s = Supplicant::new([0; 32], AA, SPA, Vec::new(), [0; 32]);
        assert_eq!(s.handle(&[1, 3, 0, 0]), Err(Error::Malformed));
        let mut f = ap_frame(INFO_VERSION_2 | INFO_PAIRWISE | INFO_ACK, 1, [1; 32], vec![1, 2, 3], [0; 8], None);
        f[98] = 200; // data length beyond the frame
        assert_eq!(s.handle(&f), Err(Error::Malformed));
        let g = ap_frame(INFO_VERSION_2 | INFO_ACK | INFO_MIC, 1, [0; 32], Vec::new(), [0; 8], None);
        assert_eq!(s.handle(&g), Err(Error::Ignored), "group message before the 4-way handshake");
    }
}
