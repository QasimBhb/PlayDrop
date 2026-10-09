//! Windows process and token helpers shared by the named pipe server and client (SR-27).
//!
//! Every FFI call sits in a small function under a scoped `#[allow(unsafe_code)]` with a
//! SAFETY comment. Not compiled or tested on the Linux build machine.

use std::io;
use std::os::windows::io::AsRawHandle;

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `ERROR_FILE_NOT_FOUND`: no pipe of that name, the daemon is not running.
pub(crate) const ERROR_FILE_NOT_FOUND: i32 = 2;
/// `ERROR_ACCESS_DENIED`: `FILE_FLAG_FIRST_PIPE_INSTANCE` lost, another daemon owns the pipe.
pub(crate) const ERROR_ACCESS_DENIED: i32 = 5;
/// `ERROR_PIPE_BUSY`: every instance is connected; retry shortly.
pub(crate) const ERROR_PIPE_BUSY: i32 = 231;

fn win_err(e: &windows::core::Error) -> io::Error {
    io::Error::other(e.message())
}

/// A kernel handle closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by OpenProcess or OpenProcessToken, is owned
        // by this value alone, and is closed exactly once here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The string SID of the user this process runs as.
#[allow(unsafe_code)]
pub(crate) fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing and is always
    // valid for the calling process.
    let process = unsafe { GetCurrentProcess() };
    token_user_sid(process)
}

/// The string SID of the user process `pid` runs as.
pub(crate) fn process_user_sid(pid: u32) -> io::Result<String> {
    let process = open_process(pid)?;
    token_user_sid(process.0)
}

/// The full Win32 image path of process `pid`.
#[allow(unsafe_code)]
pub(crate) fn process_image_path(pid: u32) -> io::Result<String> {
    let process = open_process(pid)?;
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` is writable for `len` UTF-16 units; the call writes at most that many
    // and updates `len` to the count written, excluding the terminator.
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        )
    }
    .map_err(|e| win_err(&e))?;
    let len = usize::try_from(len).unwrap_or(buf.len()).min(buf.len());
    String::from_utf16(&buf[..len]).map_err(io::Error::other)
}

/// PID of the client connected to a server pipe instance.
#[allow(unsafe_code)]
pub(crate) fn pipe_client_pid(pipe: &impl AsRawHandle) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle is a live named pipe server instance borrowed for this call; `pid`
    // is a valid out pointer.
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &raw mut pid) }
        .map_err(|e| win_err(&e))?;
    Ok(pid)
}

/// PID of the server process at the other end of a client pipe.
#[allow(unsafe_code)]
pub(crate) fn pipe_server_pid(pipe: &impl AsRawHandle) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle is a live named pipe client borrowed for this call; `pid` is a
    // valid out pointer.
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &raw mut pid) }
        .map_err(|e| win_err(&e))?;
    Ok(pid)
}

#[allow(unsafe_code)]
fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: plain FFI call; the returned handle is owned by `OwnedHandle` and closed on
    // drop.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| win_err(&e))?;
    Ok(OwnedHandle(handle))
}

#[allow(unsafe_code)]
fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is a valid process handle for the duration of the call; `token` is
    // a valid out pointer and the opened token is owned by `OwnedHandle` below.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }.map_err(|e| win_err(&e))?;
    let token = OwnedHandle(token);

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

/// A self-relative security descriptor whose DACL grants the given user SID and `SYSTEM`
/// generic access and nobody else (`docs/ipc.md` section 3).
pub(crate) struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable after creation and only read by CreateNamedPipeW;
// the pointer is owned by this value and freed once on drop.
#[allow(unsafe_code)]
unsafe impl Send for SecurityDescriptor {}
// SAFETY: as above; shared references only ever read the descriptor.
#[allow(unsafe_code)]
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    /// `D:P(A;;GA;;;<sid>)(A;;GA;;;SY)`: protected DACL, user and SYSTEM only.
    #[allow(unsafe_code)]
    pub(crate) fn user_and_system(sid: &str) -> io::Result<Self> {
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
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

    /// Security attributes pointing at this descriptor. Valid while `self` lives.
    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: self.0 .0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for SecurityDescriptor {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the descriptor came from ConvertStringSecurityDescriptorToSecurityDescriptorW
        // (LocalAlloc) and is freed exactly once.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}
