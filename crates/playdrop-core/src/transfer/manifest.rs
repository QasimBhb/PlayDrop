//! Transfer manifest (protocol.md 7.2), SR-17 path rules, SR-20 limits, and a builder that
//! walks a local tree and records per-source resume facts (SR-21).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rand::Rng;
use serde::{Deserialize, Serialize};

use super::source::{SourceRef, StreamSource};
use crate::constants::{DEFAULT_CHUNK_SIZE, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE};
use crate::{Error, Result};

/// Most entries (files plus dirs) a manifest may list, protocol.md 7.2 and SR-20.
pub const MAX_ENTRIES: usize = 100_000;

/// Largest manifest body in bytes (16 MiB), protocol.md 7.2 and SR-20. Enforcing it while
/// reading the HTTP body is the caller's job.
pub const MAX_MANIFEST_BODY: usize = 16 * 1024 * 1024;

/// Longest path component in UTF-8 bytes, SR-17.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// Deepest path (number of components), SR-17.
pub const MAX_DEPTH: usize = 32;

/// Longest path in bytes, SR-17. Measured as the components joined with one `/` each.
pub const MAX_PATH_BYTES: usize = 1024;

/// Files smaller than this travel in the bundle when built by [`ManifestBuilder`]
/// (protocol.md 7.2 *Choice*: senders SHOULD bundle files under 256 KiB).
pub const BUNDLE_THRESHOLD: u64 = 256 * 1024;

/// Largest member a ustar header can describe (11 octal digits), protocol.md 7.7.
pub const MAX_BUNDLE_MEMBER: u64 = 0o77_777_777_777;

/// A transfer offer, protocol.md 7.2. Field names and shape match the wire JSON exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// 16 random bytes as lowercase hex, made by the sender.
    pub id: String,
    /// Proposed chunk size: a power of two from 64 KiB to 16 MiB.
    pub chunk_size: u32,
    /// Sum of all file sizes.
    pub total_bytes: u64,
    /// Optional free-text note shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Files; `fid` equals the index.
    pub files: Vec<FileEntry>,
    /// Directories to create (needed only for empty ones).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<Vec<String>>,
    /// Present when any file is `inBundle`; carries the exact tar length (7.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<BundleInfo>,
}

/// One manifest file, protocol.md 7.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Index in `files`.
    pub fid: u32,
    /// Path components, NFC, checked by SR-17.
    pub path: Vec<String>,
    /// Size in bytes.
    pub size: u64,
    /// Modification time, Unix milliseconds.
    pub mtime: i64,
    /// Optional MIME type hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// True when the file travels inside the tar bundle (7.7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_bundle: bool,
}

/// The `bundle` object of a manifest, protocol.md 7.2 and 7.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleInfo {
    /// Exact tar length in bytes, see [`crate::transfer::bundle_size`].
    pub size: u64,
}

/// Returns true for a valid chunk size: a power of two from 64 KiB to 16 MiB (protocol.md 7.2).
pub fn is_valid_chunk_size(chunk_size: u32) -> bool {
    chunk_size.is_power_of_two() && (MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&chunk_size)
}

/// Returns true for a transfer ID: 32 lowercase hex characters (protocol.md 7.2).
pub fn is_valid_transfer_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Makes a fresh transfer ID: 16 random bytes as lowercase hex (protocol.md 7.2).
pub fn new_transfer_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    data_encoding::HEXLOWER.encode(&bytes)
}

