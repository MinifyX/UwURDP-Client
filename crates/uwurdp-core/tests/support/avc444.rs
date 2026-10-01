//! What a server does to a picture before it encodes it as AVC420 or
//! AVC444 (MS-RDPEGFX 3.3.8.3): RGB to YUV 4:4:4 with BT.709 at full range,
//! then the main view (luma and averaged chroma, plain 4:2:0) and the
//! auxiliary view that carries the chroma the main view left out, in the
//! v1 or v2 layout. Shared by the dev server and uwurdp-core's unit tests
//! (included with `#[path]`); no dependencies.

#![allow(dead_code)] // Each includer uses a different subset.

/// A picture in YUV 4:4:4, one byte per sample, no padding.
pub struct Yuv444 {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Yuv444 {
    /// From tightly packed RGB, BT.709 at full range.
    pub fn from_rgb(rgb: &[u8], width: usize, height: usize) -> Self {
        let mut p = Self {
            width,
            height,
            y: Vec::with_capacity(width * height),
            u: Vec::with_capacity(width * height),
            v: Vec::with_capacity(width * height),
        };
        for px in rgb.as_chunks::<3>().0.iter().take(width * height) {
            let (r, g, b) = (f32::from(px[0]), f32::from(px[1]), f32::from(px[2]));
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            p.y.push(clamp(y));
            p.u.push(clamp((b - y) / 1.8556 + 128.0));
            p.v.push(clamp((r - y) / 1.5748 + 128.0));
        }
        p
    }

    fn at(plane: &[u8], width: usize, x: usize, y: usize) -> u8 {
        plane[y * width + x]
    }

    /// The main view as I420 (Y, then U, then V): luma as it is, chroma
    /// averaged over each 2×2 block. On its own it is a plain AVC420 picture.
    /// Width and height must be even.
    pub fn main_view(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut out = self.y.clone();
        for plane in [&self.u, &self.v] {
            for y in 0..h / 2 {
                for x in 0..w / 2 {
                    let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                        .iter()
                        .map(|(dx, dy)| u32::from(Self::at(plane, w, 2 * x + dx, 2 * y + dy)))
                        .sum();
                    out.push(u8::try_from((sum + 2) / 4).unwrap_or(255));
                }
            }
        }
        out
    }

    /// The auxiliary view of AVC444 (v1, MS-RDPEGFX 3.3.8.3.2) as I420: the
    /// odd chroma rows in its luma plane, eight U rows then eight V rows in
    /// every sixteen (B4, B5); the odd columns of the even rows in its chroma
    /// planes (B6, B7). Rows past the picture's height (when it is not a
    /// multiple of 16) are dropped.
    pub fn aux_view_v1(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut luma = vec![128u8; w * h];
        for k in 0..h / 2 {
            let row = (k & !7) + k;
            for (plane, target) in [(&self.u, row), (&self.v, row + 8)] {
                if target < h {
                    luma[target * w..(target + 1) * w]
                        .copy_from_slice(&plane[(2 * k + 1) * w..(2 * k + 2) * w]);
                }
            }
        }
        let mut out = luma;
        for plane in [&self.u, &self.v] {
            for y in 0..h / 2 {
                for x in 0..w / 2 {
                    out.push(Self::at(plane, w, 2 * x + 1, 2 * y));
                }
            }
        }
        out
    }

    /// The auxiliary view of AVC444v2 (MS-RDPEGFX 3.3.8.3.3) as I420: the
    /// odd columns of U and V side by side in its luma plane (B4, B5), the
    /// columns 4n and 4n+2 of the odd rows in its chroma planes (B6–B9).
    /// Width must be a multiple of 4, height even.
    pub fn aux_view_v2(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut out = Vec::with_capacity(w * h * 3 / 2);
        for y in 0..h {
            for plane in [&self.u, &self.v] {
                for x in 0..w / 2 {
                    out.push(Self::at(plane, w, 2 * x + 1, y));
                }
            }
        }
        for offset in [0, 2] {
            for y in 0..h / 2 {
                for plane in [&self.u, &self.v] {
                    for x in 0..w / 4 {
                        out.push(Self::at(plane, w, 4 * x + offset, 2 * y + 1));
                    }
                }
            }
        }
        out
    }
}

fn clamp(value: f32) -> u8 {
    // In range after the clamp, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = value.round().clamp(0.0, 255.0) as u8;
    byte
}
