//! The pairing QR code (SR-9): rendering the one-use URI to an image, and on Windows
//! keeping the window out of screenshots and screen sharing while it is shown.

use qrcode::{Color, QrCode};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Light modules around the code, as the QR spec asks (4 modules).
const QUIET_ZONE: usize = 4;
/// Pixels per module. The image is drawn at this size and scaled with pixelated sampling,
/// so module edges stay sharp at any display scale.
const SCALE: usize = 6;

/// Renders `uri` as a black-on-white QR image.
pub fn render(uri: &str) -> Result<Image, qrcode::types::QrError> {
    let code = QrCode::new(uri.as_bytes())?;
    Ok(Image::from_rgba8(pixels(code.width(), &code.to_colors())))
}

fn pixels(width: usize, modules: &[Color]) -> SharedPixelBuffer<Rgba8Pixel> {
    const WHITE: Rgba8Pixel = Rgba8Pixel {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    const BLACK: Rgba8Pixel = Rgba8Pixel {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    let side = (width + 2 * QUIET_ZONE) * SCALE;
    let side_u32 = u32::try_from(side).unwrap_or(u32::MAX);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(side_u32, side_u32);
    let out = buf.make_mut_slice();
    out.fill(WHITE);
    for (i, m) in modules.iter().enumerate() {
        if *m != Color::Dark {
            continue;
        }
        let (mx, my) = (i % width + QUIET_ZONE, i / width + QUIET_ZONE);
        for y in my * SCALE..(my + 1) * SCALE {
            let row = y * side;
            out[row + mx * SCALE..row + (mx + 1) * SCALE].fill(BLACK);
        }
    }
    buf
}

/// Excludes `window` from capture while `on`, and restores it after (SR-9).
///
/// Uses winit's `set_content_protected` (`WDA_EXCLUDEFROMCAPTURE`) on the window's own
/// handle. Only if the winit window is not available (not created yet, or another
/// backend) does it fall back to finding the window by title.
#[cfg(windows)]
pub fn set_capture_excluded(window: &slint::Window, on: bool) -> Result<(), String> {
    use slint::winit_030::WinitWindowAccessor as _;

    if window
        .with_winit_window(|w| w.set_content_protected(on))
        .is_some()
    {
        return Ok(());
    }
    set_capture_excluded_by_title(on)
}

/// Fallback for [`set_capture_excluded`]: finds the main window (title "PlayDrop") among
/// this process's top-level windows. Falls back to `WDA_MONITOR` (shows black in
/// captures) on Windows versions older than 10 2004, which lack `WDA_EXCLUDEFROMCAPTURE`.
#[cfg(windows)]
#[allow(unsafe_code)]
fn set_capture_excluded_by_title(on: bool) -> Result<(), String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
        WDA_MONITOR, WDA_NONE,
    };

    let pid = std::process::id();
    let mut after: Option<HWND> = None;
    loop {
        // SAFETY: FindWindowExW only reads the two string pointers, which are a null PCWSTR
        // and a static NUL-terminated literal from `w!`; `after` is a handle the previous
        // call returned. A failed search returns an error, never an invalid handle.
        let hwnd = unsafe { FindWindowExW(None, after, PCWSTR::null(), w!("PlayDrop")) }
            .map_err(|_| "PlayDrop window not found".to_owned())?;
        let mut owner = 0u32;
        // SAFETY: `hwnd` came from FindWindowExW above and `owner` is a live local the call
        // writes one u32 into.
        unsafe { GetWindowThreadProcessId(hwnd, Some(std::ptr::addr_of_mut!(owner))) };
        if owner == pid {
            let mode = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
            // SAFETY: `hwnd` is a top-level window owned by this process (checked above);
            // SetWindowDisplayAffinity only changes how the compositor captures it.
            let result = unsafe { SetWindowDisplayAffinity(hwnd, mode) };
            return match result {
                Ok(()) => Ok(()),
                // SAFETY: as above.
                Err(_) if on => unsafe { SetWindowDisplayAffinity(hwnd, WDA_MONITOR) }
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
        }
        after = Some(hwnd);
    }
}

/// No capture protection API on this platform. Wayland compositors do not let other
/// clients read a window without the user picking it in a portal dialog.
#[cfg(not(windows))]
// Same signature as the Windows version so callers need no cfg.
#[allow(clippy::unnecessary_wraps)]
pub fn set_capture_excluded(_window: &slint::Window, _on: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_quiet_zone() {
        let uri = "playdrop://pair?id=abcdef&a=192.168.1.5:42187&s=0123456789abcdef&e=120";
        let code = QrCode::new(uri.as_bytes()).unwrap();
        let w = code.width();
        let buf = pixels(w, &code.to_colors());
        let side = (w + 2 * QUIET_ZONE) * SCALE;
        assert_eq!(buf.width() as usize, side);
        assert_eq!(buf.height() as usize, side);
        let px = buf.as_slice();
        // Quiet zone is white; the top-left finder pattern corner module is black.
        assert_eq!(px[0].r, 255);
        let corner = QUIET_ZONE * SCALE * side + QUIET_ZONE * SCALE;
        assert_eq!(px[corner].r, 0);
        assert!(render(uri).is_ok());
    }
}
