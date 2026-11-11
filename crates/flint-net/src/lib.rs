//! Networking for Flint: `netd` (DHCP on wired interfaces, Wi-Fi with WPA2-Personal),
//! `net` (status and Wi-Fi commands) and `ping`. Everything here is Flint's own: netlink,
//! DHCP, DNS, the 802.11 handshake and its cryptography.

pub mod cli;
pub mod crypto;
pub mod dhcp;
pub mod dns;
pub mod eapol;
pub mod netd;
pub mod netlink;
pub mod nl80211;
pub mod packet;
pub mod ping;
pub mod rtnl;
pub mod wifi;

pub use cli::net;
pub use netd::netd;
pub use ping::ping;
