//! H.264 through Cisco's OpenH264 library, loaded at run time.
//!
//! UwURDP does not ship an H.264 decoder. The app downloads Cisco's prebuilt
//! OpenH264 binary to the user's machine when they turn H.264 on — the only
//! way that binary comes with Cisco's patent license — and hands its path
//! here. The `openh264` crate checks the file's SHA-256 against the releases
//! it knows before loading it, so nothing else ever gets loaded.

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource as _;
use openh264::OpenH264API;
use std::path::Path;

/// A decoded picture: `width`×`height` straight RGBA.
pub(crate) struct Picture<'a> {
    pub width: u16,
    pub height: u16,
    pub rgba: &'a [u8],
}

pub(crate) struct H264Decoder {
    decoder: Decoder,
    annex_b: Vec<u8>,
    rgba: Vec<u8>,
}

impl H264Decoder {
    /// Loads the OpenH264 library at `path`; fails for a file that is not a
    /// known Cisco release.
    pub fn load(path: &Path) -> Result<Self, String> {
        let api = OpenH264API::from_blob_path(path).map_err(|e| e.to_string())?;
        Self::with_api(api)
    }

    /// The decoder compiled from source, for tests only: a source build does
    /// not carry Cisco's patent license, so the app never uses it.
    #[cfg(test)]
    pub fn from_source() -> Result<Self, String> {
        Self::with_api(OpenH264API::from_source())
    }

    fn with_api(api: OpenH264API) -> Result<Self, String> {
        let decoder =
            Decoder::with_api_config(api, DecoderConfig::new()).map_err(|e| e.to_string())?;
        Ok(Self {
            decoder,
            annex_b: Vec::new(),
            rgba: Vec::new(),
        })
    }

    /// Decodes one access unit. RDP carries H.264 in Annex B form (start
    /// codes); some servers (IronRDP's) send 4-byte length prefixes instead,
    /// which are rewritten first. `None` when the data held no picture.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture<'_>>, String> {
        let stream = if is_annex_b(data) {
            data
        } else {
            length_prefixed_to_annex_b(data, &mut self.annex_b);
            &self.annex_b
        };
        let Some(yuv) = self.decoder.decode(stream).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let (width, height) = yuv.dimensions();
        let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
            return Err(format!("picture too large: {width}x{height}"));
        };
        self.rgba.resize(width * height * 4, 0);
        yuv.write_rgba8(&mut self.rgba);
        Ok(Some(Picture {
            width: w,
            height: h,
            rgba: &self.rgba,
        }))
    }
}

fn is_annex_b(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])
}

/// Turns 4-byte big-endian length prefixes into start codes. A length that
/// runs past the end ends the conversion; what came before still decodes.
fn length_prefixed_to_annex_b(data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut rest = data;
    while let Some((len, tail)) = rest.split_first_chunk::<4>() {
        let len = usize::try_from(u32::from_be_bytes(*len)).unwrap_or(usize::MAX);
        let Some(nal) = tail.get(..len) else {
            break;
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        rest = &tail[len..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_codes_are_recognized() {
        assert!(is_annex_b(&[0, 0, 0, 1, 0x67]));
        assert!(is_annex_b(&[0, 0, 1, 0x67]));
        assert!(!is_annex_b(&[0, 0, 0, 5, 0x67]));
    }

    #[test]
    fn length_prefixes_become_start_codes() {
        let mut out = Vec::new();
        length_prefixed_to_annex_b(&[0, 0, 0, 2, 0xAA, 0xBB, 0, 0, 0, 1, 0xCC], &mut out);
        assert_eq!(out, vec![0, 0, 0, 1, 0xAA, 0xBB, 0, 0, 0, 1, 0xCC]);
        // A truncated NAL is dropped, the ones before it stay.
        length_prefixed_to_annex_b(&[0, 0, 0, 1, 0xAA, 0, 0, 0, 9, 0xBB], &mut out);
        assert_eq!(out, vec![0, 0, 0, 1, 0xAA]);
    }

    #[test]
    fn a_file_that_is_not_openh264_is_refused() {
        let dir = std::env::temp_dir().join(format!("uwurdp-h264-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let fake = dir.join("openh264.dll");
        std::fs::write(&fake, b"not a library").expect("write");
        assert!(H264Decoder::load(&fake).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
