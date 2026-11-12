//! What the update check remembers between runs: `update.json` in the data dir, mode 0600,
//! written atomically like `settings.json`.
//!
//! It holds the times the once-a-day rule needs (SR-45), the version the user said "Later"
//! to, and the last signed answer (`SHA256SUMS` and its signature, as received). The answer
//! is kept so an offer survives a restart of the daemon without asking GitHub again; it is
//! verified against the pinned keys again every time it is read, so the file is never
//! trusted for content (boundary B9). Nothing in it identifies the device: GitHub's file,
//! two timestamps and a version.

use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

/// File name inside the data dir.
pub const UPDATE_FILE: &str = "update.json";

/// The update check's memory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remembered {
    /// When a check last got an answer, Unix milliseconds.
    pub last_answer_ms: Option<u64>,
    /// When a check last failed, Unix milliseconds.
    pub last_failure_ms: Option<u64>,
    /// The version the user said "Later" to; cleared by the next check that runs.
    pub later_version: Option<String>,
    /// When, Unix milliseconds.
    pub later_at_ms: Option<u64>,
    /// The last `SHA256SUMS` that verified, as text (a verified one is UTF-8).
    pub sums: Option<String>,
    /// Its `SHA256SUMS.sig`, as text.
    pub sig: Option<String>,
}

/// Reads `update.json`. A missing or unreadable file is an empty memory: the worst case is
/// one extra check, which the once-a-day rule then limits again.
pub fn load(data_dir: &Path) -> Remembered {
    let path = data_dir.join(UPDATE_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "update.json does not parse; starting afresh");
            Remembered::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Remembered::default(),
        Err(e) => {
            tracing::warn!(error = %e, "update.json could not be read; starting afresh");
            Remembered::default()
        }
    }
}

/// Writes `update.json` atomically, mode 0600 on Unix (the data dir's owner-only DACL on
/// Windows).
pub fn save(data_dir: &Path, remembered: &Remembered) -> anyhow::Result<()> {
    let path = data_dir.join(UPDATE_FILE);
    let tmp = data_dir.join(format!("{UPDATE_FILE}.tmp-{}", std::process::id()));
    let mut json = serde_json::to_vec_pretty(remembered).context("encoding update.json")?;
    json.push(b'\n');
    let write = || -> std::io::Result<()> {
        let mut f = private_file(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_privately_and_survives_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()), Remembered::default());
        let r = Remembered {
            last_answer_ms: Some(1),
            last_failure_ms: Some(2),
            later_version: Some("1.2.3".into()),
            later_at_ms: Some(3),
            sums: Some("sums".into()),
            sig: Some("sig".into()),
        };
        save(tmp.path(), &r).unwrap();
        assert_eq!(load(tmp.path()), r);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(tmp.path().join(UPDATE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(tmp.path().join(UPDATE_FILE), b"{oops").unwrap();
        assert_eq!(load(tmp.path()), Remembered::default());
        std::fs::write(
            tmp.path().join(UPDATE_FILE),
            br#"{"last_answer_ms":7,"new":1}"#,
        )
        .unwrap();
        assert_eq!(load(tmp.path()).last_answer_ms, Some(7));
    }
}
