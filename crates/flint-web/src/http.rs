//! HTTP/1.1 GET with redirects, Content-Length or chunked bodies, timeouts and a progress
//! callback.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::dns;
use crate::tls::TlsStream;
use crate::url::Url;

pub struct Response {
    pub status: u16,
    pub url: Url,
    pub body: Vec<u8>,
}

const MAX_REDIRECTS: usize = 8;

fn connect(url: &Url) -> io::Result<TcpStream> {
    let addrs = dns::resolve(&url.host)?;
    let mut last = None;
    for ip in addrs {
        match TcpStream::connect_timeout(&SocketAddr::new(ip, url.port), Duration::from_secs(15)) {
            Ok(s) => {
                s.set_read_timeout(Some(Duration::from_secs(60)))?;
                s.set_write_timeout(Some(Duration::from_secs(60)))?;
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{}: no address", url.host))))
}

enum Conn {
    Plain(TcpStream),
    Tls(Box<TlsStream>),
}

impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(b),
            Conn::Tls(s) => s.read(b),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(b),
            Conn::Tls(s) => s.write(b),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Read the body: exactly `len` bytes, chunked, or until the connection closes.
fn read_body(r: &mut impl BufRead, len: Option<u64>, chunked: bool, limit: u64, progress: &mut dyn FnMut(u64, Option<u64>)) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut buf = vec![0u8; 65536];
    if chunked {
        loop {
            let mut line = String::new();
            r.read_line(&mut line)?;
            let size = u64::from_str_radix(line.trim().split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad("bad chunk size"))?;
            if size == 0 {
                // trailers until an empty line
                loop {
                    let mut t = String::new();
                    if r.read_line(&mut t)? == 0 || t.trim().is_empty() {
                        break;
                    }
                }
                return Ok(body);
            }
            if body.len() as u64 + size > limit {
                return Err(bad("download too large"));
            }
            let mut left = size;
            while left > 0 {
                let n = r.read(&mut buf[..left.min(65536) as usize])?;
                if n == 0 {
                    return Err(bad("connection closed in the middle of the download"));
                }
                body.extend_from_slice(&buf[..n]);
                left -= n as u64;
                progress(body.len() as u64, None);
            }
            let mut crlf = String::new();
            r.read_line(&mut crlf)?;
        }
    }
    loop {
        if len.is_some_and(|l| body.len() as u64 >= l) {
            break;
        }
        let want = len.map_or(65536, |l| (l - body.len() as u64).min(65536) as usize);
        let n = r.read(&mut buf[..want])?;
        if n == 0 {
            if len.is_some() {
                return Err(bad("connection closed in the middle of the download"));
            }
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() as u64 > limit {
            return Err(bad("download too large"));
        }
        progress(body.len() as u64, len);
    }
    Ok(body)
}

/// GET `url`, following redirects. Non-2xx statuses are returned, not errors.
pub fn get(url: &str, limit: u64, progress: &mut dyn FnMut(u64, Option<u64>)) -> io::Result<Response> {
    let mut url = Url::parse(url).map_err(bad)?;
    for _ in 0..=MAX_REDIRECTS {
        let tcp = connect(&url)?;
        let mut conn = if url.https { Conn::Tls(Box::new(TlsStream::connect(tcp, &url.host)?)) } else { Conn::Plain(tcp) };
        let host = if (url.https && url.port == 443) || (!url.https && url.port == 80) { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
        let req = format!("GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pkg (Flint Linux)\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n", url.path);
        conn.write_all(req.as_bytes())?;
        conn.flush()?;
        let mut r = BufReader::new(conn);
        let mut status_line = String::new();
        r.read_line(&mut status_line)?;
        let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| bad(format!("{url}: not an HTTP response")))?;
        let (mut len, mut chunked, mut location) = (None, false, None);
        loop {
            let mut h = String::new();
            if r.read_line(&mut h)? == 0 {
                break;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                let v = v.trim();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => len = v.parse().ok(),
                    "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
                    "location" => location = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let loc = location.ok_or_else(|| bad(format!("{url}: redirect without a Location")))?;
            let next = url.join(&loc).map_err(bad)?;
            if url.https && !next.https {
                return Err(bad(format!("{url}: refusing to follow a redirect from https to http")));
            }
            url = next;
            continue;
        }
        let body = read_body(&mut r, len, chunked, limit, progress)?;
        return Ok(Response { status, url, body });
    }
    Err(bad(format!("{url}: too many redirects")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bodies() {
        let mut p = |_: u64, _: Option<u64>| {};
        let mut c = Cursor::new(b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\nX-Trailer: y\r\n\r\n".to_vec());
        assert_eq!(read_body(&mut c, None, true, 1000, &mut p).unwrap(), b"Wikipedia");
        let mut c = Cursor::new(b"hello world".to_vec());
        assert_eq!(read_body(&mut c, Some(5), false, 1000, &mut p).unwrap(), b"hello");
        let mut c = Cursor::new(b"abc".to_vec());
        assert!(read_body(&mut c, Some(5), false, 1000, &mut p).is_err(), "cut short");
        let mut c = Cursor::new(b"until close".to_vec());
        assert_eq!(read_body(&mut c, None, false, 1000, &mut p).unwrap(), b"until close");
        let mut c = Cursor::new(vec![b'x'; 2000]);
        assert!(read_body(&mut c, None, false, 1000, &mut p).is_err(), "limit");
    }
}
