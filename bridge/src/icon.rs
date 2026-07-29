//! The tray/window icon, generated rather than shipped as a file.
//!
//! Lives in the library, outside the `tray` feature, because two different
//! front ends need the same bytes: the headless binary's `tray-icon` tray and
//! the Tauri app's `tauri::tray`. Generating it keeps the spike free of an
//! asset pipeline, and keeps the two trays visually identical without a shared
//! build step.

/// Side length in pixels of the generated icon.
pub const SIZE: u32 = 32;

/// A lightning bolt as RGBA8, `SIZE * SIZE * 4` bytes.
///
/// Matches the app's bolt wordmark closely enough to be recognisable at tray
/// size, in the app's amber accent.
pub fn bolt_rgba() -> Vec<u8> {
    // 16×16 bitmask, one u16 per row, MSB = leftmost pixel. Scaled ×2.
    const BOLT: [u16; 16] = [
        0b0000_0111_1100_0000,
        0b0000_1111_1000_0000,
        0b0001_1111_0000_0000,
        0b0011_1110_0000_0000,
        0b0111_1100_0000_0000,
        0b1111_1111_1110_0000,
        0b1111_1111_1110_0000,
        0b0000_0011_1110_0000,
        0b0000_0111_1100_0000,
        0b0000_0111_1000_0000,
        0b0000_1111_0000_0000,
        0b0001_1110_0000_0000,
        0b0011_1100_0000_0000,
        0b0111_1000_0000_0000,
        0b0111_0000_0000_0000,
        0b1110_0000_0000_0000,
    ];

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let bit = (BOLT[(y / 2) as usize] >> (15 - (x / 2))) & 1;
            if bit == 1 {
                rgba.extend_from_slice(&[0xFB, 0xBF, 0x24, 0xFF]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_is_exactly_what_an_rgba_consumer_expects() {
        // Both tray implementations reject a buffer whose length disagrees
        // with the dimensions, and the failure is a runtime panic rather than
        // a compile error, so it is worth an assertion.
        assert_eq!(bolt_rgba().len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn the_icon_has_both_opaque_and_transparent_pixels() {
        // A fully transparent icon looks exactly like a missing one, which is
        // the confusing failure mode on Windows.
        let rgba = bolt_rgba();
        let alphas: Vec<u8> = rgba.chunks(4).map(|p| p[3]).collect();
        assert!(alphas.iter().any(|&a| a == 0xFF), "nothing would be drawn");
        assert!(alphas.iter().any(|&a| a == 0), "no transparency at all");
    }
}
