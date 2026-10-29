//! Secrets at rest on Windows (SR-28): the data dir gets a protected DACL that only this
//! user and `SYSTEM` appear in, and `identity.key` and `pins.json` (keys, announce keys,
//! pins and grants) are sealed with DPAPI `CurrentUser` through the core's
//! [`SecretStore`] (`docs/core-api.md` section 2a).
//!
//! The core seals a plain file the first time it reads it with a store configured, so an
//! existing install migrates on its first start with this daemon. A sealed file cannot be
//! read by a daemon without the store (an older release), which then refuses to start
//! instead of making a new identity.
//!
//! The DACL lives here, not in the core: the core forbids `unsafe` and has no Windows
//! security APIs. The daemon secures the data dir before it starts the core
//! ([`crate::settings::prepare_data_dir`]), and everything the core creates inside it later
//! inherits the two entries.
//!
//! Every FFI call sits in a small function under a scoped `#[allow(unsafe_code)]` with a
//! SAFETY comment. Not compiled or tested on the Linux build machine; Windows CI runs the
//! tests at the bottom.

use std::io;
use std::path::Path;

use playdrop_core::identity::sealing::{SecretStore, SecretStoreError};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use windows::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Extra entropy mixed into every DPAPI blob, so another program running as the same user
/// cannot open these blobs with a bare `CryptUnprotectData` call. Not a secret; it
/// only has to differ from what other programs pass. Changing it orphans every sealed file.
const ENTROPY: &[u8] = b"playdrop-sealed-v1";

fn win_err(e: &windows::core::Error) -> io::Error {
    io::Error::other(e.message())
}

/// Seals with DPAPI `CurrentUser`: only this Windows user on this machine can open the
/// blobs, and the key follows the user's password, not a file kept on disk.
#[derive(Debug, Default, Clone, Copy)]
pub struct DpapiStore;

impl SecretStore for DpapiStore {
    fn seal(&self, mut plain: Vec<u8>) -> Result<Vec<u8>, SecretStoreError> {
        let sealed = protect(&plain);
        wipe(&mut plain);
        sealed.map_err(|e| SecretStoreError::new(format!("DPAPI could not seal: {e}")))
    }

    fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, SecretStoreError> {
        unprotect(&sealed).map_err(|e| SecretStoreError::new(format!("DPAPI could not open: {e}")))
    }
}

/// Overwrites a buffer that held key material. `black_box` keeps the compiler from
/// dropping the writes as dead stores; best effort, like any wipe without `zeroize`.
fn wipe(buf: &mut [u8]) {
    buf.fill(0);
    std::hint::black_box(buf);
}

/// A blob that borrows `data` for one DPAPI call. DPAPI only reads input blobs, so the
/// pointer cast to `*mut` is never written through.
fn input_blob(data: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "secret too large"))?,
        pbData: data.as_ptr().cast_mut(),
    })
}

#[allow(unsafe_code)]
fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    let input = input_blob(plain)?;
    let entropy = input_blob(ENTROPY)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` and `entropy` point at live slices for the whole call and DPAPI only
    // reads them; `output` is a valid out pointer that receives a LocalAlloc'd buffer,
    // which `take_output` copies and frees.
    unsafe {
        CryptProtectData(
            &raw const input,
            windows::core::w!("PlayDrop"),
            Some(&raw const entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut output,
        )
    }
    .map_err(|e| win_err(&e))?;
    Ok(take_output(output, false))
}

#[allow(unsafe_code)]
fn unprotect(sealed: &[u8]) -> io::Result<Vec<u8>> {
    let input = input_blob(sealed)?;
    let entropy = input_blob(ENTROPY)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`; no description is requested, so nothing else is allocated.
    unsafe {
        CryptUnprotectData(
            &raw const input,
            None,
            Some(&raw const entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut output,
        )
    }
    .map_err(|e| win_err(&e))?;
    Ok(take_output(output, true))
}

/// Copies a DPAPI output blob into a `Vec` and frees it, wiping it first when it held
/// plaintext.
#[allow(unsafe_code)]
fn take_output(output: CRYPT_INTEGER_BLOB, secret: bool) -> Vec<u8> {
    if output.pbData.is_null() {
        return Vec::new();
    }
    let len = usize::try_from(output.cbData).unwrap_or(0);
    // SAFETY: on success DPAPI sets `pbData` to a LocalAlloc'd buffer of `cbData` bytes that
    // nothing else references; it is only read and wiped here, then freed exactly once.
    unsafe {
        let buf = std::slice::from_raw_parts_mut(output.pbData, len);
        let out = buf.to_vec();
        if secret {
            wipe(buf);
        }
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        out
    }
}

