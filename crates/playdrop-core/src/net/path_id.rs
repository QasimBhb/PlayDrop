//! The path id of protocol.md 3.5: one radio answers on several addresses (link-local IPv6
//! and IPv4 on one Wi-Fi interface), and measuring each costs up to 4 MiB to learn nothing.
//! A `200` to `GET /v1/probe?bytes=0` says which of the serving host's interfaces accepted the
//! connection, as an opaque id, so a dialler can tell two addresses of one radio apart from
//! two radios.
//!
//! - [`PathId`]: a keyed hash of the interface name under a random per-process salt. It
//!   says nothing about the name, and means nothing outside this process's lifetime.
//! - [`path_id_of`]: the id of the interface holding a local socket address, for both
//!   the serving side and the dialler's own side of a connection. Any failure is `None`.
//! - [`PathKey`]: what makes two connections one path: the peer's id and the dialler's own.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;

use http::header::HeaderValue;

use super::client::canonical_addr;

/// Name of the response header that carries the id (protocol.md 3.5).
pub const PATH_HEADER: &str = "x-playdrop-path";

/// Length of an id on the wire, in lowercase hex characters.
const PATH_ID_HEX: usize = 16;

/// An opaque id of one network interface of one running host.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathId([u8; PATH_ID_HEX / 2]);

impl std::fmt::Debug for PathId {
    /// Leaves out the value: an id has no business in a log (SR-32).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PathId(..)")
    }
}

impl PathId {
    /// The id of `interface` under `salt`. *Choice:* 64 bits: an id is compared only with
    /// the ids of one peer's few interfaces within one check, and a shorter header is all
    /// that needs.
    fn derive(salt: &[u8; 32], interface: &str) -> Self {
        let hash = blake3::keyed_hash(salt, interface.as_bytes());
        let mut id = [0u8; PATH_ID_HEX / 2];
        id.copy_from_slice(&hash.as_bytes()[..PATH_ID_HEX / 2]);
        Self(id)
    }

    /// The header form: 16 lowercase hex characters.
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(PATH_ID_HEX);
        for b in self.0 {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// The value for [`PATH_HEADER`].
    pub fn header_value(self) -> HeaderValue {
        HeaderValue::from_str(&self.to_hex()).expect("hex is a valid header value")
    }

    /// Reads a header value. Anything but exactly 16 lowercase hex characters is `None`, and
    /// so measured like a peer that sends no header.
    pub fn from_header(value: &HeaderValue) -> Option<Self> {
        let nibble = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let bytes = value.as_bytes();
        if bytes.len() != PATH_ID_HEX {
            return None;
        }
        let mut id = [0u8; PATH_ID_HEX / 2];
        for (out, [high, low]) in id.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *out = nibble(*high)? << 4 | nibble(*low)?;
        }
        Some(Self(id))
    }
}

/// What makes two connections to one peer one path (protocol.md 3.5): the peer's interface
/// and this host's own. *Choice:* a dialler with two radios reaching one peer interface has
/// two genuinely different paths, so the peer's id alone would merge them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathKey {
    peer: PathId,
    own: PathId,
}

impl PathKey {
    /// The key, if both ids are known. A missing id (a peer from before the header, a
    /// listing that failed) gives no key, and a connection with no key is never merged.
    pub fn new(peer: Option<PathId>, own: Option<PathId>) -> Option<Self> {
        Some(Self {
            peer: peer?,
            own: own?,
        })
    }
}

/// Finds the network interface that holds a local address. Rust only; tests supply their own.
pub trait InterfaceLookup: Send + Sync + std::fmt::Debug {
    /// The name of the interface holding `local`, or `None` if that is not known. May block
    /// briefly, as listing the interfaces does.
    fn interface_of(&self, local: SocketAddr) -> Option<String>;
}

/// [`InterfaceLookup`] over the system's list from `if-addrs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemInterfaceLookup;

