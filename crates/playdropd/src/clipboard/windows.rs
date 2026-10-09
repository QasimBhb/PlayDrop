//! Windows backend: `AddClipboardFormatListener` on a message-only window, `CF_UNICODETEXT`,
//! the registered `PNG` format for images, and the SR-24 exclusion formats.
//!
//! UNTESTED: written against the `windows` 0.62 API but never compiled or run, because this
//! workspace is developed on Linux. Check it on Windows 11 (Phase 2 task f, the Bitwarden test)
//! before relying on it.
//!
//! SR-24 on Windows:
//! - The watcher skips a clip that carries `ExcludeClipboardContentFromMonitorProcessing`, or
//!   `CanIncludeInClipboardHistory` = 0, or `CanUploadToCloudClipboard` = 0.
//! - [`read_current`] reports the same marks as `sensitive`.
//! - [`apply`] with `sensitive` writes all three marks next to the text.
//!
//! Images use the registered `PNG` clipboard format, which browsers, Office, Paint and the
//! Snipping Tool write and read alongside `CF_DIB`. Without an image crate there is no
//! `CF_DIB` conversion: a clip that only has `CF_DIB` is not synced, and an applied image is
//! offered as `PNG` only.
//!
//! The clipboard is opened with no owner window, as `arboard` and `clipboard-win` do; nothing
//! is delay-rendered, so no owner messages are needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::{mpsc, oneshot};

use ::windows::core::{w, PCWSTR};
use ::windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use ::windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    RemoveClipboardFormatListener, SetClipboardData,
};
use ::windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use ::windows::Win32::System::Threading::GetCurrentThreadId;
use ::windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW,
    PostThreadMessageW, RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WM_QUIT, WNDCLASSW,
};

use super::{
    short_hash, ClipContent, ClipboardSupport, LocalClip, IMAGE_MIME, MAX_IMAGE_BYTES,
    MAX_SYNC_BYTES,
};

const BACKEND: &str = "clipboard-format-listener";
/// `CF_UNICODETEXT` (the constant lives behind the `Win32_System_Ole` feature, not enabled).
const CF_UNICODETEXT: u32 = 13;
/// Our own wake-up message, posted when `WM_CLIPBOARDUPDATE` reaches the window procedure.
const WM_PD_CLIPBOARD: u32 = WM_APP + 1;
const CLASS_NAME: PCWSTR = w!("PlayDropClipboardListener");

pub(super) fn probe() -> ClipboardSupport {
    ClipboardSupport {
        supported: true,
        backend: BACKEND,
        reason: None,
    }
}

/// Registered ids of the SR-24 marker formats and `PNG` (0 when registration failed).
struct Formats {
    exclude: u32,
    history: u32,
    cloud: u32,
    png: u32,
}

#[allow(unsafe_code)]
fn formats() -> &'static Formats {
    static FORMATS: OnceLock<Formats> = OnceLock::new();
    FORMATS.get_or_init(|| {
        // SAFETY: the arguments are static NUL-terminated wide strings from `w!`.
        unsafe {
            Formats {
                exclude: RegisterClipboardFormatW(w!(
                    "ExcludeClipboardContentFromMonitorProcessing"
                )),
                history: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
                cloud: RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")),
                png: RegisterClipboardFormatW(w!("PNG")),
            }
        }
    })
}

/// SR-24 rule for Windows marks, kept pure for testing.
fn marks_sensitive(exclude_present: bool, history: Option<u32>, cloud: Option<u32>) -> bool {
    exclude_present || history == Some(0) || cloud == Some(0)
}

/// Keeps the clipboard open for its lifetime and closes it on drop.
struct OpenGuard;

impl OpenGuard {
    #[allow(unsafe_code)]
    fn open() -> anyhow::Result<Self> {
        for attempt in 1..=10u64 {
            // SAFETY: OpenClipboard has no pointer arguments; failure is returned, not UB.
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(Duration::from_millis(5 * attempt));
        }
        anyhow::bail!("another app is holding the clipboard open")
    }
}