/// Gives `dir` a protected DACL (no entries inherited from its parents) that grants full
/// control to the current user and `SYSTEM` only, inherited by every file and folder
/// below it. Existing children that inherit pick the new entries up; that is how an
/// install made before this change is tightened on its next start.
pub fn protect_dir(dir: &Path) -> io::Result<()> {
    let sid = current_user_sid()?;
    let descriptor = Descriptor::from_sddl(&owner_only_sddl(&sid))?;
    set_dacl(dir, &descriptor)
}

/// `D:P(A;OICI;FA;;;<user>)(A;OICI;FA;;;SY)`: protected, user and `SYSTEM` full control,
/// inherited by files (OI) and folders (CI).
fn owner_only_sddl(sid: &str) -> String {
    format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)")
}

#[allow(unsafe_code)]
fn set_dacl(dir: &Path, descriptor: &Descriptor) -> io::Result<()> {
    let mut present = windows::core::BOOL::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `descriptor` holds a valid self-relative descriptor for the call; the out
    // pointers are valid. `dacl` points into the descriptor and is used while it lives.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        )
    }
    .map_err(|e| win_err(&e))?;
    if !present.as_bool() || dacl.is_null() {
        return Err(io::Error::other("security descriptor has no DACL"));
    }
    let path = HSTRING::from(dir);
    // SAFETY: `path` is a NUL-terminated wide string and `dacl` a valid ACL inside
    // `descriptor`, both alive for the call. Owner, group and SACL are left untouched.
    let status = unsafe {
        SetNamedSecurityInfoW(
            &path,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl.cast_const()),
            None,
        )
    };
    status
        .ok()
        .map_err(|e| io::Error::other(format!("{}: {}", dir.display(), e.message())))
}

/// A security descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`,
/// freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    #[allow(unsafe_code)]
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let sddl = HSTRING::from(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid NUL-terminated wide string for the call; on success
        // `descriptor` receives a LocalAlloc'd descriptor owned by the returned value.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(|e| win_err(&e))?;
        Ok(Self(descriptor))
    }
}

impl Drop for Descriptor {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by the system with LocalAlloc (SDDL
        // conversion or GetNamedSecurityInfoW) and is freed exactly once.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}

/// A token handle closed on drop.
struct Token(HANDLE);

impl Drop for Token {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by OpenProcessToken, is owned by this value
        // alone, and is closed exactly once here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The string SID of the user this process runs as (the token user, not the owner of new
/// objects, which is the Administrators group in an elevated process).
#[allow(unsafe_code)]
fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo handle that is always valid and needs no
    // closing; `token` is a valid out pointer and is owned by `Token` below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
        .map_err(|e| win_err(&e))?;
    let token = Token(token);

    let mut needed = 0u32;
    // SAFETY: size query with no buffer; the call only writes `needed`. It reports
    // ERROR_INSUFFICIENT_BUFFER by design, so the result is ignored.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::other("token user size query failed"));
    }
    // u64 elements give the pointer alignment TOKEN_USER needs.
    let words = usize::try_from(needed).unwrap_or(usize::MAX).div_ceil(8);
    let mut buf = vec![0u64; words];
    // SAFETY: `buf` holds at least `needed` writable bytes, suitably aligned for TOKEN_USER.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(|e| win_err(&e))?;
    // SAFETY: the call above filled `buf` with a TOKEN_USER whose SID pointer points into
    // `buf`, which stays alive until after the SID is converted.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_to_string(sid)
}

#[allow(unsafe_code)]
fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points at a valid SID; on success `text` receives a LocalAlloc'd
    // NUL-terminated string that is freed below.
    unsafe { ConvertSidToStringSidW(sid, &raw mut text) }.map_err(|e| win_err(&e))?;
    // SAFETY: `text` is a valid NUL-terminated UTF-16 string from the call above.
    let out = unsafe { text.to_string() }.map_err(io::Error::other);
    // SAFETY: `text` was allocated by ConvertSidToStringSidW with LocalAlloc and is freed
    // exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    out
}

#[cfg(test)]
mod tests {
    use windows::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows::Win32::Security::{
        AclSizeInformation, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, SE_DACL_PROTECTED,
    };

    use super::*;

    /// `ACCESS_ALLOWED_ACE_TYPE`.
    const ACCESS_ALLOWED: u8 = 0;
    /// `FILE_ALL_ACCESS`, what `FA` in SDDL expands to.
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;

