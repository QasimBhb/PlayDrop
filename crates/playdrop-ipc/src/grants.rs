//! The grants a [`crate::Request::SetGrants`] carries (`docs/ipc.md` section 1.10).
//!
//! Two meanings, chosen by the request's `merge` flag:
//!
//! - Without `merge` (every client up to 0.15.0): the legacy meaning. The six original grants
//!   are required and a missing notification grant reads as its pairing default, exactly as
//!   0.15.0 read the request ([`GrantsUpdate::into_full`]).
//! - With `merge: true` (every client from 0.16.0): a partial update. A grant that is missing
//!   or `null` keeps its stored value ([`GrantsUpdate::apply`]).
//!
//! Every field is written out explicitly when present, unlike [`Grants`], which leaves out
//! the newer grants while they sit at their defaults.

use serde::{Deserialize, Serialize};

use crate::types::Grants;

/// One optional value per grant. `None` keeps the stored value.
///
/// [`From<Grants>`] and [`GrantsUpdate::apply`] destructure [`Grants`] without `..`, so a
/// grant added to the core does not compile until it has its field here too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantsUpdate {
    /// Auto-accept file offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<bool>,
    /// Accept clipboard pushes from the peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_in: Option<bool>,
    /// Send local clipboard changes to the peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_out: Option<bool>,
    /// Remote input from the peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<bool>,
    /// Media state and control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<bool>,
    /// "Find my device" ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<bool>,
    /// Show mirrored notifications the peer sends (added in 0.10.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications_in: Option<bool>,
    /// Mirror this device's notifications to the peer (added in 0.10.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications_out: Option<bool>,
}

impl GrantsUpdate {
    /// The stored grants with every carried field replaced and every other one kept.
    #[must_use]
    pub fn apply(self, current: Grants) -> Grants {
        let Grants {
            files,
            clipboard_in,
            clipboard_out,
            input,
            media,
            find,
            notifications_in,
            notifications_out,
        } = current;
        Grants {
            files: self.files.unwrap_or(files),
            clipboard_in: self.clipboard_in.unwrap_or(clipboard_in),
            clipboard_out: self.clipboard_out.unwrap_or(clipboard_out),
            input: self.input.unwrap_or(input),
            media: self.media.unwrap_or(media),
            find: self.find.unwrap_or(find),
            notifications_in: self.notifications_in.unwrap_or(notifications_in),
            notifications_out: self.notifications_out.unwrap_or(notifications_out),
        }
    }

