//! The rules for the values of a `reach` frame (protocol.md 12). The frame comes from a paired
//! peer that may be hostile or have a bug, and what it says decides who this host contacts, so
//! everything is checked here, without panicking on any input, before it is stored.

use std::net::IpAddr;

use iroh::{EndpointId, RelayUrl};

/// Longest relay URL accepted (protocol.md 12).
pub const MAX_RELAY_URL: usize = 256;

/// What a valid `reach` frame says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachInfo {
    /// Both fields null: the peer took the grant back, forget what was stored.
    Forget,
    /// The peer's endpoint and the relay it named.
    Set {
        /// Its Endpoint ID.
        endpoint: EndpointId,
        /// Its relay.
        relay: RelayUrl,
    },
}

/// Why a `reach` frame's values were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReachValueError {
    /// One field null and the other not.
    #[error("endpoint and relay must both be set or both be null")]
    Partial,
    /// Not 64 lowercase hex characters of a valid key.
    #[error("not an endpoint id")]
    Endpoint,
    /// Over [`MAX_RELAY_URL`] bytes.
    #[error("relay URL too long")]
    TooLong,
    /// Not a URL, or carries a user name, a password, a path, a query or a fragment.
    #[error("not a plain relay URL")]
    Url,
    /// Neither `https`, nor `http` to a loopback or private address.
    #[error("relay URL scheme not allowed for this address")]
    Scheme,
}

/// Checks the values of a `reach` frame.
pub fn parse_values(
    endpoint: Option<&str>,
    relay: Option<&str>,
) -> Result<ReachInfo, ReachValueError> {
    match (endpoint, relay) {
        (None, None) => Ok(ReachInfo::Forget),
        (Some(e), Some(r)) => Ok(ReachInfo::Set {
            endpoint: parse_endpoint(e)?,
            relay: parse_relay(r)?,
        }),
        _ => Err(ReachValueError::Partial),
    }
}

/// An Endpoint ID as the frame carries it: exactly 64 lowercase hex characters of a valid key.
pub fn parse_endpoint(s: &str) -> Result<EndpointId, ReachValueError> {
    if s.len() != 64 {
        return Err(ReachValueError::Endpoint);
    }
    let bytes = data_encoding::HEXLOWER
        .decode(s.as_bytes())
        .map_err(|_| ReachValueError::Endpoint)?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| ReachValueError::Endpoint)?;
    EndpointId::from_bytes(&bytes).map_err(|_| ReachValueError::Endpoint)
}

/// An Endpoint ID in the form [`parse_endpoint`] reads.
pub fn endpoint_hex(id: &EndpointId) -> String {
    data_encoding::HEXLOWER.encode(id.as_bytes())
}

/// A relay URL under the rules of protocol.md 12: at most [`MAX_RELAY_URL`] bytes, `https`, or
/// `http` for a loopback or private address, and nothing beyond scheme, host and port.
pub fn parse_relay(s: &str) -> Result<RelayUrl, ReachValueError> {
    if s.len() > MAX_RELAY_URL {
        return Err(ReachValueError::TooLong);
    }
    let url: RelayUrl = s.parse().map_err(|_| ReachValueError::Url)?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none();
    if !plain {
        return Err(ReachValueError::Url);
    }
    let host = url.host_str().filter(|h| !h.is_empty());
    let host = host.ok_or(ReachValueError::Url)?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_local_host(host) => Ok(url),
        _ => Err(ReachValueError::Scheme),
    }
}

/// True for `localhost` and for a loopback or private address literal. A host name other than
/// `localhost` could resolve anywhere, so it does not count.
fn is_local_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    match literal.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_loopback() || a.is_private(),
        Ok(IpAddr::V6(a)) => {
            a.is_loopback() || a.is_unique_local() || a.to_ipv4_mapped().is_some_and(is_private_v4)
        }
        Err(_) => false,
    }
}

