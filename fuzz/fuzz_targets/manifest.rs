//! Transfer manifests, protocol.md 7.2 and 7.7 (threat-model.md SR-17, SR-19, SR-20, SR-33).
//!
//! Input: a `POST /v1/transfers` body. Parsed exactly as the receiver does (serde into
//! `Manifest`, then `Manifest::validate`), then the bundle layout is built from every
//! manifest that validates. Checks that validation really enforces the path and size rules
//! and that the layout agrees with `bundle.size`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use playdrop_core::transfer::bundle::Position;
use playdrop_core::transfer::manifest::{
    validate_component, MAX_BUNDLE_MEMBER, MAX_DEPTH, MAX_ENTRIES, MAX_MANIFEST_BODY,
};
use playdrop_core::transfer::{bundle_size, BundleLayout, Manifest};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_MANIFEST_BODY {
        return;
    }
    let Ok(m) = serde_json::from_slice::<Manifest>(data) else {
        return;
    };
    let valid = m.validate().is_ok();

    let again: Manifest =
        serde_json::from_slice(&serde_json::to_vec(&m).expect("manifest serialises"))
            .expect("re-serialised manifest parses");
    assert_eq!(again, m, "manifest round trip");
    assert_eq!(
        again.validate().is_ok(),
        valid,
        "validation depends on formatting"
    );

    if !valid {
        return;
    }
    assert!(m.files.len() + m.dirs.len() <= MAX_ENTRIES);
    let mut sum = 0u64;
    for (i, f) in m.files.iter().enumerate() {
        assert_eq!(f.fid as usize, i);
        assert!(!f.path.is_empty() && f.path.len() <= MAX_DEPTH);
        for c in &f.path {
            assert!(validate_component(c).is_ok());
        }
        if f.in_bundle {
            assert!(f.size <= MAX_BUNDLE_MEMBER);
        }
        sum += f.size;
    }
    assert_eq!(sum, m.total_bytes);

    let layout = BundleLayout::new(&m).expect("a valid manifest has a valid bundle layout");
    let members = layout.members();
    assert_eq!(members.len(), m.bundle_members().count());
    match &m.bundle {
        Some(info) => {
            assert!(!members.is_empty());
            assert_eq!(info.size, layout.total());
        }
        None => assert!(members.is_empty()),
    }
    assert_eq!(layout.total(), bundle_size(&m));

    let mut expected_offset = 0u64;
    let mut content = 0u64;
    for (idx, mem) in members.iter().enumerate() {
        assert_eq!(mem.header_offset, expected_offset, "members are contiguous");
        assert_eq!(mem.header_offset % 512, 0, "headers are block aligned");
        assert_eq!(layout.member(mem.fid), Some(mem));
        assert_eq!(
            layout.locate(mem.header_offset),
            Position::Header { idx, within: 0 }
        );
        if mem.size > 0 {
            assert_eq!(
                layout.locate(mem.data_offset()),
                Position::Data { idx, within: 0 }
            );
        }
        content += mem.size;
        expected_offset = mem.padded_end();
    }
    assert_eq!(layout.total(), expected_offset + 1024, "two-block trailer");
    assert_eq!(layout.locate(layout.total()), Position::End);
    assert_eq!(layout.content_bytes_below(layout.total()), content);
});
