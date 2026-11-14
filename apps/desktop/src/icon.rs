//! The window icon. The same shape is in `assets/playdrop.svg` for packaging.
//!
//! It is given to the winit window directly. Slint's own `Window.icon` property cannot be
//! used for an image made in code: Slint 1.18 sets the icon only when the image's cache key
//! changes, and an image built from a pixel buffer has no cache key, so the icon was never
//! set and Windows showed the generic program icon in the title bar (seen on a PC running
//! 0.16.0).
//!
//! On Windows the icon comes from the program's own resource (`build.rs` embeds
//! `packaging/windows/playdrop.ico`), so the title bar and the taskbar each get the size
//! drawn for them. Elsewhere it is drawn in code, so it needs no image decoder at run time.

use slint::winit_030::winit::window::Window;

/// Icon edge in pixels.
#[cfg(any(not(windows), test))]
const SIZE: u32 = 64;
/// Samples per pixel edge for anti-aliasing.
#[cfg(any(not(windows), test))]
const SS: u32 = 4;

/// Gives the window its icon as soon as the window exists.
pub fn install(app: &crate::App) {
    use slint::winit_030::WinitWindowAccessor as _;
    use slint::ComponentHandle as _;

    let weak = app.as_weak();
    let spawned = slint::spawn_local(async move {
        let Some(app) = weak.upgrade() else { return };
        match app.window().winit_window().await {
            Ok(window) => apply(&window),
            Err(e) => tracing::warn!("no window to give an icon to: {e}"),
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("could not set the window icon: {e}");
    }
}

/// The icon resource of this program (`build.rs`).
#[cfg(windows)]
const ICON_RESOURCE: u16 = 1;

/// The title bar gets the small icon and the taskbar the large one, each at the size
/// Windows shows it at on this screen, so neither is a scaled copy of the other.
#[cfg(windows)]
fn apply(window: &Window) {
    use slint::winit_030::winit::dpi::PhysicalSize;
    use slint::winit_030::winit::platform::windows::{IconExtWindows as _, WindowExtWindows as _};
    use slint::winit_030::winit::window::Icon;

    let scale = window.scale_factor();
    let load = |edge: f64| {
        // An icon edge is a few dozen pixels.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let px = (edge * scale).round().max(1.0) as u32;
        Icon::from_resource(ICON_RESOURCE, Some(PhysicalSize::new(px, px)))
    };
    match load(16.0) {
        Ok(icon) => window.set_window_icon(Some(icon)),
        Err(e) => tracing::warn!("could not load the title bar icon: {e}"),
    }
    match load(32.0) {
        Ok(icon) => window.set_taskbar_icon(Some(icon)),
        Err(e) => tracing::warn!("could not load the taskbar icon: {e}"),
    }
}

/// X11 shows this icon; a Wayland compositor takes the icon from the app ID instead.
#[cfg(not(windows))]
fn apply(window: &Window) {
    use slint::winit_030::winit::window::Icon;

    match Icon::from_rgba(rgba(), SIZE, SIZE) {
        Ok(icon) => window.set_window_icon(Some(icon)),
        Err(e) => tracing::warn!("could not build the window icon: {e}"),
    }
}

/// Whether a point (in icon units, 0 to 64) is inside the white drop: a circle at the
/// bottom and a cone up to its tip.
#[cfg(any(not(windows), test))]
fn in_drop(x: f32, y: f32) -> bool {
    const CX: f32 = 32.0;
    const CY: f32 = 38.0;
    const R: f32 = 14.0;
    const TIP: f32 = 12.0;
    let dx = x - CX;
    let dy = y - CY;
    if dx * dx + dy * dy <= R * R {
        return true;
    }
    if (TIP..CY).contains(&y) {
        // The cone meets the circle near its widest point; a straight edge reads well.
        let half = R * (y - TIP) / (CY - TIP);
        return dx.abs() <= half;
    }
    false
}

/// Whether a point is inside the rounded square background.
#[cfg(any(not(windows), test))]
fn in_background(x: f32, y: f32) -> bool {
    const R: f32 = 14.0;
    const S: f32 = 64.0;
    let cx = x.clamp(R, S - R);
    let cy = y.clamp(R, S - R);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= R * R
}

// Pixel coordinates and coverage counts are small; the casts cannot lose anything that
// matters for an icon.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
/// Renders the icon as `SIZE` by `SIZE` pixels of red, green, blue and alpha.
#[cfg(any(not(windows), test))]
fn rgba() -> Vec<u8> {
    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
    let total = (SS * SS) as f32;
    for (i, px) in buf.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (px_x, px_y) = (i as u32 % SIZE, i as u32 / SIZE);
        let (mut bg, mut fg) = (0u32, 0u32);
        for sy in 0..SS {
            for sx in 0..SS {
                let x = px_x as f32 + (sx as f32 + 0.5) / SS as f32;
                let y = px_y as f32 + (sy as f32 + 0.5) / SS as f32;
                if in_background(x, y) {
                    bg += 1;
                    if in_drop(x, y) {
                        fg += 1;
                    }
                }
            }
        }
        let a = bg as f32 / total;
        let w = if bg == 0 { 0.0 } else { fg as f32 / bg as f32 };
        // Accent blue #2f6fed blended toward white by drop coverage.
        let mix = |c: f32| (c + (255.0 - c) * w).round() as u8;
        px.copy_from_slice(&[mix(47.0), mix(111.0), mix(237.0), (a * 255.0).round() as u8]);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape() {
        assert!(in_drop(32.0, 40.0));
        assert!(in_drop(32.0, 13.0));
        assert!(!in_drop(10.0, 10.0));
        assert!(in_background(32.0, 32.0));
        assert!(!in_background(0.5, 0.5));
    }

    #[test]
    fn pixels() {
        let px = rgba();
        assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
        let at = |x: u32, y: u32| {
            let i = ((y * SIZE + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(0, 0)[3], 0, "the corner is outside the rounded square");
        assert_eq!(at(32, 40), [255, 255, 255, 255], "the drop is white");
        assert_eq!(
            at(4, 32),
            [47, 111, 237, 255],
            "the background is the accent blue"
        );
    }
}
