//! What crosses the channel, in Windows' own clipboard formats: text as
//! `CF_UNICODETEXT`, rich text as `HTML Format` (CF_HTML) and pictures as
//! `CF_DIB`. Pure functions, so all of it is tested without a clipboard.

/// The biggest picture taken either way: 16384 × 16384 pixels is far beyond
/// any screenshot and still keeps one bad header from asking for gigabytes.
const MAX_IMAGE_SIDE: usize = 16_384;

/// Decodes `CF_UNICODETEXT`: UTF-16LE up to the first NUL. Windows often
/// sends the whole buffer it allocated, so whatever follows the terminator is
/// junk; broken surrogates become U+FFFD instead of failing the paste.
pub(crate) fn decode_utf16_text(data: &[u8]) -> String {
    let units = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0);
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Encodes text as `CF_UNICODETEXT`: CRLF line ends, UTF-16LE, NUL-terminated.
pub(crate) fn encode_utf16_text(text: &str) -> Vec<u8> {
    let crlf = to_crlf(text);
    let mut out = Vec::with_capacity(crlf.len() * 2 + 2);
    for unit in crlf.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Windows expects CRLF in `CF_UNICODETEXT`; plenty of Windows apps show a
/// bare LF as nothing at all.
pub(crate) fn to_crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut previous = '\0';
    for c in text.chars() {
        if c == '\n' && previous != '\r' {
            out.push('\r');
        }
        out.push(c);
        previous = c;
    }
    out
}

/// Back to the local convention: CRLF stays on Windows, becomes LF elsewhere.
pub(crate) fn from_remote_line_endings(text: &str) -> String {
    if cfg!(windows) {
        text.to_owned()
    } else {
        text.replace("\r\n", "\n")
    }
}

// ── HTML Format ──────────────────────────────────────────────────────────────

const HTML_PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
const HTML_SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";

/// Wraps an HTML fragment in the CF_HTML header Windows apps expect. The
/// offsets count bytes of the UTF-8 text; ten digits each keep the header's
/// length fixed. NUL-terminated like Windows' own.
pub(crate) fn wrap_cf_html(fragment: &str) -> Vec<u8> {
    const HEADER: usize = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\n\
         StartFragment:0000000000\r\nEndFragment:0000000000\r\n"
        .len();
    let start_html = HEADER;
    let start_fragment = start_html + HTML_PREFIX.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + HTML_SUFFIX.len();
    let mut out = format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n\
         {HTML_PREFIX}{fragment}{HTML_SUFFIX}"
    )
    .into_bytes();
    out.push(0);
    out
}

/// The fragment of a CF_HTML buffer (or the whole HTML part when the
/// fragment markers are missing or out of range). `None` if it isn't CF_HTML.
pub(crate) fn unwrap_cf_html(data: &[u8]) -> Option<String> {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let data = &data[..end];
    let header_end = find(data, b"<").unwrap_or(data.len());
    let header = std::str::from_utf8(&data[..header_end]).ok()?;
    let offset = |key: &str| -> Option<usize> {
        header
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                (k.trim() == key).then(|| v.trim().parse::<i64>().ok())?
            })
            // -1 means "not present" in the spec.
            .and_then(|v| usize::try_from(v).ok())
    };
    let pick = |start: Option<usize>, end: Option<usize>| -> Option<&[u8]> {
        let (start, end) = (start?, end?.min(data.len()));
        (start < end).then(|| &data[start..end])
    };
    let part = pick(offset("StartFragment"), offset("EndFragment"))
        .or_else(|| pick(offset("StartHTML"), offset("EndHTML")))?;
    Some(String::from_utf8_lossy(part).into_owned())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ── Pictures ────────────────────────────────────────────────────────────────

