//! Repositories on the network and how pkg trusts them.
//!
//! A remote repository is a URL with three kinds of files: `INDEX` (every package with its
//! size and SHA-256), `INDEX.sig` (an Ed25519 signature of INDEX) and the packages. pkg
//! downloads INDEX and INDEX.sig, checks the signature against the keys in
//! ROOT/etc/flint/keys/*.pub, and refuses an INDEX older than the last one it accepted
//! (so nobody can feed it an old, signed list with known-vulnerable packages). Packages are
//! then checked against the SHA-256 in that signed INDEX. That chain is the whole trust
//! model: the HTTPS connection itself is not trusted.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flint_crypto::curve25519::{ed25519_public, ed25519_sign, ed25519_verify};
use flint_crypto::{hex, random, sha256, unhex};

pub const SIG_MAGIC: &str = "flint-signature";
const MAX_INDEX: u64 = 64 << 20;
const MAX_PACKAGE: u64 = 2 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoSpec {
    Local(PathBuf),
    Remote(String),
}

impl std::fmt::Display for RepoSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepoSpec::Local(p) => write!(f, "{}", p.display()),
            RepoSpec::Remote(u) => f.write_str(u),
        }
    }
}

pub fn parse_conf(text: &str) -> Result<Vec<RepoSpec>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some(loc) = line.strip_prefix("repo").map(str::trim) else {
            return Err(format!("pkg.conf: unknown line '{line}' (expected: repo PATH-OR-URL)"));
        };
        if loc.is_empty() {
            return Err("pkg.conf: 'repo' needs a path or URL".into());
        }
        out.push(spec(loc)?);
    }
    Ok(out)
}

pub fn spec(loc: &str) -> Result<RepoSpec, String> {
    if loc.starts_with("https://") || loc.starts_with("http://") {
        flint_web::Url::parse(loc)?;
        return Ok(RepoSpec::Remote(loc.trim_end_matches('/').to_string()));
    }
    let path = loc.strip_prefix("file://").unwrap_or(loc);
    if path.contains("://") {
        return Err(format!("'{loc}': repositories are directories or http(s):// URLs"));
    }
    Ok(RepoSpec::Local(PathBuf::from(path)))
}

/// Where a remote repository's verified INDEX and downloaded packages are kept.
pub fn cache_dir(root: &Path, url: &str) -> PathBuf {
    let id = hex(&sha256::digest(url.as_bytes()));
    root.join("var/cache/pkg/remote").join(&id[..16])
}

// ---------------------------------------------------------------------------------------
// Keys and signatures

pub struct Key {
    pub name: String,
    pub public: [u8; 32],
}

/// A public key file: `ed25519 HEX [NAME]`.
pub fn parse_public(text: &str, fallback_name: &str) -> Result<Key, String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).ok_or("empty key file")?;
    let mut w = line.split_whitespace();
    if w.next() != Some("ed25519") {
        return Err("not an ed25519 public key (expected: ed25519 HEX NAME)".into());
    }
    let public: [u8; 32] = w.next().and_then(unhex).and_then(|v| v.try_into().ok()).ok_or("bad public key")?;
    let name = w.next().unwrap_or(fallback_name).to_string();
    Ok(Key { name, public })
}

pub fn trusted_keys(root: &Path) -> Vec<Key> {
    let dir = root.join("etc/flint/keys");
    let mut keys = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "pub")).collect();
        files.sort();
        for f in files {
            let stem = f.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            match fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| parse_public(&t, &stem)) {
                Ok(k) => keys.push(k),
                Err(e) => eprintln!("pkg: ignoring {}: {e}", f.display()),
            }
        }
    }
    keys
}

/// The secret key file: `flint-secret-key ed25519 SEEDHEX`.
pub fn parse_secret(text: &str) -> Result<[u8; 32], String> {
    let mut w = text.split_whitespace();
    if w.next() != Some("flint-secret-key") || w.next() != Some("ed25519") {
        return Err("not a pkg secret key file".into());
    }
    w.next().and_then(unhex).and_then(|v| v.try_into().ok()).ok_or_else(|| "bad secret key".to_string())
}