impl Manifest {
    /// Checks every rule of protocol.md 7.2: SR-17 paths, SR-20 entry limit, chunk size,
    /// `fid` order, `totalBytes`, and the bundle size. Any violation rejects the whole
    /// manifest.
    ///
    /// Errors: [`Error::PathInvalid`] for path rules, [`Error::TooLarge`] for the entry
    /// limit, [`Error::BadRequest`] for everything else.
    pub fn validate(&self) -> Result<()> {
        if !is_valid_transfer_id(&self.id) {
            return Err(Error::BadRequest(
                "id must be 32 lowercase hex chars".into(),
            ));
        }
        if !is_valid_chunk_size(self.chunk_size) {
            return Err(Error::BadRequest(format!(
                "chunkSize {} is not a power of two in 64 KiB..16 MiB",
                self.chunk_size
            )));
        }
        let entries = self.files.len() + self.dirs.len();
        if entries > MAX_ENTRIES {
            return Err(Error::TooLarge(format!(
                "{entries} entries, limit {MAX_ENTRIES}"
            )));
        }
        let mut total: u64 = 0;
        let mut any_bundle = false;
        for (index, file) in self.files.iter().enumerate() {
            if usize::try_from(file.fid).ok() != Some(index) {
                return Err(Error::BadRequest(format!(
                    "fid {} at index {index}",
                    file.fid
                )));
            }
            validate_path(&file.path)?;
            total = total
                .checked_add(file.size)
                .ok_or_else(|| Error::TooLarge("totalBytes overflows".into()))?;
            if file.in_bundle {
                any_bundle = true;
                if file.size > MAX_BUNDLE_MEMBER {
                    return Err(Error::BadRequest(format!(
                        "fid {} too large for the bundle",
                        file.fid
                    )));
                }
            }
        }
        for dir in &self.dirs {
            validate_path(dir)?;
        }
        if total != self.total_bytes {
            return Err(Error::BadRequest(format!(
                "totalBytes {} but files sum to {total}",
                self.total_bytes
            )));
        }
        check_collisions(self)?;
        match (&self.bundle, any_bundle) {
            (None, false) => {}
            (Some(info), true) => {
                let expected = crate::transfer::bundle_size(self);
                if info.size != expected {
                    return Err(Error::BadRequest(format!(
                        "bundle.size {} but the tar is {expected} bytes",
                        info.size
                    )));
                }
            }
            (Some(_), false) => {
                return Err(Error::BadRequest(
                    "bundle present but no file is inBundle".into(),
                ))
            }
            (None, true) => {
                return Err(Error::BadRequest(
                    "inBundle files but no bundle object".into(),
                ))
            }
        }
        Ok(())
    }

    /// Files that travel in the bundle, in `fid` order (protocol.md 7.7).
    pub fn bundle_members(&self) -> impl Iterator<Item = &FileEntry> {
        self.files.iter().filter(|f| f.in_bundle)
    }
}

/// Validates one path (array of components) against SR-17 and protocol.md 7.2.
pub fn validate_path(path: &[String]) -> Result<()> {
    if path.is_empty() {
        return Err(Error::PathInvalid("empty path".into()));
    }
    if path.len() > MAX_DEPTH {
        return Err(Error::PathInvalid(format!(
            "depth {} exceeds {MAX_DEPTH}",
            path.len()
        )));
    }
    let joined_len: usize = path.iter().map(String::len).sum::<usize>() + path.len() - 1;
    if joined_len > MAX_PATH_BYTES {
        return Err(Error::PathInvalid(format!(
            "path is {joined_len} bytes, limit {MAX_PATH_BYTES}"
        )));
    }
    for component in path {
        validate_component(component)?;
    }
    Ok(())
}

/// Validates one path component against SR-17.
pub fn validate_component(c: &str) -> Result<()> {
    let bad = |why: &str| Err(Error::PathInvalid(format!("{why}: {c:?}")));
    if c.is_empty() {
        return bad("empty component");
    }
    if c == "." || c == ".." {
        return bad("dot component");
    }
    if c.len() > MAX_COMPONENT_BYTES {
        return bad("component over 255 bytes");
    }
    for ch in c.chars() {
        // `char::is_control` covers C0 (including NUL), DEL and C1.
        if ch.is_control() {
            return bad("control character");
        }
        if matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            return bad("reserved character");
        }
        if matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
            return bad("bidi control");
        }
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return bad("trailing dot or space");
    }
    if is_windows_device_name(c) {
        return bad("Windows device name");
    }
    if !is_nfc(c) {
        return bad("not NFC");
    }
    Ok(())
}

/// True for CON, PRN, AUX, NUL, COM0-9, LPT0-9 (also with superscript 1, 2, 3), plus
/// CONIN$ and CONOUT$, with or without an extension, any case (SR-17). Windows ignores
/// trailing spaces before the extension, so they are trimmed too.
fn is_windows_device_name(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => return true,
        _ => {}
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    if prefix != "COM" && prefix != "LPT" {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    matches!(rest.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '\u{00B9}' | '\u{00B2}' | '\u{00B3}'))
}

