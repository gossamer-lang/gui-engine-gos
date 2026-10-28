// PNG files in and out of the renderer's straight (not premultiplied) RGBA8.

/// A decoded image: `rgba` holds `width * height` pixels, row by row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Image {
    pub width: i64,
    pub height: i64,
    pub rgba: Vec<u8>,
}

/// The PNG file holding `width * height` RGBA8 pixels; `None` when the
/// dimensions do not match the pixels.
pub fn encode(width: i64, height: i64, rgba: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if rgba.len() != (w as usize).checked_mul(h as usize)?.checked_mul(4)? {
        return None;
    }
    let mut out = vec![];
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Maximum RGBA output for the default decoder; larger images need an
/// explicit application budget passed to `decode_with_limit`.
pub const DECODE_BYTE_LIMIT: usize = 64 * 1024 * 1024;

/// The pixels of a PNG file, every color type widened to RGBA8, capped at 64 MiB.
pub fn decode(bytes: &[u8]) -> Option<Image> {
    decode_with_limit(bytes, DECODE_BYTE_LIMIT)
}

/// Rejects dimensions before allocating frame storage when RGBA output would
/// exceed `limit`. PNG internal buffers and the temporary frame buffer each
/// also have this limit. Peak decoder storage can be three times the limit.
pub fn decode_with_limit(bytes: &[u8], limit: usize) -> Option<Image> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: limit });
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let rgba_bytes = (info.width as usize)
        .checked_mul(info.height as usize)?
        .checked_mul(4)?;
    if rgba_bytes > limit || reader.output_buffer_size() > limit {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => data
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        // Expansion turns indexed color into RGB or RGBA before it gets here.
        png::ColorType::Indexed => return None,
    };
    Some(Image {
        width: i64::from(info.width),
        height: i64::from(info.height),
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_survive_an_encode_and_decode() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 255];
        let file = encode(2, 2, &rgba).unwrap();
        assert_eq!(
            decode(&file),
            Some(Image {
                width: 2,
                height: 2,
                rgba
            })
        );
    }

    #[test]
    fn mismatched_dimensions_encode_nothing() {
        assert_eq!(encode(3, 2, &[0; 16]), None);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn output_dimensions_are_bounded_before_decode_allocation() {
        let bytes = encode(100, 100, &vec![0; 40_000]).unwrap();
        assert!(decode_with_limit(&bytes, 39_999).is_none());
        assert!(decode_with_limit(&bytes, 0).is_none());
        assert!(decode_with_limit(&bytes, 256 * 1024).is_some());
        assert!(encode(i64::MAX, i64::MAX, &[]).is_none());
    }
}