impl Drop for OpenGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: this guard exists only while this thread has the clipboard open.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Reads a DWORD-valued marker format. Must be called with the clipboard open.
/// A present but malformed value counts as 0 (fail closed).
#[allow(unsafe_code)]
fn read_dword(_open: &OpenGuard, format: u32) -> Option<u32> {
    if format == 0 {
        return None;
    }
    // SAFETY: the clipboard is open (proved by `_open`). The handle from GetClipboardData is
    // owned by the system and valid until the clipboard closes; we lock it, read at most
    // GlobalSize bytes, and unlock before returning.
    unsafe {
        IsClipboardFormatAvailable(format).ok()?;
        let Ok(handle) = GetClipboardData(format) else {
            return Some(0);
        };
        let hglobal = HGLOBAL(handle.0);
        if GlobalSize(hglobal) < 4 {
            return Some(0);
        }
        let ptr = GlobalLock(hglobal).cast::<u32>();
        if ptr.is_null() {
            return Some(0);
        }
        let value = ptr.read_unaligned();
        let _ = GlobalUnlock(hglobal);
        Some(value)
    }
}

#[allow(unsafe_code)]
fn clip_is_marked(open: &OpenGuard) -> bool {
    let f = formats();
    // SAFETY: no pointer arguments; the clipboard is open.
    let exclude = f.exclude != 0 && unsafe { IsClipboardFormatAvailable(f.exclude) }.is_ok();
    marks_sensitive(
        exclude,
        read_dword(open, f.history),
        read_dword(open, f.cloud),
    )
}

enum TextRead {
    None,
    Text(String),
    TooLarge,
}

#[allow(unsafe_code)]
fn read_text(_open: &OpenGuard) -> TextRead {
    // SAFETY: the clipboard is open. The CF_UNICODETEXT handle is system-owned and valid until
    // the clipboard closes. We lock it, view at most GlobalSize / 2 UTF-16 units (stopping at
    // the first NUL), copy them out, and unlock.
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return TextRead::None;
        }
        let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else {
            return TextRead::None;
        };
        let hglobal = HGLOBAL(handle.0);
        let units = GlobalSize(hglobal) / 2;
        let ptr = GlobalLock(hglobal).cast::<u16>().cast_const();
        if ptr.is_null() {
            return TextRead::None;
        }
        let wide = std::slice::from_raw_parts(ptr, units);
        let len = wide.iter().position(|&u| u == 0).unwrap_or(units);
        // Every UTF-16 unit becomes at least one UTF-8 byte, so this pre-check is safe.
        let result = if len > MAX_SYNC_BYTES {
            TextRead::TooLarge
        } else {
            let text = String::from_utf16_lossy(&wide[..len]);
            if text.len() > MAX_SYNC_BYTES {
                TextRead::TooLarge
            } else if text.is_empty() {
                TextRead::None
            } else {
                TextRead::Text(text)
            }
        };
        let _ = GlobalUnlock(hglobal);
        result
    }
}

enum ImageRead {
    None,
    Png(Vec<u8>),
    TooLarge,
}