/// NFC check (SR-17): the component must already be in Unicode Normalization Form C.
/// Senders normalise before building the manifest; a receiver rejects rather than
/// silently renormalising, so both ends agree on every byte of the name.
fn is_nfc(c: &str) -> bool {
    c.is_ascii() || unicode_normalization::is_nfc(c)
}

/// Case-fold key used for duplicate detection. Upper then lower casing folds `ß` and `SS`
/// together, which is stricter than either platform needs.
fn fold(component: &str) -> String {
    component.to_uppercase().to_lowercase()
}

/// Rejects case-insensitive duplicates across files and dirs, and a file that would have
/// to be a directory for another entry (SR-17).
fn check_collisions(m: &Manifest) -> Result<()> {
    let mut files: HashSet<Vec<String>> = HashSet::with_capacity(m.files.len());
    let mut ancestors: HashSet<Vec<String>> = HashSet::new();
    let mut dirs: HashSet<Vec<String>> = HashSet::with_capacity(m.dirs.len());
    let record_ancestors = |key: &[String], ancestors: &mut HashSet<Vec<String>>| {
        for depth in 1..key.len() {
            if !ancestors.insert(key[..depth].to_vec()) {
                // Shorter prefixes were inserted with this one; stop early.
                break;
            }
        }
    };
    for f in &m.files {
        let key: Vec<String> = f.path.iter().map(|c| fold(c)).collect();
        record_ancestors(&key, &mut ancestors);
        if !files.insert(key) {
            return Err(Error::PathInvalid(format!(
                "duplicate path (case-insensitive): {:?}",
                f.path
            )));
        }
    }
    for d in &m.dirs {
        let key: Vec<String> = d.iter().map(|c| fold(c)).collect();
        record_ancestors(&key, &mut ancestors);
        if files.contains(&key) || !dirs.insert(key) {
            return Err(Error::PathInvalid(format!(
                "duplicate path (case-insensitive): {d:?}"
            )));
        }
    }
    if let Some(f) = m.files.iter().find(|f| {
        let key: Vec<String> = f.path.iter().map(|c| fold(c)).collect();
        ancestors.contains(&key)
    }) {
        return Err(Error::PathInvalid(format!(
            "file is also a directory: {:?}",
            f.path
        )));
    }
    Ok(())
}

/// Identity of a source file beyond its path, used to notice a replaced file (SR-21).
/// Serialisable because the sender's on-disk record of a send keeps it (protocol.md 7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    /// Device number.
    pub dev: u64,
    /// Inode number (or file index on platforms that expose one).
    pub ino: u64,
}

/// Facts the sender records per source at offer time so resume can tell whether the source
/// changed (protocol.md 7.6, SR-21). Never serialised to the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    /// Where the bytes are read from: a local path or a host-supplied opener.
    pub source: SourceRef,
    /// Size at offer time.
    pub size: u64,
    /// Modification time at offer time, Unix ms. For a stream source with an unknown
    /// mtime this is 0 and mtime is left out of the check (`docs/core-api.md` section 3).
    pub mtime_ms: i64,
    /// Device and inode, when the platform exposes them through `std`.
    pub file_id: Option<FileId>,
}

impl SourceInfo {
    /// Captures size, mtime and file ID from metadata of a regular file at `local_path`.
    pub fn from_metadata(local_path: PathBuf, meta: &std::fs::Metadata) -> Self {
        Self::of(SourceRef::Path(local_path), meta)
    }

    /// Captures size, mtime and file ID of any source from its metadata.
    pub fn of(source: SourceRef, meta: &std::fs::Metadata) -> Self {
        Self {
            source,
            size: meta.len(),
            mtime_ms: mtime_ms(meta),
            file_id: file_id(meta),
        }
    }

    /// The local path, for `Path` sources.
    pub fn local_path(&self) -> Option<&Path> {
        self.source.path()
    }
}

/// Modification time of `meta` in Unix ms; 0 when the platform cannot say.
pub fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    let Ok(modified) = meta.modified() else {
        return 0;
    };
    let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => ms(d),
        Err(e) => -ms(e.duration()),
    }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)] // same signature as the non-Unix version, which has no ID