    /// The legacy meaning (a request without `merge`): the grants exactly as 0.15.0 read
    /// them. The six grants that existed before 0.10.0 must all be present, or the error
    /// names the first one missing; a grant added since then reads as its pairing default
    /// when missing (`notificationsIn` on, `notificationsOut` off).
    ///
    /// # Errors
    ///
    /// The wire name of the first required grant that is missing.
    pub fn into_full(self) -> Result<Grants, &'static str> {
        let Self {
            files,
            clipboard_in,
            clipboard_out,
            input,
            media,
            find,
            notifications_in,
            notifications_out,
        } = self;
        let defaults = Grants::default();
        Ok(Grants {
            files: files.ok_or("files")?,
            clipboard_in: clipboard_in.ok_or("clipboardIn")?,
            clipboard_out: clipboard_out.ok_or("clipboardOut")?,
            input: input.ok_or("input")?,
            media: media.ok_or("media")?,
            find: find.ok_or("find")?,
            notifications_in: notifications_in.unwrap_or(defaults.notifications_in),
            notifications_out: notifications_out.unwrap_or(defaults.notifications_out),
        })
    }

    /// True when the request carries no grant at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl From<Grants> for GrantsUpdate {
    /// Every grant carried, so the stored grants become exactly `g`, in either meaning.
    /// This is what the in-tree clients send, with `merge: true`.
    fn from(g: Grants) -> Self {
        let Grants {
            files,
            clipboard_in,
            clipboard_out,
            input,
            media,
            find,
            notifications_in,
            notifications_out,
        } = g;
        Self {
            files: Some(files),
            clipboard_in: Some(clipboard_in),
            clipboard_out: Some(clipboard_out),
            input: Some(input),
            media: Some(media),
            find: Some(find),
            notifications_in: Some(notifications_in),
            notifications_out: Some(notifications_out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stored grants far from the defaults, so a reset to defaults would show.
    fn stored() -> Grants {
        Grants {
            files: false,
            clipboard_in: false,
            clipboard_out: true,
            input: true,
            media: false,
            find: true,
            notifications_in: false,
            notifications_out: true,
        }
    }

    fn parse(json: &str) -> GrantsUpdate {
        serde_json::from_str(json).expect("grants update")
    }

    #[test]
    fn a_full_update_replaces_every_grant() {
        let want = Grants {
            input: false,
            ..Grants::default()
        };
        assert_eq!(GrantsUpdate::from(want).apply(stored()), want);
        assert_eq!(GrantsUpdate::from(stored()).apply(want), stored());
    }

    #[test]
    fn a_full_update_writes_every_field_even_at_its_default() {
        // `Grants` itself leaves `notificationsIn: true` and `notificationsOut: false` out;
        // an update must not, or the daemon would read them as "keep".
        let v = serde_json::to_value(GrantsUpdate::from(Grants::default())).expect("json");
        let obj = v.as_object().expect("object");
        for key in [
            "files",
            "clipboardIn",
            "clipboardOut",
            "input",
            "media",
            "find",
            "notificationsIn",
            "notificationsOut",
        ] {
            assert!(obj.contains_key(key), "{key} missing from {v}");
        }
        assert_eq!(obj.len(), 8, "{v}");
    }

    #[test]
    fn a_full_update_reads_back_as_core_grants() {
        // A new client talking to an older daemon: the daemon parses `Grants`.
        for g in [Grants::default(), stored()] {
            let json = serde_json::to_string(&GrantsUpdate::from(g)).expect("json");
            let back: Grants = serde_json::from_str(&json).expect("grants");
            assert_eq!(back, g);
        }
    }

    /// The regression case: a 0.10.0 to 0.15.0 window leaves `notificationsIn` out because
    /// it is `true` (core `Grants` skips defaults). The legacy meaning reads it as `true`,
    /// so turning "show notifications" back on works.
    #[test]
    fn legacy_body_from_0_15_turns_notifications_in_back_on() {
        let on = Grants {
            notifications_in: true,
            ..stored()
        };
        let json = serde_json::to_string(&on).expect("json");
        assert!(!json.contains("notificationsIn"), "{json}");
        assert_eq!(parse(&json).into_full(), Ok(on));
        // And off, which 0.15.0 writes out.
        let off = Grants {
            notifications_in: false,
            ..stored()
        };
        let json = serde_json::to_string(&off).expect("json");
        assert_eq!(parse(&json).into_full(), Ok(off));
    }

    /// The legacy meaning on purpose: a missing notification grant is its default, whatever
    /// is stored. A client older than 0.10.0 therefore still resets them; it cannot be told
    /// apart from the 0.10.0 to 0.15.0 case above.
    #[test]
    fn legacy_body_reads_missing_notification_grants_as_defaults() {
        let old = parse(
            r#"{"files":true,"clipboardIn":true,"clipboardOut":false,
                "input":false,"media":true,"find":false}"#,
        );
        let full = old.into_full().expect("six grants present");
        assert!(full.notifications_in && !full.notifications_out);
        assert_eq!(full, old.apply(Grants::default()));
    }

    /// Without `merge` the six original grants are required, as 0.15.0 required them.
    #[test]
    fn legacy_body_without_an_original_grant_is_refused() {
        assert_eq!(parse("{}").into_full(), Err("files"));
        assert_eq!(
            parse(r#"{"files":true,"clipboardIn":true,"clipboardOut":true,"input":false,"media":true}"#)
                .into_full(),
            Err("find")
        );
        assert_eq!(
            parse(r#"{"files":true,"clipboardIn":null,"clipboardOut":true,"input":false,"media":true,"find":true}"#)
                .into_full(),
            Err("clipboardIn")
        );
        // A full update reads back whole in the legacy meaning too.
        assert_eq!(GrantsUpdate::from(stored()).into_full(), Ok(stored()));
    }

    #[test]
    fn a_merge_keeps_the_grants_it_does_not_carry() {
        // A merge body that carries the six original grants only.
        let old = parse(
            r#"{"files":true,"clipboardIn":true,"clipboardOut":false,
                "input":false,"media":true,"find":false}"#,
        );
        assert_eq!(old.notifications_in, None);
        assert_eq!(old.notifications_out, None);
        let got = old.apply(stored());
        assert!(!got.notifications_in, "notificationsIn was reset");
        assert!(got.notifications_out, "notificationsOut was reset");
        assert!(got.files && got.clipboard_in && got.media);
        assert!(!got.clipboard_out && !got.input && !got.find);
    }

    #[test]
    fn any_missing_field_keeps_its_stored_value() {
        assert!(parse("{}").is_empty());
        assert_eq!(parse("{}").apply(stored()), stored());
        let only_find = parse(r#"{"find":false}"#);
        assert_eq!(
            only_find.apply(stored()),
            Grants {
                find: false,
                ..stored()
            }
        );
        let null_is_absent = parse(r#"{"media":null,"files":true}"#);
        assert_eq!(
            null_is_absent.apply(stored()),
            Grants {
                files: true,
                ..stored()
            }
        );
    }

    #[test]
    fn unknown_grants_from_a_newer_client_are_ignored() {
        let newer = parse(r#"{"files":false,"someLaterGrant":true}"#);
        assert_eq!(newer.apply(stored()), stored());
    }

    #[test]
    fn an_absent_field_is_not_serialised() {
        let only = GrantsUpdate {
            media: Some(true),
            ..GrantsUpdate::default()
        };
        assert_eq!(
            serde_json::to_string(&only).expect("json"),
            r#"{"media":true}"#
        );
    }
}
