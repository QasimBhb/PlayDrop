//! The window icon, drawn in code so it needs no image decoder at run time. The same
//! shape is in `assets/playdrop.svg` for packaging.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Icon edge in pixels.
const SIZE: u32 = 64;
/// Samples per pixel edge for anti-aliasing.
const SS: u32 = 4;

/// Whether a point (in icon units, 0 to 64) is inside the white drop: a circle at the
/// bottom and a cone up to its tip.
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
/// Renders the icon.
pub fn image() -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(SIZE, SIZE);
    let total = (SS * SS) as f32;
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
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
        *px = Rgba8Pixel {
            r: mix(47.0),
            g: mix(111.0),
            b: mix(237.0),
            a: (a * 255.0).round() as u8,
        };
    }
    Image::from_rgba8(buf)
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
}