pub(crate) fn file_id(meta: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
pub(crate) fn file_id(_meta: &std::fs::Metadata) -> Option<FileId> {
    // Windows file index needs an unstable std API or a winapi dependency.
    None
}

/// Builds a [`Manifest`] from local files and folders (protocol.md 7.2), recording a
/// [`SourceInfo`] per file for resume (SR-21).
///
/// Symlinks inside a walked folder are skipped, never followed. Entries are visited in
/// byte-wise name order so the same tree always yields the same fids.
#[derive(Debug)]
pub struct ManifestBuilder {
    chunk_size: u32,
    note: Option<String>,
    bundle_threshold: u64,
    files: Vec<FileEntry>,
    dirs: Vec<Vec<String>>,
    sources: Vec<SourceInfo>,
}

impl Default for ManifestBuilder {
    fn default() -> Self {
        Self::new(DEFAULT_CHUNK_SIZE)
    }
}

impl ManifestBuilder {
    /// Starts an empty manifest with the proposed chunk size.
    pub fn new(chunk_size: u32) -> Self {
        Self {
            chunk_size,
            note: None,
            bundle_threshold: BUNDLE_THRESHOLD,
            files: Vec::new(),
            dirs: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Sets the note shown to the receiver.
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Changes the bundle threshold (default 256 KiB); 0 disables bundling.
    #[must_use]
    pub fn bundle_threshold(mut self, bytes: u64) -> Self {
        self.bundle_threshold = bytes;
        self
    }

    /// Adds a file or a whole folder. The entry's own name becomes the first path
    /// component, as in the protocol.md 7.2 example.
    pub async fn add_path(&mut self, path: &Path) -> Result<()> {
        let meta = tokio::fs::symlink_metadata(path).await?;
        let name = os_name(path)?;
        if meta.is_dir() {
            self.walk_dir(path.to_path_buf(), vec![name]).await
        } else if meta.is_file() {
            self.push_file(path.to_path_buf(), vec![name], &meta);
            Ok(())
        } else {
            Err(Error::PathInvalid(format!(
                "{} is not a regular file or folder",
                path.display()
            )))
        }
    }

    /// Adds one file behind a host-supplied opener (`docs/core-api.md` section 3). Its
    /// manifest path is `[name]`, which must pass the SR-17 component rules. The opener is
    /// called once here, on a blocking thread, to check that it yields a regular file of the
    /// declared size and to record its file ID for the SR-21 resume check. The recorded
    /// mtime is the opened file's when `mtime_ms` is known and 0 otherwise, so the check
    /// compares like with like; the manifest carries `mtime_ms` as given.
    pub async fn add_stream(&mut self, src: StreamSource) -> Result<()> {
        let name: String =
            unicode_normalization::UnicodeNormalization::nfc(src.name.as_str()).collect();
        validate_component(&name)?;
        let opener = std::sync::Arc::clone(&src.opener);
        let meta = tokio::task::spawn_blocking(move || opener.open()?.metadata())
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e)))??;
        if !meta.is_file() {
            return Err(Error::BadRequest(format!(
                "{name} does not open as a regular file"
            )));
        }
        if meta.len() != src.size {
            return Err(Error::BadRequest(format!(
                "{name}: declared size {} differs from the opened file's {}",
                src.size,
                meta.len()
            )));
        }
        let mut info = SourceInfo::of(SourceRef::Opener(src.opener), &meta);
        if src.mtime_ms == 0 {
            info.mtime_ms = 0;
        }
        let fid = u32::try_from(self.files.len()).unwrap_or(u32::MAX);
        self.files.push(FileEntry {
            fid,
            path: vec![name],
            size: src.size,
            mtime: src.mtime_ms,
            mime: src.mime,
            in_bundle: src.size > 0 && src.size < self.bundle_threshold,
        });
        self.sources.push(info);
        Ok(())
    }

    async fn walk_dir(&mut self, root: PathBuf, root_rel: Vec<String>) -> Result<()> {
        // Explicit stack keeps deep trees off the async call stack. Children are pushed in
        // reverse so they pop in name order (depth-first, pre-order).
        let mut stack = vec![(root, root_rel)];
        while let Some((dir, rel)) = stack.pop() {
            let mut children = Vec::new();
            let mut rd = tokio::fs::read_dir(&dir).await?;
            while let Some(entry) = rd.next_entry().await? {
                children.push(entry);
            }
            children.sort_by_key(tokio::fs::DirEntry::file_name);
            let mut any_child = false;
            let mut subdirs = Vec::new();
            for entry in children {
                let ft = entry.file_type().await?;
                if ft.is_symlink() {
                    continue;
                }
                let child_path = entry.path();
                let mut child_rel = rel.clone();
                child_rel.push(os_name(&child_path)?);
                if ft.is_dir() {
                    any_child = true;
                    subdirs.push((child_path, child_rel));
                } else if ft.is_file() {
                    any_child = true;
                    let meta = entry.metadata().await?;
                    self.push_file(child_path, child_rel, &meta);
                }
            }
            if !any_child {
                self.dirs.push(rel);
            }
            stack.extend(subdirs.into_iter().rev());
        }
        Ok(())
    }

    fn push_file(&mut self, local: PathBuf, rel: Vec<String>, meta: &std::fs::Metadata) {
        let fid = u32::try_from(self.files.len()).unwrap_or(u32::MAX);
        let info = SourceInfo::from_metadata(local, meta);
        self.files.push(FileEntry {
            fid,
            path: rel,
            size: info.size,
            mtime: info.mtime_ms,
            mime: None,
            in_bundle: info.size > 0 && info.size < self.bundle_threshold,
        });
        self.sources.push(info);
    }

    /// Finishes the manifest: fresh ID, `totalBytes`, exact `bundle.size`, then
    /// [`Manifest::validate`]. Returns the manifest and the per-fid sources.
    pub fn build(self) -> Result<(Manifest, Vec<SourceInfo>)> {
        let total_bytes = self.files.iter().map(|f| f.size).sum();
        let mut manifest = Manifest {
            id: new_transfer_id(),
            chunk_size: self.chunk_size,
            total_bytes,
            note: self.note,
            files: self.files,
            dirs: self.dirs,
            bundle: None,
        };
        if manifest.files.iter().any(|f| f.in_bundle) {
            manifest.bundle = Some(BundleInfo {
                size: crate::transfer::bundle_size(&manifest),
            });
        }
        manifest.validate()?;
        Ok((manifest, self.sources))
    }
}