fn is_private_v4(a: std::net::Ipv4Addr) -> bool {
    a.is_loopback() || a.is_private()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn valid_endpoint() -> String {
        endpoint_hex(&iroh::SecretKey::from_bytes(&[3; 32]).public())
    }

    #[test]
    fn a_good_pair_and_the_forget_form_parse() {
        let e = valid_endpoint();
        let got = parse_values(Some(&e), Some("https://relay.example.org")).unwrap();
        let ReachInfo::Set { endpoint, relay } = got else {
            panic!("set")
        };
        assert_eq!(endpoint_hex(&endpoint), e);
        assert_eq!(relay.scheme(), "https");
        assert_eq!(parse_values(None, None).unwrap(), ReachInfo::Forget);
    }

    #[test]
    fn one_field_alone_is_refused() {
        let e = valid_endpoint();
        assert_eq!(
            parse_values(Some(&e), None).unwrap_err(),
            ReachValueError::Partial
        );
        assert_eq!(
            parse_values(None, Some("https://a.example")).unwrap_err(),
            ReachValueError::Partial
        );
    }

    #[test]
    fn endpoint_must_be_64_lowercase_hex_of_a_key() {
        for bad in [
            "",
            "abcd",
            &"g".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
            &valid_endpoint().to_uppercase(),
            // Multi-byte characters of the right byte length.
            &"é".repeat(32),
        ] {
            assert_eq!(
                parse_endpoint(bad).unwrap_err(),
                ReachValueError::Endpoint,
                "{bad}"
            );
        }
    }

    #[test]
    fn relay_rules() {
        for good in [
            "https://relay.example.org",
            "https://relay.example.org/",
            "https://relay.example.org:8443",
            "https://203.0.113.9",
            "http://127.0.0.1:3340",
            "http://[::1]:3340",
            "http://10.1.2.3:3340",
            "http://192.168.1.2",
            "http://172.16.0.1:80",
            "http://[fd00::1]:3340",
            "http://localhost:3340",
        ] {
            parse_relay(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        for (bad, why) in [
            ("http://relay.example.org", ReachValueError::Scheme),
            ("http://8.8.8.8:3340", ReachValueError::Scheme),
            ("http://[2001:db8::1]:3340", ReachValueError::Scheme),
            ("ftp://relay.example.org", ReachValueError::Scheme),
            ("file:///etc/passwd", ReachValueError::Url),
            ("https://user@relay.example.org", ReachValueError::Url),
            ("https://u:p@relay.example.org", ReachValueError::Url),
            ("https://relay.example.org/a", ReachValueError::Url),
            ("https://relay.example.org/?a=1", ReachValueError::Url),
            ("https://relay.example.org/#a", ReachValueError::Url),
            ("not a url", ReachValueError::Url),
            ("", ReachValueError::Url),
        ] {
            assert_eq!(parse_relay(bad).unwrap_err(), why, "{bad}");
        }
        let long = format!("https://{}.example.org", "a".repeat(MAX_RELAY_URL));
        assert_eq!(parse_relay(&long).unwrap_err(), ReachValueError::TooLong);
    }

    proptest! {
        /// No input panics, and whatever passes is within the limits.
        #[test]
        fn hostile_values_never_panic(e in ".{0,80}", r in ".{0,300}") {
            let _ = parse_values(Some(&e), Some(&r));
            let _ = parse_values(None, Some(&r));
            let _ = parse_endpoint(&e);
            if let Ok(url) = parse_relay(&r) {
                prop_assert!(r.len() <= MAX_RELAY_URL);
                prop_assert!(matches!(url.scheme(), "https" | "http"));
            }
        }

        /// Anything URL-shaped, the likelier way to reach the deeper branches.
        #[test]
        fn url_shaped_values_never_panic(
            scheme in "(http|https|ws|wss|ftp|file)",
            host in "[a-zA-Z0-9\\.\\-\\[\\]:@%]{0,60}",
            rest in "[a-zA-Z0-9/\\?#&=%:@]{0,60}",
        ) {
            let s = format!("{scheme}://{host}{rest}");
            if let Ok(url) = parse_relay(&s) {
                prop_assert!(url.username().is_empty());
                prop_assert!(url.query().is_none());
            }
        }
    }
}
