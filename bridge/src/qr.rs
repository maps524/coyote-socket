//! QR rendering for the pairing URL.
//!
//! Emits SVG rather than a raster: it scales to whatever the tray popup or
//! browser window is, needs no image encoder, and is a handful of bytes.
//! Rendered server-side so the pairing page has no script and no assets — it
//! has to work on a phone that has never loaded this origin before.

use qrcode::{types::Color, QrCode};

/// Modules of quiet zone. Four is the spec minimum; scanners are unreliable
/// with less.
const QUIET_ZONE: u32 = 4;

/// Render `data` as a standalone SVG document.
///
/// The viewBox is in module units, so the caller sizes it with CSS and the
/// code stays crisp at any resolution.
pub fn to_svg(data: &str) -> Result<String, qrcode::types::QrError> {
    let code = QrCode::new(data.as_bytes())?;
    let width = code.width() as u32;
    let colors = code.to_colors();
    let total = width + QUIET_ZONE * 2;

    let mut svg = String::with_capacity(4096);
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {total} {total}" shape-rendering="crispEdges" role="img" aria-label="Pairing URL">"#
    ));
    // Always a light background regardless of page theme — a QR on a dark
    // card with inverted colours does not scan on many phones.
    svg.push_str(&format!(
        r##"<rect width="{total}" height="{total}" fill="#ffffff"/><g fill="#000000">"##
    ));

    // Coalesce horizontal runs so the document stays small.
    for y in 0..width {
        let mut x = 0;
        while x < width {
            if colors[(y * width + x) as usize] == Color::Dark {
                let start = x;
                while x < width && colors[(y * width + x) as usize] == Color::Dark {
                    x += 1;
                }
                svg.push_str(&format!(
                    r#"<rect x="{}" y="{}" width="{}" height="1"/>"#,
                    start + QUIET_ZONE,
                    y + QUIET_ZONE,
                    x - start
                ));
            } else {
                x += 1;
            }
        }
    }

    svg.push_str("</g></svg>");
    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_scannable_looking_document() {
        let svg = to_svg("http://192.168.1.10:8787/").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("viewBox"));
        assert!(svg.contains("<rect"), "should contain dark modules");
    }

    #[test]
    fn quiet_zone_is_included_in_the_viewbox() {
        let svg = to_svg("x").unwrap();
        // Version 1 is 21 modules; +4 either side = 29.
        assert!(svg.contains("viewBox=\"0 0 29 29\""), "got: {}", &svg[..80]);
    }

    #[test]
    fn handles_a_long_url() {
        assert!(to_svg(&format!("http://{}/", "a".repeat(200))).is_ok());
    }
}
