//! Fetching things from the network: DNS, TLS 1.3, HTTP/1.1. Used by pkg.
//!
//! Trust: the TLS client encrypts the connection but does not check the server's
//! certificate. Whatever is downloaded must therefore carry its own proof: pkg only accepts
//! repository indexes signed with a key it trusts, and packages whose SHA-256 is in that
//! signed index. Someone in the middle can see which files are fetched and can block them,
//! but cannot make pkg install anything that was not signed.

pub mod dns;
pub mod fetch;
pub mod http;
pub mod tls;
pub mod url;

pub use http::{get, Response};
pub use url::Url;

pub use fetch::fetch;
