//! Turning `--to <peer>` into one paired device.
//!
//! The CLI resolves the peer itself, before it asks the daemon for anything, so it can
//! pick the only online device when `--to` is left out and list the choices when it
//! cannot. The daemon then gets the full hex Device ID, which it resolves without doubt.

use playdrop_core::host::PeerSummary;

/// Why a peer query did not name exactly one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerError {
    /// Nothing is paired yet.
    NoPeers,
    /// `--to` was left out and no paired device is online.
    NoneOnline,
    /// `--to` was left out and more than one paired device is online.
    SeveralOnline,
    /// No label or Short ID matches the query.
    NotFound(String),
    /// More than one device matches; "<label> <Short ID>" of each match.
    Ambiguous(String, Vec<String>),
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPeers => {
                f.write_str("no paired devices; pair one from the PlayDrop window first")
            }
            Self::NoneOnline => f.write_str("no paired device is online; name one with --to"),
            Self::SeveralOnline => {
                f.write_str("more than one paired device is online; name one with --to")
            }
            Self::NotFound(q) => write!(f, "no paired device matches \"{q}\""),
            Self::Ambiguous(q, labels) => write!(
                f,
                "\"{q}\" matches more than one device ({}); use more of the Short ID",
                labels.join(", ")
            ),
        }
    }
}