/// A picture as straight RGBA, row-major, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rgba {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Encodes `CF_DIB`: a `BITMAPINFOHEADER`, 32 bits per pixel, bottom-up BGRA.
/// Windows makes `CF_BITMAP` and `CF_DIBV5` out of it on its own.
pub(crate) fn rgba_to_dib(image: &Rgba) -> Option<Vec<u8>> {
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
        return None;
    }
    let row = w * 4;
    if image.bytes.len() < row * h {
        return None;
    }
    let size = u32::try_from(row * h).ok()?;
    let mut out = Vec::with_capacity(40 + row * h);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&i32::try_from(w).ok()?.to_le_bytes());
    out.extend_from_slice(&i32::try_from(h).ok()?.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&BI_RGB.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    // 96 DPI, no palette.
    out.extend_from_slice(&3780i32.to_le_bytes());
    out.extend_from_slice(&3780i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        for px in image.bytes[y * row..(y + 1) * row].as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Some(out)
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// One channel of a bit-field pixel, scaled to 0..=255.
fn channel(pixel: u32, mask: u32) -> Option<u8> {
    if mask == 0 {
        return None;
    }
    let value = (pixel & mask) >> mask.trailing_zeros();
    let max = mask >> mask.trailing_zeros();
    Some(((u64::from(value) * 255 + u64::from(max) / 2) / u64::from(max)) as u8)
}

/// Decodes `CF_DIB` or `CF_DIBV5`: 8 (palette), 16, 24 and 32 bits per pixel,
/// plain or with bit fields, bottom-up or top-down. A 32-bit picture whose
/// alpha is all zero (what most Windows apps put there) comes out opaque.
pub(crate) fn dib_to_rgba(data: &[u8]) -> Option<Rgba> {
    let header = usize::try_from(u32_at(data, 0)?).ok()?;
    if header < 40 || header > data.len() {
        return None;
    }
    let width = i32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
    let raw_height = i32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
    let bits = u16_at(data, 14)?;
    let compression = u32_at(data, 16)?;
    let colors_used = u32_at(data, 32)?;
    let width = usize::try_from(width).ok()?;
    let top_down = raw_height < 0;
    let height = usize::try_from(raw_height.unsigned_abs()).ok()?;
    if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return None;
    }

    let mut at = header;
    let masks = match compression {
        BI_RGB => match bits {
            16 => Some([0x7c00, 0x03e0, 0x001f, 0]),
            _ => None,
        },
        BI_BITFIELDS | BI_ALPHABITFIELDS => {
            let count = if compression == BI_ALPHABITFIELDS {
                4
            } else {
                3
            };
            if header >= 56 {
                // V4/V5 headers carry the masks (alpha included) themselves.
                Some([
                    u32_at(data, 40)?,
                    u32_at(data, 44)?,
                    u32_at(data, 48)?,
                    u32_at(data, 52)?,
                ])
            } else {
                let mut m = [0u32; 4];
                for (i, slot) in m.iter_mut().enumerate().take(count) {
                    *slot = u32_at(data, at + i * 4)?;
                }
                at += count * 4;
                Some(m)
            }
        }
        _ => return None,
    };

    let palette: Vec<[u8; 3]> = if bits <= 8 {
        let count = match colors_used {
            0 => 1usize << bits,
            n => usize::try_from(n).ok()?.min(256),
        };
        let table = data.get(at..at + count * 4)?;
        at += count * 4;
        table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| [c[2], c[1], c[0]])
            .collect()
    } else {
        Vec::new()
    };

    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let pixels = data.get(at..at.checked_add(stride.checked_mul(height)?)?)?;
    let mut out = vec![0u8; width * height * 4];
    let mut any_alpha = false;
    let alpha_mask = masks.map_or(0, |m| m[3]);
    for y in 0..height {
        let src = &pixels[if top_down { y } else { height - 1 - y } * stride..][..stride];
        let dst = &mut out[y * width * 4..(y + 1) * width * 4];
        for x in 0..width {
            let rgba: [u8; 4] = match (bits, masks) {
                (8, _) => {
                    let c = palette.get(usize::from(src[x]))?;
                    [c[0], c[1], c[2], 255]
                }
                (16 | 32, Some(m)) => {
                    let p = if bits == 16 {
                        u32::from(u16::from_le_bytes([src[x * 2], src[x * 2 + 1]]))
                    } else {
                        u32::from_le_bytes(src[x * 4..x * 4 + 4].try_into().ok()?)
                    };
                    [
                        channel(p, m[0])?,
                        channel(p, m[1])?,
                        channel(p, m[2])?,
                        channel(p, m[3]).unwrap_or(0),
                    ]
                }
                (24, None) => [src[x * 3 + 2], src[x * 3 + 1], src[x * 3], 255],
                (32, None) => [src[x * 4 + 2], src[x * 4 + 1], src[x * 4], src[x * 4 + 3]],
                _ => return None,
            };
            any_alpha |= rgba[3] != 0;
            dst[x * 4..x * 4 + 4].copy_from_slice(&rgba);
        }
    }
    let has_alpha_channel = bits == 32 && (masks.is_none() || alpha_mask != 0);
    if has_alpha_channel && !any_alpha || bits == 16 && alpha_mask == 0 {
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }
    Some(Rgba {
        width,
        height,
        bytes: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_text_stops_at_the_terminator_and_survives_junk() {
        let mut data = encode_utf16_text("a\nä😀");
        // What Windows sends: the allocation, garbage after the NUL included.
        data.extend_from_slice(&[0x41, 0x00, 0x00, 0xd8]);
        assert_eq!(decode_utf16_text(&data), "a\r\nä😀");
        // A lone surrogate is replaced, not an error.
        assert_eq!(decode_utf16_text(&[0x00, 0xd8, 0x41, 0x00]), "\u{fffd}A");
        // An odd trailing byte is ignored.
        assert_eq!(decode_utf16_text(&[0x41, 0x00, 0x42]), "A");
        assert_eq!(decode_utf16_text(&[]), "");
    }

    #[test]
    fn encoded_text_has_crlf_and_a_terminator() {
        let data = encode_utf16_text("x\ny");
        assert_eq!(data, b"x\0\r\0\n\0y\0\0\0");
    }

    #[test]
    fn line_endings() {
        assert_eq!(to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
        if !cfg!(windows) {
            assert_eq!(from_remote_line_endings("a\r\nb"), "a\nb");
        }
    }

    #[test]
    fn cf_html_round_trips_with_correct_offsets() {
        let fragment = "<b>Grüße</b>";
        let data = wrap_cf_html(fragment);
        assert_eq!(data.last(), Some(&0));
        let text = std::str::from_utf8(&data[..data.len() - 1]).expect("utf-8");
        let at = |key: &str| -> usize {
            let line = text.lines().find(|l| l.starts_with(key)).expect(key);
            line[key.len() + 1..].parse().expect("offset")
        };
        assert_eq!(&text[at("StartFragment")..at("EndFragment")], fragment);
        assert!(text[at("StartHTML")..at("EndHTML")].starts_with("<html>"));
        assert_eq!(at("EndHTML"), text.len());
        assert_eq!(unwrap_cf_html(&data).as_deref(), Some(fragment));
    }

    #[test]
    fn cf_html_without_fragment_markers_falls_back_to_the_html() {
        let body = "<p>hi</p>";
        let header = "Version:0.9\r\nStartHTML:0000000089\r\nEndHTML:0000000098\r\n\
                      StartFragment:-1\r\nEndFragment:-1\r\n";
        assert_eq!(header.len(), 89);
        let data = format!("{header}{body}");
        assert_eq!(unwrap_cf_html(data.as_bytes()).as_deref(), Some(body));
        assert_eq!(unwrap_cf_html(b"<p>no header</p>"), None);
    }

    fn sample() -> Rgba {
        // 3×2, every pixel different, half transparent in one place.
        let bytes = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, //
            1, 2, 3, 128, 40, 50, 60, 255, 200, 100, 0, 255,
        ];
        Rgba {
            width: 3,
            height: 2,
            bytes,
        }
    }

    #[test]
    fn dib_round_trips() {
        let image = sample();
        let dib = rgba_to_dib(&image).expect("encode");
        // Header, then rows of 12 bytes (already 4-aligned), bottom row first.
        assert_eq!(dib.len(), 40 + 24);
        assert_eq!(&dib[40..44], &[3, 2, 1, 128]);
        assert_eq!(dib_to_rgba(&dib), Some(image));
    }

    #[test]
    fn a_32_bit_dib_without_alpha_is_opaque() {
        let mut image = sample();
        for px in image.bytes.as_chunks_mut::<4>().0 {
            px[3] = 0;
        }
        let decoded = dib_to_rgba(&rgba_to_dib(&image).expect("encode")).expect("decode");
        assert!(decoded
            .bytes
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[3] == 255));
        assert_eq!(decoded.bytes[0..3], [255, 0, 0]);
    }

    fn header(size: u32, w: i32, h: i32, bits: u16, compression: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(&compression.to_le_bytes());
        out.extend_from_slice(&[0; 20]);
        out
    }

    #[test]
    fn a_top_down_24_bit_dib_with_row_padding() {
        // 1×2, top-down: each row is 3 bytes plus one byte of padding.
        let mut dib = header(40, 1, -2, 24, BI_RGB);
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        let image = dib_to_rgba(&dib).expect("decode");
        assert_eq!(image.bytes, vec![255, 0, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn a_dibv5_with_bit_fields_and_alpha() {
        let mut dib = header(124, 1, 1, 32, BI_BITFIELDS);
        dib.truncate(40);
        for mask in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff, 0xff00_0000] {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.resize(124, 0);
        dib.extend_from_slice(&[10, 20, 30, 40]);
        let image = dib_to_rgba(&dib).expect("decode");
        assert_eq!(image.bytes, vec![30, 20, 10, 40]);
    }

    #[test]
    fn an_8_bit_palette_dib() {
        let mut dib = header(40, 2, 1, 8, BI_RGB);
        dib[32..36].copy_from_slice(&2u32.to_le_bytes());
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        dib.extend_from_slice(&[1, 0, 0, 0]);
        let image = dib_to_rgba(&dib).expect("decode");
        assert_eq!(image.bytes, vec![0, 0, 255, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn a_16_bit_555_dib() {
        let mut dib = header(40, 1, 1, 16, BI_RGB);
        // Pure red in 5-5-5, padded to four bytes.
        dib.extend_from_slice(&0x7c00u16.to_le_bytes());
        dib.extend_from_slice(&[0, 0]);
        assert_eq!(
            dib_to_rgba(&dib).expect("decode").bytes,
            vec![255, 0, 0, 255]
        );
    }

    #[test]
    fn broken_dibs_are_refused() {
        assert_eq!(dib_to_rgba(&[]), None);
        assert_eq!(
            dib_to_rgba(&header(40, 4, 4, 32, BI_RGB)),
            None,
            "no pixels"
        );
        assert_eq!(dib_to_rgba(&header(40, 100_000, 1, 32, BI_RGB)), None);
        assert_eq!(dib_to_rgba(&header(40, 1, 1, 32, 1 /* RLE8 */)), None);
        assert_eq!(dib_to_rgba(&header(12, 1, 1, 24, BI_RGB)), None);
    }
}