impl InterfaceLookup for SystemInterfaceLookup {
    fn interface_of(&self, local: SocketAddr) -> Option<String> {
        let listed: Vec<Listed> = match if_addrs::get_if_addrs() {
            Ok(list) => list
                .into_iter()
                .map(|i| Listed {
                    ip: i.ip(),
                    index: i.index,
                    name: i.name,
                })
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "listing network interfaces failed; no path id");
                return None;
            }
        };
        find_interface(&listed, local)
    }
}

/// One address of the interface list.
struct Listed {
    name: String,
    ip: IpAddr,
    index: Option<u32>,
}

/// The name of the one interface in `listed` holding `local`.
///
/// `None` for a loopback address: *Choice:* everything over loopback (an `adb forward`, an
/// SSH tunnel, a local proxy) sits on one interface here, yet may be a different path
/// further on, so it is measured rather than merged. `None` too if two interfaces hold the
/// address: a wrong merge would hide a path, so an unsure answer gives no id. A link-local
/// address also matches on the socket's scope, because two interfaces may hold the same one.
fn find_interface(listed: &[Listed], local: SocketAddr) -> Option<String> {
    let local = canonical_addr(local);
    if local.ip().is_loopback() {
        return None;
    }
    let scope = match local {
        SocketAddr::V6(v6) if v6.scope_id() != 0 => Some(v6.scope_id()),
        _ => None,
    };
    let mut found: Option<&str> = None;
    for l in listed {
        let in_scope = match (scope, l.index) {
            (Some(scope), Some(index)) => scope == index,
            _ => true,
        };
        if l.ip != local.ip() || !in_scope {
            continue;
        }
        match found {
            Some(name) if name != l.name => return None,
            _ => found = Some(&l.name),
        }
    }
    found.map(str::to_owned)
}

/// The random salt of this process. *Choice:* per process, not per boot or stored: an id is
/// only ever compared within one check, so nothing needs it to last, and an id that changes
/// with each start cannot follow a device around.
fn process_salt() -> &'static [u8; 32] {
    static SALT: OnceLock<[u8; 32]> = OnceLock::new();
    SALT.get_or_init(rand::random)
}