/// Reads the registered `PNG` format, at most [`MAX_IMAGE_BYTES`].
#[allow(unsafe_code)]
fn read_png(_open: &OpenGuard) -> ImageRead {
    let format = formats().png;
    if format == 0 {
        return ImageRead::None;
    }
    // SAFETY: the clipboard is open. The handle is system-owned and valid until the clipboard
    // closes. We lock it, copy at most GlobalSize bytes out, and unlock.
    unsafe {
        if IsClipboardFormatAvailable(format).is_err() {
            return ImageRead::None;
        }
        let Ok(handle) = GetClipboardData(format) else {
            return ImageRead::None;
        };
        let hglobal = HGLOBAL(handle.0);
        let size = GlobalSize(hglobal);
        if size == 0 {
            return ImageRead::None;
        }
        if size > MAX_IMAGE_BYTES {
            return ImageRead::TooLarge;
        }
        let ptr = GlobalLock(hglobal).cast::<u8>().cast_const();
        if ptr.is_null() {
            return ImageRead::None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(hglobal);
        ImageRead::Png(bytes)
    }
}

/// Reads text, or a PNG image when there is no text and `images` is on.
fn read_content(open: &OpenGuard, images: bool) -> anyhow::Result<Option<ClipContent>> {
    match read_text(open) {
        TextRead::Text(text) => return Ok(Some(ClipContent::Text(text))),
        TextRead::TooLarge => {
            anyhow::bail!("clipboard text is larger than 1 MiB and is not synced")
        }
        TextRead::None => {}
    }
    if !images {
        return Ok(None);
    }
    match read_png(open) {
        ImageRead::None => Ok(None),
        ImageRead::Png(bytes) => Ok(Some(ClipContent::Image {
            mime: IMAGE_MIME.to_owned(),
            bytes,
        })),
        ImageRead::TooLarge => {
            anyhow::bail!("clipboard image is larger than 16 MiB and is not synced")
        }
    }
}

pub(super) fn read_current(images: bool) -> anyhow::Result<Option<LocalClip>> {
    let open = OpenGuard::open()?;
    let sensitive = clip_is_marked(&open);
    Ok(read_content(&open, images)?.map(|content| LocalClip { content, sensitive }))
}

/// Copies `data` into a movable global block and hands it to the clipboard.
#[allow(unsafe_code)]
fn set_data(_open: &OpenGuard, format: u32, data: &[u8]) -> anyhow::Result<()> {
    // SAFETY: we allocate `max(len, 1)` bytes, write exactly `len` bytes into the locked block,
    // and unlock before SetClipboardData. On success the system owns the block; on failure we
    // free it ourselves.
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)).context("out of memory")?;
        let ptr = GlobalLock(block).cast::<u8>();
        if ptr.is_null() {
            let _ = GlobalFree(Some(block));
            anyhow::bail!("could not lock clipboard memory");
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        let _ = GlobalUnlock(block);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(block.0))) {
            let _ = GlobalFree(Some(block));
            return Err(e).context("could not write the clipboard");
        }
    }
    Ok(())
}

#[allow(unsafe_code)]
pub(super) fn apply(text: &str, sensitive: bool) -> anyhow::Result<()> {
    let open = OpenGuard::open()?;
    // SAFETY: the clipboard is open on this thread.
    unsafe { EmptyClipboard() }.context("could not clear the clipboard")?;
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_ne_bytes)
        .collect();
    set_data(&open, CF_UNICODETEXT, &bytes)?;
    if sensitive {
        set_sensitive_marks(&open)?;
    }
    Ok(())
}

/// SR-24: keeps the clip out of clipboard history, cloud clipboard and other monitors.
fn set_sensitive_marks(open: &OpenGuard) -> anyhow::Result<()> {
    let f = formats();
    for format in [f.exclude, f.history, f.cloud] {
        if format != 0 {
            set_data(open, format, &0u32.to_ne_bytes())?;
        }
    }
    Ok(())
}

#[allow(unsafe_code)]
pub(super) fn apply_image(bytes: &[u8], mime: &str, sensitive: bool) -> anyhow::Result<()> {
    if mime != IMAGE_MIME {
        anyhow::bail!("only PNG images can be put on the Windows clipboard");
    }
    let png = formats().png;
    anyhow::ensure!(png != 0, "the PNG clipboard format is not available");
    let open = OpenGuard::open()?;
    // SAFETY: the clipboard is open on this thread.
    unsafe { EmptyClipboard() }.context("could not clear the clipboard")?;
    set_data(&open, png, bytes)?;
    if sensitive {
        set_sensitive_marks(&open)?;
    }
    Ok(())
}

/// Handle to the listener thread.
pub(super) struct WatchHandle {
    thread_id: u32,
    stop: Arc<AtomicBool>,
}