/// Make a key pair: NAME.key (secret, mode 0600) and NAME.pub in `dir`.
pub fn new_key(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf), String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err("a key name is letters, digits, '-', '_' or '.'".into());
    }
    let sk = dir.join(format!("{name}.key"));
    let pk = dir.join(format!("{name}.pub"));
    if sk.exists() || pk.exists() {
        return Err(format!("{} or {} already exists", sk.display(), pk.display()));
    }
    let mut seed = [0u8; 32];
    random(&mut seed).map_err(|e| format!("no random numbers: {e}"))?;
    let public = ed25519_public(&seed);
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&sk).map_err(|e| format!("{}: {e}", sk.display()))?;
        writeln!(f, "flint-secret-key ed25519 {}", hex(&seed)).map_err(|e| e.to_string())?;
    }
    fs::write(&pk, format!("ed25519 {} {name}\n", hex(&public))).map_err(|e| format!("{}: {e}", pk.display()))?;
    Ok((sk, pk))
}

pub fn sign(index: &[u8], seed: &[u8; 32]) -> String {
    let public = ed25519_public(seed);
    let sig = ed25519_sign(seed, index);
    format!("{SIG_MAGIC} ed25519 {} {}\n", hex(&public), hex(&sig))
}

/// Check INDEX.sig against the trusted keys. Ok(name of the key that signed it).
pub fn verify(index: &[u8], sig_text: &str, keys: &[Key]) -> Result<String, String> {
    let mut w = sig_text.split_whitespace();
    if w.next() != Some(SIG_MAGIC) || w.next() != Some("ed25519") {
        return Err("INDEX.sig is not a pkg signature".into());
    }
    let public: [u8; 32] = w.next().and_then(unhex).and_then(|v| v.try_into().ok()).ok_or("INDEX.sig: bad key")?;
    let sig: [u8; 64] = w.next().and_then(unhex).and_then(|v| v.try_into().ok()).ok_or("INDEX.sig: bad signature")?;
    let Some(key) = keys.iter().find(|k| k.public == public) else {
        return Err(format!("signed with a key that is not trusted ({}...); trusted keys live in /etc/flint/keys", &hex(&public)[..16]));
    };
    if !ed25519_verify(&key.public, index, &sig) {
        return Err(format!("the signature does not match INDEX (key {}): the file was changed or damaged", key.name));
    }
    Ok(key.name.clone())
}