/// Lowercase and drop hyphens, so `K3QD-7F2M` and `k3qd7f2m` compare equal.
fn normalise_id(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Picks the device a command acts on.
///
/// With a query, three matches are collected: an exact hex Device ID, else every Short ID
/// the query is a prefix of (hyphens optional, case-insensitive), and every exact label
/// (case-insensitive). Exactly one distinct device across them is the answer; more is
/// [`PeerError::Ambiguous`] with each listed. IDs come first because a label is the
/// peer's own advertised name: a device that calls itself another peer's Short ID must
/// not capture `--to`, so such a clash is reported instead of guessed.
///
/// Without a query: the only online peer, if exactly one is online.
pub fn resolve<'a>(
    peers: &'a [PeerSummary],
    query: Option<&str>,
) -> Result<&'a PeerSummary, PeerError> {
    if peers.is_empty() {
        return Err(PeerError::NoPeers);
    }
    let Some(query) = query.map(str::trim) else {
        let mut online = peers.iter().filter(|p| p.online);
        return match (online.next(), online.next()) {
            (Some(p), None) => Ok(p),
            (None, _) => Err(PeerError::NoneOnline),
            (Some(_), Some(_)) => Err(PeerError::SeveralOnline),
        };
    };
    if query.is_empty() {
        return Err(PeerError::NotFound(query.to_owned()));
    }
    let wanted = normalise_id(query);
    let mut matches: Vec<&PeerSummary> = Vec::new();
    if let Some(p) = peers.iter().find(|p| p.device_id.to_lowercase() == wanted) {
        matches.push(p);
    } else if !wanted.is_empty() && wanted.chars().all(|c| c.is_ascii_alphanumeric()) {
        matches.extend(
            peers
                .iter()
                .filter(|p| normalise_id(&p.short_id).starts_with(&wanted)),
        );
    }
    let lower = query.to_lowercase();
    for p in peers.iter().filter(|p| p.label.to_lowercase() == lower) {
        if !matches.iter().any(|m| m.device_id == p.device_id) {
            matches.push(p);
        }
    }
    match matches.as_slice() {
        [] => Err(PeerError::NotFound(query.to_owned())),
        [one] => Ok(*one),
        many => Err(PeerError::Ambiguous(
            query.to_owned(),
            many.iter()
                .map(|p| format!("{} {}", p.label, p.short_id))
                .collect(),
        )),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use playdrop_core::pin_store::Grants;

    pub(crate) fn peer(short_id: &str, label: &str, online: bool) -> PeerSummary {
        PeerSummary {
            device_id: format!("{:0<64}", normalise_id(short_id)),
            short_id: short_id.to_owned(),
            label: label.to_owned(),
            online,
            grants: Grants::default(),
            last_known: Vec::new(),
            paired_at: 0,
            last_seen: None,
            removed_by_peer: false,
            capabilities: Vec::new(),
        }
    }

    fn sample() -> Vec<PeerSummary> {
        vec![
            peer("k3qd-7f2m-aaaa-bbbb", "Phone", true),
            peer("k3qx-1111-cccc-dddd", "Laptop", false),
            peer("zz99-2222-eeee-ffff", "iPhone", false),
        ]
    }

    fn label(r: Result<&PeerSummary, PeerError>) -> Result<String, PeerError> {
        r.map(|p| p.label.clone())
    }

    #[test]
    fn label_matches_ignoring_case() {
        let peers = sample();
        assert_eq!(label(resolve(&peers, Some("phone"))), Ok("Phone".into()));
        assert_eq!(label(resolve(&peers, Some("IPHONE"))), Ok("iPhone".into()));
    }

    #[test]
    fn short_id_prefix_with_or_without_hyphens() {
        let peers = sample();
        assert_eq!(label(resolve(&peers, Some("k3qd"))), Ok("Phone".into()));
        assert_eq!(label(resolve(&peers, Some("K3QD-7F"))), Ok("Phone".into()));
        assert_eq!(label(resolve(&peers, Some("k3qd7f2m"))), Ok("Phone".into()));
        assert_eq!(
            label(resolve(&peers, Some("zz99-2222-eeee-ffff"))),
            Ok("iPhone".into())
        );
    }

    #[test]
    fn full_device_id_matches() {
        let peers = sample();
        let id = peers[1].device_id.to_uppercase();
        assert_eq!(label(resolve(&peers, Some(&id))), Ok("Laptop".into()));
    }

    #[test]
    fn shared_prefix_is_ambiguous() {
        let peers = sample();
        assert_eq!(
            resolve(&peers, Some("k3q")).map(|p| p.label.clone()),
            Err(PeerError::Ambiguous(
                "k3q".into(),
                vec![
                    "Phone k3qd-7f2m-aaaa-bbbb".into(),
                    "Laptop k3qx-1111-cccc-dddd".into()
                ]
            ))
        );
    }

    #[test]
    fn duplicate_labels_are_ambiguous() {
        let peers = vec![
            peer("aaaa-1", "Phone", true),
            peer("bbbb-1", "phone", false),
        ];
        assert!(matches!(
            resolve(&peers, Some("Phone")),
            Err(PeerError::Ambiguous(..))
        ));
        // The Short ID still picks one.
        assert_eq!(label(resolve(&peers, Some("bbbb"))), Ok("phone".into()));
    }

    #[test]
    fn a_label_that_looks_like_another_peers_id_is_ambiguous() {
        // "ffff-1" named itself "abcd", the Short ID prefix of "Desk".
        let peers = vec![peer("abcd-1", "Desk", false), peer("ffff-1", "abcd", false)];
        assert_eq!(
            resolve(&peers, Some("abcd")).map(|_| ()),
            Err(PeerError::Ambiguous(
                "abcd".into(),
                vec!["Desk abcd-1".into(), "abcd ffff-1".into()]
            ))
        );
        // The same against a full Device ID.
        let mut peers = peers;
        peers[1].label = peers[0].device_id.clone();
        let id = peers[0].device_id.clone();
        assert!(matches!(
            resolve(&peers, Some(&id)),
            Err(PeerError::Ambiguous(..))
        ));
        // The other peer's own Short ID still reaches it.
        assert_eq!(label(resolve(&peers, Some("ffff"))), Ok(id));
    }

    #[test]
    fn id_and_label_naming_the_same_peer_is_fine() {
        let peers = vec![peer("abcd-1", "abcd", false), peer("ffff-1", "Desk", false)];
        assert_eq!(label(resolve(&peers, Some("ABCD"))), Ok("abcd".into()));
    }

    #[test]
    fn unknown_or_empty_query_is_not_found() {
        let peers = sample();
        assert_eq!(
            resolve(&peers, Some("tablet")).map(|_| ()),
            Err(PeerError::NotFound("tablet".into()))
        );
        assert!(matches!(
            resolve(&peers, Some("  ")),
            Err(PeerError::NotFound(_))
        ));
        assert!(matches!(
            resolve(&peers, Some("---")),
            Err(PeerError::NotFound(_))
        ));
    }

    #[test]
    fn default_is_the_only_online_peer() {
        let peers = sample();
        assert_eq!(label(resolve(&peers, None)), Ok("Phone".into()));
    }

    #[test]
    fn default_fails_with_none_or_several_online() {
        let mut peers = sample();
        peers[0].online = false;
        assert_eq!(
            resolve(&peers, None).map(|_| ()),
            Err(PeerError::NoneOnline)
        );
        peers[0].online = true;
        peers[2].online = true;
        assert_eq!(
            resolve(&peers, None).map(|_| ()),
            Err(PeerError::SeveralOnline)
        );
        assert_eq!(resolve(&[], None).map(|_| ()), Err(PeerError::NoPeers));
        assert_eq!(resolve(&[], Some("x")).map(|_| ()), Err(PeerError::NoPeers));
    }
}