    #[test]
    fn dpapi_round_trip_and_tamper() {
        let store = DpapiStore;
        let plain = b"{\"device\":\"secret key bytes\"}".to_vec();
        let sealed = store.seal(plain.clone()).unwrap();
        assert_ne!(sealed, plain, "sealed bytes differ from the plain ones");
        assert!(
            !sealed.windows(6).any(|w| w == b"secret"),
            "no plain text in the blob"
        );
        assert_eq!(store.open(sealed.clone()).unwrap(), plain);

        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(store.open(tampered).is_err(), "a flipped bit fails to open");
        assert!(store.open(b"not a dpapi blob".to_vec()).is_err());
    }

    #[test]
    #[allow(unsafe_code)]
    fn dpapi_entropy_is_required() {
        let sealed = protect(b"k").unwrap();
        let input = input_blob(&sealed).unwrap();
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `unprotect`, without the entropy blob.
        let r = unsafe {
            CryptUnprotectData(
                &raw const input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut output,
            )
        };
        let _ = take_output(output, true);
        assert!(
            r.is_err(),
            "a blob cannot be opened without PlayDrop's entropy"
        );
    }

    /// The data dir's DACL as `(protected, [(sid, mask, flags)])`.
    #[allow(unsafe_code)]
    fn read_dacl(dir: &Path) -> (bool, Vec<(String, u32, u8)>) {
        let path = HSTRING::from(dir);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `path` is a valid wide string; `dacl` and `sd` are valid out pointers;
        // `sd` is LocalAlloc'd and freed by `Descriptor` below, and `dacl` points into it.
        let status = unsafe {
            GetNamedSecurityInfoW(
                &path,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&raw mut dacl),
                None,
                &raw mut sd,
            )
        };
        status.ok().unwrap();
        let sd = Descriptor(sd);
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: `sd.0` is a valid descriptor; out pointers are valid.
        unsafe { GetSecurityDescriptorControl(sd.0, &raw mut control, &raw mut revision) }.unwrap();
        let mut info = ACL_SIZE_INFORMATION::default();
        // SAFETY: `dacl` is a valid ACL inside `sd`; `info` is writable for its size.
        unsafe {
            GetAclInformation(
                dacl,
                (&raw mut info).cast(),
                u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).unwrap(),
                AclSizeInformation,
            )
        }
        .unwrap();
        let mut aces = Vec::new();
        for i in 0..info.AceCount {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `i` is below the ACE count; `ace` receives a pointer into `dacl`.
            unsafe { GetAce(dacl, i, &raw mut ace) }.unwrap();
            // SAFETY: every ACE starts with an ACE_HEADER; only ACCESS_ALLOWED entries are
            // read further, whose layout is ACCESS_ALLOWED_ACE with the SID at SidStart.
            let (kind, flags, mask, sid) = unsafe {
                let a = ace.cast::<ACCESS_ALLOWED_ACE>();
                let kind = (*a).Header.AceType;
                let flags = (*a).Header.AceFlags;
                let sid = if kind == ACCESS_ALLOWED {
                    sid_to_string(PSID((&raw mut (*a).SidStart).cast())).unwrap()
                } else {
                    String::new()
                };
                (kind, flags, (*a).Mask, sid)
            };
            assert_eq!(kind, ACCESS_ALLOWED, "only allow entries");
            aces.push((sid, mask, flags));
        }
        (control & SE_DACL_PROTECTED.0 != 0, aces)
    }

    #[test]
    fn data_dir_gets_owner_only_protected_dacl() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        crate::settings::prepare_data_dir(&dir).unwrap();
        // A file created afterwards (as the core does) inherits the same two entries.
        std::fs::write(dir.join("pins.json"), b"{}").unwrap();

        let me = current_user_sid().unwrap();
        let (protected, aces) = read_dacl(&dir);
        assert!(protected, "inheritance from the temp dir is cut");
        let mut sids: Vec<&str> = aces.iter().map(|(s, _, _)| s.as_str()).collect();
        sids.sort_unstable();
        let mut want = vec![me.as_str(), "S-1-5-18"];
        want.sort_unstable();
        assert_eq!(sids, want, "exactly the user and SYSTEM");
        assert!(aces.iter().all(|(_, m, _)| *m == FILE_ALL_ACCESS));

        let (_, file_aces) = read_dacl(&dir.join("pins.json"));
        let mut file_sids: Vec<&str> = file_aces.iter().map(|(s, _, _)| s.as_str()).collect();
        file_sids.sort_unstable();
        assert_eq!(file_sids, want, "files inside inherit only the two entries");

        // Running it again on the existing dir is fine (every start does).
        crate::settings::prepare_data_dir(&dir).unwrap();
        assert_eq!(read_dacl(&dir).1.len(), 2);
    }
}