/// `timestamp: N` from the header block of an INDEX (0 if there is none).
pub fn timestamp(index: &str) -> u64 {
    index.lines().next().and_then(|l| l.strip_prefix("timestamp:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// Downloads

fn download(url: &str, limit: u64, label: &str) -> Result<Vec<u8>, String> {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let mut shown = 0u64;
    let mut progress = |got: u64, total: Option<u64>| {
        if !tty || got < shown + 128 * 1024 {
            return;
        }
        shown = got;
        match total {
            Some(t) if t > 0 => eprint!("\r  {label}: {} / {} KiB", got / 1024, t / 1024),
            _ => eprint!("\r  {label}: {} KiB", got / 1024),
        }
    };
    let r = flint_web::get(url, limit, &mut progress).map_err(|e| format!("{url}: {e}"));
    if shown > 0 {
        eprint!("\r\x1b[K");
    }
    let r = r?;
    if r.status == 404 {
        return Err(format!("{url}: not found (404)"));
    }
    if !(200..300).contains(&r.status) {
        return Err(format!("{url}: HTTP status {}", r.status));
    }
    Ok(r.body)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("bad path")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.part", path.file_name().map_or("x".into(), |n| n.to_string_lossy().into_owned())));
    fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub struct Updated {
    pub key: String,
    pub packages: usize,
    pub changed: bool,
}

/// Fetch and verify a remote repository's INDEX, then store it in the cache.
pub fn update(root: &Path, url: &str) -> Result<Updated, String> {
    let keys = trusted_keys(root);
    if keys.is_empty() {
        return Err(format!("{url}: no trusted keys, so nothing from the network can be checked (put the repository's public key in {}/etc/flint/keys/)", root.display().to_string().trim_end_matches('/')));
    }
    let index = download(&format!("{url}/INDEX"), MAX_INDEX, "INDEX")?;
    let sig = download(&format!("{url}/INDEX.sig"), 4096, "INDEX.sig")?;
    let sig_text = String::from_utf8_lossy(&sig).into_owned();
    let key = verify(&index, &sig_text, &keys).map_err(|e| format!("{url}: {e}"))?;
    let text = String::from_utf8(index.clone()).map_err(|_| format!("{url}: INDEX is not text"))?;
    let entries = crate::control::parse_index(&text).map_err(|e| format!("{url}: {e}"))?;
    let dir = cache_dir(root, url);
    let old = fs::read_to_string(dir.join("INDEX")).ok();
    let (old_ts, new_ts) = (old.as_deref().map_or(0, timestamp), timestamp(&text));
    if new_ts < old_ts {
        return Err(format!("{url}: the INDEX offered is older than the one already accepted (timestamp {new_ts} < {old_ts}); refusing it"));
    }
    let changed = old.as_deref() != Some(text.as_str());
    write_atomic(&dir.join("INDEX.sig"), &sig)?;
    write_atomic(&dir.join("INDEX"), &index)?;
    fs::write(dir.join("URL"), format!("{url}\n")).map_err(|e| e.to_string())?;
    Ok(Updated { key, packages: entries.len(), changed })
}

/// The cached INDEX of a remote repository, checked again (the cache could have been
/// tampered with). None if `pkg update` has not run yet.
pub fn cached_index(root: &Path, url: &str) -> Result<Option<String>, String> {
    let dir = cache_dir(root, url);
    let Ok(index) = fs::read(dir.join("INDEX")) else { return Ok(None) };
    let sig = fs::read_to_string(dir.join("INDEX.sig")).map_err(|_| format!("{url}: cached INDEX has no signature; run pkg update"))?;
    verify(&index, &sig, &trusted_keys(root)).map_err(|e| format!("{url} (cached): {e}; run pkg update"))?;
    String::from_utf8(index).map(Some).map_err(|_| format!("{url}: cached INDEX is not text"))
}

/// Make sure `path` holds the package with this size and SHA-256, downloading it from
/// `url` if needed.
pub fn fetch_package(url: &str, path: &Path, size: u64, sha256_hex: &str) -> Result<(), String> {
    if let Ok(data) = fs::read(path) {
        if data.len() as u64 == size && hex(&sha256::digest(&data)) == sha256_hex {
            return Ok(());
        }
    }
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    println!("downloading {name} ({} KiB)", size.div_ceil(1024));
    let data = download(url, MAX_PACKAGE.min(size + 1), &name)?;
    if data.len() as u64 != size {
        return Err(format!("{name}: got {} bytes, the signed INDEX says {size}", data.len()));
    }
    let got = hex(&sha256::digest(&data));
    if got != sha256_hex {
        return Err(format!("{name}: SHA-256 {got} does not match the signed INDEX ({sha256_hex}); refusing it"));
    }
    write_atomic(path, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config() {
        let c = parse_conf("# repos\nrepo /var/cache/pkg/repo\nrepo https://example.org/flint/  # remote\nrepo file:///x\n").unwrap();
        assert_eq!(c, [RepoSpec::Local("/var/cache/pkg/repo".into()), RepoSpec::Remote("https://example.org/flint".into()), RepoSpec::Local("/x".into())]);
        assert!(parse_conf("mirror x").is_err());
        assert!(parse_conf("repo ftp://x").is_err());
        assert_ne!(cache_dir(Path::new("/"), "https://a"), cache_dir(Path::new("/"), "https://b"));
    }

    #[test]
    fn signing() {
        let dir = std::env::temp_dir().join(format!("pkg-keys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (sk, pk) = new_key(&dir, "test").unwrap();
        assert!(new_key(&dir, "test").is_err(), "never overwrites a key");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&sk).unwrap().permissions().mode() & 0o777, 0o600);
        let seed = parse_secret(&fs::read_to_string(&sk).unwrap()).unwrap();
        let key = parse_public(&fs::read_to_string(&pk).unwrap(), "x").unwrap();
        assert_eq!(key.name, "test");
        let index = b"timestamp: 5\n\nname: a\n";
        let sig = sign(index, &seed);
        assert_eq!(verify(index, &sig, &[key]).unwrap(), "test");
        let key = parse_public(&fs::read_to_string(&pk).unwrap(), "x").unwrap();
        assert!(verify(b"timestamp: 5\n\nname: b\n", &sig, &[key]).is_err(), "a changed INDEX fails");
        assert!(verify(index, &sig, &[]).is_err(), "an unknown key fails");
        assert_eq!(timestamp("timestamp: 42\n\nname: x\n"), 42);
        assert_eq!(timestamp("name: x\n"), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