/// This process's id of the interface holding `local`; `None` if it cannot be found.
pub fn path_id_of(lookup: &dyn InterfaceLookup, local: SocketAddr) -> Option<PathId> {
    let name = lookup.interface_of(local)?;
    Some(PathId::derive(process_salt(), &name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Named(&'static str);

    impl InterfaceLookup for Named {
        fn interface_of(&self, _local: SocketAddr) -> Option<String> {
            Some(self.0.to_owned())
        }
    }

    fn listed(name: &str, ip: &str, index: u32) -> Listed {
        Listed {
            name: name.to_owned(),
            ip: ip.parse().unwrap(),
            index: Some(index),
        }
    }

    fn local(addr: &str) -> SocketAddr {
        addr.parse().unwrap()
    }

    fn sample() -> Vec<Listed> {
        vec![
            listed("lo", "127.0.0.1", 1),
            listed("wlan0", "192.168.86.74", 3),
            listed("wlan0", "fe80::1", 3),
            listed("usb0", "192.168.42.129", 4),
            listed("usb0", "fe80::1", 4),
            listed("usb0", "2001:db8::4", 4),
        ]
    }

    #[test]
    fn the_id_hides_the_name_and_is_stable_within_a_process() {
        let name = "wlan0";
        let at = local("192.168.86.74:42187");
        let id = path_id_of(&Named(name), at).unwrap().to_hex();
        assert_eq!(id.len(), 16);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        // Salted: not what anyone could work out from a list of likely names.
        let unsalted = blake3::hash(name.as_bytes());
        let unsalted = PathId::derive(&[0u8; 32], name).to_hex() + &unsalted.to_hex()[..16];
        assert!(!unsalted.contains(&id), "{id}");
        assert_eq!(path_id_of(&Named(name), at).unwrap().to_hex(), id);
    }

    #[test]
    fn the_id_differs_between_interfaces_and_salts() {
        let salt = [7u8; 32];
        assert_ne!(
            PathId::derive(&salt, "wlan0"),
            PathId::derive(&salt, "usb0")
        );
        assert_ne!(
            PathId::derive(&salt, "wlan0"),
            PathId::derive(&[8u8; 32], "wlan0")
        );
    }

    /// Runs this same test in two child processes, each with a salt of its own.
    #[test]
    fn the_id_differs_between_processes() {
        const CHILD: &str = "PD_PATH_ID_CHILD";
        let id = path_id_of(&Named("wlan0"), local("192.168.86.74:1"))
            .unwrap()
            .to_hex();
        if std::env::var_os(CHILD).is_some() {
            println!("id={id}.");
            return;
        }
        let child = || {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "net::path_id::tests::the_id_differs_between_processes",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let at = text.find("id=").expect(&text);
            text[at + 3..at + 3 + PATH_ID_HEX].to_owned()
        };
        let (a, b) = (child(), child());
        assert_ne!(a, b);
        assert_ne!(a, id);
    }

    #[test]
    fn two_addresses_of_one_interface_give_one_name() {
        let list = sample();
        let of = |a: &str| find_interface(&list, local(a));
        assert_eq!(of("192.168.86.74:42187").as_deref(), Some("wlan0"));
        // The mapped form a dual-stack listener reports.
        assert_eq!(of("[::ffff:192.168.86.74]:42187").as_deref(), Some("wlan0"));
        assert_eq!(of("[fe80::1%3]:42187").as_deref(), Some("wlan0"));
        assert_eq!(of("[fe80::1%4]:42187").as_deref(), Some("usb0"));
        assert_eq!(of("[2001:db8::4]:42187").as_deref(), Some("usb0"));
    }

    #[test]
    fn an_unsure_answer_gives_no_name() {
        let list = sample();
        let of = |a: &str| find_interface(&list, local(a));
        assert_eq!(of("10.0.0.1:42187"), None, "on no interface");
        assert_eq!(of("[fe80::1]:42187"), None, "on two, with no scope to tell");
        assert_eq!(of("127.0.0.1:42187"), None, "loopback");
        assert_eq!(of("[::ffff:127.0.0.1]:42187"), None, "mapped loopback");
        assert_eq!(find_interface(&[], local("192.168.86.74:1")), None);
    }

    #[test]
    fn the_system_list_names_a_real_address_and_never_loopback() {
        let lookup = SystemInterfaceLookup;
        assert_eq!(lookup.interface_of(local("127.0.0.1:1")), None);
        // Whatever this machine has: a CI box may have nothing but loopback.
        for i in if_addrs::get_if_addrs().unwrap() {
            if i.is_loopback() {
                continue;
            }
            let at = match i.addr {
                if_addrs::IfAddr::V4(ref v4) => SocketAddr::from((v4.ip, 1)),
                if_addrs::IfAddr::V6(_) => continue,
            };
            let others_hold_it = if_addrs::get_if_addrs()
                .unwrap()
                .iter()
                .any(|o| o.ip() == at.ip() && o.name != i.name);
            if !others_hold_it {
                assert_eq!(lookup.interface_of(at), Some(i.name));
            }
        }
    }

    #[test]
    fn only_exact_lowercase_hex_reads_as_an_id() {
        let good = HeaderValue::from_static("0123456789abcdef");
        assert_eq!(
            PathId::from_header(&good).unwrap().to_hex(),
            "0123456789abcdef"
        );
        for bad in [
            "",
            "0123456789abcde",
            "0123456789abcdef0",
            "0123456789ABCDEF",
            "0123456789abcdeg",
            "wlan0",
        ] {
            assert_eq!(
                PathId::from_header(&HeaderValue::from_static(bad)),
                None,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_key_needs_both_ids() {
        let id = Some(PathId::derive(&[1u8; 32], "wlan0"));
        let other = Some(PathId::derive(&[1u8; 32], "usb0"));
        assert_eq!(PathKey::new(None, id), None);
        assert_eq!(PathKey::new(id, None), None);
        assert_eq!(PathKey::new(id, id), PathKey::new(id, id));
        assert_ne!(PathKey::new(id, id), PathKey::new(other, id));
        assert_ne!(PathKey::new(id, id), PathKey::new(id, other));
    }
}
