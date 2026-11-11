//! http:// and https:// URLs.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, starting with '/'.
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let (https, rest) = if let Some(r) = s.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = s.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(format!("{s}: only http:// and https:// URLs are supported"));
        };
        let (authority, path) = match rest.find('/') {
            Some(k) => (&rest[..k], &rest[k..]),
            None => (rest, "/"),
        };
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = if let Some(h) = authority.strip_prefix('[') {
            let end = h.find(']').ok_or_else(|| format!("{s}: bad IPv6 address"))?;
            let port = h[end + 1..].strip_prefix(':').map(str::parse::<u16>).transpose().map_err(|_| format!("{s}: bad port"))?;
            (h[..end].to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p.parse::<u16>().map_err(|_| format!("{s}: bad port"))?)),
                None => (authority.to_string(), None),
            }
        };
        if host.is_empty() {
            return Err(format!("{s}: no host"));
        }
        let path = path.split('#').next().unwrap_or("/");
        Ok(Url { https, port: port.unwrap_or(if https { 443 } else { 80 }), host: host.to_ascii_lowercase(), path: if path.is_empty() { "/".into() } else { path.to_string() } })
    }

    /// Resolve a Location header (absolute or relative to this URL).
    pub fn join(&self, location: &str) -> Result<Url, String> {
        if location.starts_with("http://") || location.starts_with("https://") {
            return Url::parse(location);
        }
        if let Some(rest) = location.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.https { "https" } else { "http" }));
        }
        let path = if location.starts_with('/') {
            location.to_string()
        } else {
            let base = self.path.split('?').next().unwrap_or("/");
            let dir = &base[..base.rfind('/').map_or(0, |k| k + 1)];
            format!("{dir}{location}")
        };
        Ok(Url { path, ..self.clone() })
    }

    /// This URL with `name` appended as a path component (for repository files).
    pub fn file(&self, name: &str) -> Url {
        let base = self.path.trim_end_matches('/');
        Url { path: format!("{base}/{name}"), ..self.clone() }
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.https { "https" } else { "http" };
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            write!(f, "{scheme}://{}{}", self.host, self.path)
        } else {
            write!(f, "{scheme}://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let u = Url::parse("https://Raw.GitHubusercontent.com/a/b/main/repo").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.path.as_str()), (true, "raw.githubusercontent.com", 443, "/a/b/main/repo"));
        assert_eq!(u.file("INDEX").path, "/a/b/main/repo/INDEX");
        assert_eq!(Url::parse("http://h:8080").unwrap().path, "/");
        assert_eq!(Url::parse("http://h:8080/x").unwrap().port, 8080);
        assert_eq!(Url::parse("http://[::1]:81/").unwrap().host, "::1");
        assert!(Url::parse("ftp://x/").is_err());
        assert_eq!(u.join("/other").unwrap().to_string(), "https://raw.githubusercontent.com/other");
        assert_eq!(u.join("sibling").unwrap().path, "/a/b/main/sibling");
        assert_eq!(u.join("https://x.org/y").unwrap().host, "x.org");
        assert_eq!(Url::parse("http://h:8080/x").unwrap().to_string(), "http://h:8080/x");
    }
}