impl WatchHandle {
    #[allow(unsafe_code)]
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // SAFETY: no pointer arguments. The thread created its message queue (by creating its
        // window) before reporting its id, so the post reaches it; if it already exited the
        // call just fails.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

pub(super) async fn spawn_watcher(
    raw: mpsc::Sender<LocalClip>,
    images: Arc<AtomicBool>,
) -> anyhow::Result<WatchHandle> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("pd-clip-watch".into())
        .spawn(move || listener_thread(&raw, &thread_stop, &images, ready_tx))
        .context("could not start the clipboard watcher thread")?;
    let thread_id = ready_rx
        .await
        .context("the clipboard watcher thread exited during start-up")??;
    Ok(WatchHandle { thread_id, stop })
}

#[allow(unsafe_code)]
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        // Funnel the notification into the queue so the loop handles it in one place.
        // SAFETY: `hwnd` is the live window this procedure was called for.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_PD_CLIPBOARD, WPARAM(0), LPARAM(0)) };
        return LRESULT(0);
    }
    // SAFETY: forwarding the unmodified arguments of a window-procedure call.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[allow(unsafe_code)]
fn create_listener_window() -> anyhow::Result<HWND> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let registered = *REGISTERED.get_or_init(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: `class` is fully initialised and its strings are static.
        unsafe { RegisterClassW(&raw const class) != 0 }
    });
    anyhow::ensure!(registered, "could not register the clipboard window class");
    // SAFETY: the class is registered above; HWND_MESSAGE makes a message-only window with no
    // UI; all string arguments are static.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("PlayDrop clipboard"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    }
    .context("could not create the clipboard window")?;
    Ok(hwnd)
}

#[allow(unsafe_code)]
fn listener_thread(
    raw: &mpsc::Sender<LocalClip>,
    stop: &AtomicBool,
    images: &AtomicBool,
    ready: oneshot::Sender<anyhow::Result<u32>>,
) {
    let hwnd = match create_listener_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: `hwnd` is the window just created on this thread.
    if let Err(e) = unsafe { AddClipboardFormatListener(hwnd) } {
        // SAFETY: as above; destroyed on its own thread.
        let _ = unsafe { DestroyWindow(hwnd) };
        let _ = ready.send(Err::<u32, _>(e).context("could not listen for clipboard changes"));
        return;
    }
    // SAFETY: no arguments.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));

    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer for the duration of the call.
        let got = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
        if got.0 == 0 || got.0 == -1 {
            break; // WM_QUIT, or an error
        }
        if msg.message == WM_CLIPBOARDUPDATE || msg.message == WM_PD_CLIPBOARD {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Some(content) = read_change(images.load(Ordering::SeqCst)) {
                let clip = LocalClip {
                    content,
                    sensitive: false,
                };
                if raw.blocking_send(clip).is_err() {
                    break;
                }
            }
            continue;
        }
        // SAFETY: `msg` was filled by GetMessageW above.
        unsafe {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    // SAFETY: `hwnd` is still our window on this thread.
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = DestroyWindow(hwnd);
    }
    tracing::debug!("clipboard watcher thread exited");
}

/// Applies the SR-24 and size rules to one change and returns the clip to report.
fn read_change(images: bool) -> Option<ClipContent> {
    let open = match OpenGuard::open() {
        Ok(open) => open,
        Err(e) => {
            tracing::debug!(error = %e, "could not open the clipboard");
            return None;
        }
    };
    if clip_is_marked(&open) {
        tracing::debug!("clipboard change carries an exclusion format; not synced");
        return None;
    }
    match read_content(&open, images) {
        Ok(Some(content)) => {
            tracing::debug!(
                kind = content.kind(),
                len = content.len(),
                hash = %short_hash(&content.hash()),
                "clipboard changed"
            );
            Some(content)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::debug!(error = %e, "clipboard change not synced");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_marks() {
        assert!(!marks_sensitive(false, None, None));
        assert!(marks_sensitive(true, None, None));
        assert!(marks_sensitive(false, Some(0), None));
        assert!(marks_sensitive(false, None, Some(0)));
        assert!(!marks_sensitive(false, Some(1), Some(1)));
    }
}
