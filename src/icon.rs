//! Icon normalisation: scale to ≤ 256 px, encode PNG, base64.

use base64::Engine;
use image::RgbaImage;

use crate::model::Icon;

/// Longest icon edge in pixels.
pub(crate) const MAX_ICON_EDGE: u32 = 256;

/// Normalise a decoded image into the crate's [`Icon`] representation:
/// downscale so the longest edge is ≤ [`MAX_ICON_EDGE`], encode as PNG and
/// base64-encode the bytes.
pub(crate) fn icon_from_rgba(mut img: RgbaImage) -> Result<Icon, String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty icon image".into());
    }
    let longest = w.max(h);
    if longest > MAX_ICON_EDGE {
        let scale = MAX_ICON_EDGE as f32 / longest as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        img = image::imageops::resize(
            &img,
            nw,
            nh,
            image::imageops::FilterType::Lanczos3,
        );
    }
    icon_from_rgba_exact(img)
}

/// Encode an already-sized RGBA buffer as PNG + base64 (no rescaling).
pub(crate) fn icon_from_rgba_exact(img: RgbaImage) -> Result<Icon, String> {
    let (width, height) = img.dimensions();
    let mut png = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut png);
    use image::ImageEncoder;
    enc.write_image(
        img.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| format!("png encode: {e}"))?;
    Ok(Icon {
        base64: base64::engine::general_purpose::STANDARD.encode(png),
        width,
        height,
    })
}

/// Normalise any [`image::DynamicImage`] (PNG/JPEG/ICO/SVG raster, …).
/// Used by the Linux icon loader.
#[allow(dead_code)]
pub(crate) fn icon_from_dynamic(img: image::DynamicImage) -> Result<Icon, String> {
    icon_from_rgba(img.to_rgba8())
}

/// Normalise raw 32-bit BGRA pixels (Windows `GetDIBits` output).
///
/// `row_stride` is the number of bytes per row in `pixels`, which may exceed
/// `width * 4` due to alignment padding.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn icon_from_bgra(
    pixels: &[u8],
    width: u32,
    height: u32,
    row_stride: usize,
) -> Result<Icon, String> {
    if width == 0 || height == 0 {
        return Err("empty icon image".into());
    }
    let need = row_stride * height as usize;
    if pixels.len() < need {
        return Err(format!(
            "bgra buffer too small: {} < {}",
            pixels.len(),
            need
        ));
    }
    let mut img = RgbaImage::new(width, height);
    for y in 0..height as usize {
        let row = &pixels[y * row_stride..y * row_stride + width as usize * 4];
        for x in 0..width as usize {
            let px = &row[x * 4..x * 4 + 4];
            // BGRA -> RGBA
            img.put_pixel(x as u32, y as u32, image::Rgba([px[2], px[1], px[0], px[3]]));
        }
    }
    icon_from_rgba(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn downscales_long_edge() {
        let img = RgbaImage::from_fn(512, 256, |_, _| Rgba([255, 0, 0, 255]));
        let icon = icon_from_rgba(img).unwrap();
        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 128);
        // Valid base64 with PNG magic decoded.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&icon.base64)
            .unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn keeps_small_icons() {
        let img = RgbaImage::from_fn(32, 32, |_, _| Rgba([0, 255, 0, 255]));
        let icon = icon_from_rgba(img).unwrap();
        assert_eq!((icon.width, icon.height), (32, 32));
    }

    #[test]
    fn bgra_row_padding_respected() {
        // 2x2, stride 16 (padded): red in BGRA = [0, 0, 255, 255].
        let px = [0u8, 0, 255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0,
                  0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0];
        let icon = icon_from_bgra(&px, 2, 2, 16).unwrap();
        assert_eq!((icon.width, icon.height), (2, 2));
    }
}