fn os_name(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::PathInvalid(format!("{} has no file name", path.display())))?;
    // Paths travel NFC-normalised (protocol.md 7.2, SR-17); the local path is unchanged.
    name.to_str()
        .map(|s| unicode_normalization::UnicodeNormalization::nfc(s).collect::<String>())
        .ok_or_else(|| Error::PathInvalid(format!("{} is not UTF-8", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    fn manifest_with(paths: &[Vec<String>], dirs: &[Vec<String>]) -> Manifest {
        Manifest {
            id: "c1d2e3f4a5b6c7d8e9f00112233445f0".into(),
            chunk_size: DEFAULT_CHUNK_SIZE,
            total_bytes: 0,
            note: None,
            files: paths
                .iter()
                .enumerate()
                .map(|(i, p)| FileEntry {
                    fid: u32::try_from(i).unwrap(),
                    path: p.clone(),
                    size: 0,
                    mtime: 0,
                    mime: None,
                    in_bundle: false,
                })
                .collect(),
            dirs: dirs.to_vec(),
            bundle: None,
        }
    }

    #[test]
    fn hostile_components_rejected() {
        let long = "a".repeat(256);
        let cases: &[(&str, &str)] = &[
            ("", "empty"),
            (".", "dot"),
            ("..", "dotdot"),
            ("a/b", "slash"),
            ("a\\b", "backslash"),
            ("a\0b", "NUL"),
            ("a\u{1}b", "C0"),
            ("a\u{1f}", "C0 end"),
            ("a\u{7f}", "DEL"),
            ("a\u{85}b", "C1 NEL"),
            ("a\u{9f}b", "C1 end"),
            ("a<b", "lt"),
            ("a>b", "gt"),
            ("a:b", "colon (ADS)"),
            ("a\"b", "quote"),
            ("a|b", "pipe"),
            ("a?b", "question"),
            ("a*b", "star"),
            ("gpj\u{202E}.exe", "RLO"),
            ("a\u{202A}b", "LRE"),
            ("a\u{2066}b", "LRI"),
            ("a\u{2069}b", "PDI"),
            ("name.", "trailing dot"),
            ("name ", "trailing space"),
            ("CON", "CON"),
            ("con", "con lower"),
            ("Con.txt", "CON ext"),
            ("CON .txt", "CON space ext"),
            ("PRN", "PRN"),
            ("aux.tar.gz", "AUX double ext"),
            ("NUL", "NUL"),
            ("COM0", "COM0"),
            ("COM9.log", "COM9 ext"),
            ("LPT1", "LPT1"),
            ("lpt0", "LPT0"),
            ("COM\u{00B9}", "COM superscript 1"),
            ("LPT\u{00B2}.txt", "LPT superscript 2 ext"),
            ("com\u{00B3}", "COM superscript 3"),
            ("CONIN$", "CONIN$"),
            (&long, "256 bytes"),
            ("e\u{301}", "decomposed e acute"),
            ("\u{212B}", "angstrom sign"),
            ("\u{1100}\u{1161}", "conjoining jamo"),
        ];
        for (c, why) in cases {
            let m = manifest_with(&[vec!["ok".into(), (*c).to_owned()]], &[]);
            assert!(
                matches!(m.validate(), Err(Error::PathInvalid(_))),
                "{why} ({c:?}) was accepted"
            );
            let d = manifest_with(&[], &[vec![(*c).to_owned()]]);
            assert!(
                matches!(d.validate(), Err(Error::PathInvalid(_))),
                "{why} ({c:?}) was accepted as a dir"
            );
        }
    }

    #[test]
    fn friendly_components_accepted() {
        let max = "a".repeat(255);
        for c in [
            "IMG_0001.HEIC",
            ".bashrc",
            "CONSOLE",
            "COM10",
            "LPT",
            "com",
            "auxiliary.txt",
            "caf\u{e9}",
            "\u{d55c}\u{ae00}",
            "\u{65e5}\u{672c}",
            "a..b",
            " lead",
            &max,
        ] {
            let m = manifest_with(&[s(&["dir", c])], &[]);
            m.validate()
                .unwrap_or_else(|e| panic!("{c:?} rejected: {e}"));
        }
    }

    #[test]
    fn depth_and_length_limits() {
        let deep: Vec<String> = (0..=MAX_DEPTH).map(|i| format!("d{i}")).collect();
        assert!(matches!(
            manifest_with(&[deep], &[]).validate(),
            Err(Error::PathInvalid(_))
        ));
        let ok_depth: Vec<String> = (0..MAX_DEPTH).map(|i| format!("d{i}")).collect();
        manifest_with(&[ok_depth], &[]).validate().unwrap();
        // 5 components of 205 bytes plus 4 separators = 1029 bytes.
        let long: Vec<String> = (0..5).map(|_| "x".repeat(205)).collect();
        assert!(matches!(
            manifest_with(&[long], &[]).validate(),
            Err(Error::PathInvalid(_))
        ));
        // 4 x 255 + 3 = 1023 bytes passes.
        let fits: Vec<String> = (0..4).map(|_| "y".repeat(255)).collect();
        manifest_with(&[fits], &[]).validate().unwrap();
        assert!(matches!(
            manifest_with(&[vec![]], &[]).validate(),
            Err(Error::PathInvalid(_))
        ));
    }

    #[test]
    fn case_insensitive_duplicates_rejected() {
        let cases = [
            (vec![s(&["A", "b.txt"]), s(&["a", "B.TXT"])], vec![]),
            (vec![s(&["x", "stra\u{df}e"]), s(&["x", "STRASSE"])], vec![]),
            (vec![s(&["same"]), s(&["same"])], vec![]),
            (vec![s(&["d", "f"])], vec![s(&["D", "F"])]),
            (vec![], vec![s(&["e"]), s(&["E"])]),
            (vec![s(&["a"]), s(&["A", "child"])], vec![]),
            (vec![s(&["a"])], vec![s(&["a", "sub"])]),
        ];
        for (files, dirs) in cases {
            let m = manifest_with(&files, &dirs);
            assert!(
                matches!(m.validate(), Err(Error::PathInvalid(_))),
                "{files:?} {dirs:?} accepted"
            );
        }
        manifest_with(&[s(&["a", "x"]), s(&["a", "y"])], &[s(&["a", "z"])])
            .validate()
            .unwrap();
    }

    #[test]
    fn structural_rules() {
        let mut m = manifest_with(&[s(&["f"])], &[]);
        m.chunk_size = 3 * 64 * 1024;
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        m.chunk_size = 32 * 1024;
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        m.chunk_size = 32 * 1024 * 1024;
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        for ok in [64 * 1024, 1 << 20, 16 << 20] {
            m.chunk_size = ok;
            m.validate().unwrap();
        }
        let mut m = manifest_with(&[s(&["f"])], &[]);
        m.id = "C1D2E3F4A5B6C7D8E9F00112233445F0".into();
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        let mut m = manifest_with(&[s(&["f"])], &[]);
        m.files[0].fid = 5;
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        let mut m = manifest_with(&[s(&["f"])], &[]);
        m.total_bytes = 1;
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        let mut m = manifest_with(&[s(&["f"])], &[]);
        m.files[0].in_bundle = true;
        m.files[0].size = 1;
        m.total_bytes = 1;
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
        m.bundle = Some(BundleInfo { size: 2048 });
        m.validate().unwrap();
        m.bundle = Some(BundleInfo { size: 2047 });
        assert!(matches!(m.validate(), Err(Error::BadRequest(_))));
    }

    #[test]
    fn entry_limit() {
        let files: Vec<Vec<String>> = (0..=MAX_ENTRIES).map(|i| vec![format!("f{i}")]).collect();
        let m = manifest_with(&files, &[]);
        assert!(matches!(m.validate(), Err(Error::TooLarge(_))));
    }

    #[test]
    fn wire_json_matches_spec_example() {
        let json = r#"{"id":"c1d2e3f4a5b6c7d8e9f00112233445f0","chunkSize":1048576,"totalBytes":684388316,"note":"Holiday",
 "files":[
   {"fid":0,"path":["Holiday","IMG_0001.HEIC"],"size":2811004,"mtime":1791460800000,"mime":"image/heic"},
   {"fid":1,"path":["Holiday","clip.mp4"],"size":681574400,"mtime":1791460900000,"mime":"video/mp4"},
   {"fid":2,"path":["Holiday","notes.txt"],"size":2912,"mtime":1791460950000,"inBundle":true}],
 "dirs":[["Holiday","empty"]],
 "bundle":{"size":4608}}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        m.validate().unwrap();
        assert!(m.files[2].in_bundle);
        assert!(!m.files[0].in_bundle);
        let out = serde_json::to_value(&m).unwrap();
        let back: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(out, back);
    }

    #[tokio::test]
    async fn builder_walks_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Holiday");
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("big.bin"), vec![1u8; 300 * 1024]).unwrap();
        std::fs::write(root.join("small.txt"), b"hello").unwrap();
        std::fs::write(root.join("zero"), b"").unwrap();
        std::fs::write(root.join("sub").join("a.txt"), b"abc").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();

        let mut b = ManifestBuilder::new(DEFAULT_CHUNK_SIZE).note("hi");
        b.add_path(&root).await.unwrap();
        let (m, sources) = b.build().unwrap();
        let paths: Vec<Vec<String>> = m.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                s(&["Holiday", "big.bin"]),
                s(&["Holiday", "small.txt"]),
                s(&["Holiday", "zero"]),
                s(&["Holiday", "sub", "a.txt"]),
            ]
        );
        assert_eq!(m.dirs, vec![s(&["Holiday", "empty"])]);
        assert!(!m.files[0].in_bundle);
        assert!(m.files[1].in_bundle);
        assert!(
            !m.files[2].in_bundle,
            "zero-length files are never streamed"
        );
        assert!(m.files[3].in_bundle);
        assert_eq!(m.total_bytes, 300 * 1024 + 5 + 3);
        assert_eq!(m.bundle.unwrap().size, crate::transfer::bundle_size(&m));
        assert_eq!(sources.len(), 4);
        assert_eq!(sources[1].size, 5);
        #[cfg(unix)]
        assert!(sources[0].file_id.is_some());
        assert!(is_valid_transfer_id(&m.id));
    }
}
