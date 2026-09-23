//! Pixel buffers of the graphics pipeline: surfaces, cache entries and the
//! output the page sees.
//!
//! Everything is straight RGBA, row-major, without padding — the layout the
//! page draws — so composing a surface onto the output is a row copy. Every
//! operation clips against the buffers it touches: a server that sends a
//! rectangle outside a surface gets that part ignored, never a panic.

use crate::dirty::Rect;

/// An RGBA pixel buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pixels {
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

impl Pixels {
    /// Opaque black.
    pub fn new(width: u16, height: u16) -> Self {
        let mut pixels = Self {
            width,
            height,
            data: vec![0; usize::from(width) * usize::from(height) * 4],
        };
        pixels.clear();
        pixels
    }

    pub fn stride(&self) -> usize {
        usize::from(self.width) * 4
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Back to opaque black.
    pub fn clear(&mut self) {
        for px in self.data.as_chunks_mut::<4>().0 {
            *px = [0, 0, 0, 0xFF];
        }
    }

    /// Fills `rect` with one colour; returns what was actually filled.
    pub fn fill(&mut self, rect: Rect, rgba: [u8; 4]) -> Rect {
        let rect = rect.clip(self.width, self.height);
        let stride = self.stride();
        for y in 0..usize::from(rect.h) {
            let start = (usize::from(rect.y) + y) * stride + usize::from(rect.x) * 4;
            let row = &mut self.data[start..start + usize::from(rect.w) * 4];
            for px in row.as_chunks_mut::<4>().0 {
                *px = rgba;
            }
        }
        rect
    }

    /// Copies a `w`×`h` block of RGBA pixels (`src_stride` bytes per row) to
    /// `x`, `y`. Parts outside the buffer, or missing from `src`, are skipped.
    /// Returns the rectangle that was written.
    pub fn write(&mut self, x: u16, y: u16, w: u16, h: u16, src: &[u8], src_stride: usize) -> Rect {
        let target = Rect::new(x, y, w, h).clip(self.width, self.height);
        if target.is_empty() {
            return target;
        }
        let stride = self.stride();
        let row_bytes = usize::from(target.w) * 4;
        let mut written_rows = 0;
        for row in 0..usize::from(target.h) {
            let from = row * src_stride;
            let Some(src_row) = src.get(from..from + row_bytes) else {
                break;
            };
            let to = (usize::from(target.y) + row) * stride + usize::from(target.x) * 4;
            self.data[to..to + row_bytes].copy_from_slice(src_row);
            written_rows += 1;
        }
        Rect::new(target.x, target.y, target.w, written_rows)
    }

    /// Like [`write`](Self::write), but only where `clip` (in this buffer's
    /// coordinates) allows. Returns the parts written.
    #[allow(clippy::too_many_arguments)]
    pub fn write_clipped(
        &mut self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        src: &[u8],
        src_stride: usize,
        clip: &[Rect],
    ) -> Vec<Rect> {
        let block = Rect::new(x, y, w, h);
        let mut written = Vec::new();
        for c in clip {
            let part = intersect(&block, c);
            if part.is_empty() {
                continue;
            }
            let offset = usize::from(part.y - y) * src_stride + usize::from(part.x - x) * 4;
            let Some(src) = src.get(offset..) else {
                continue;
            };
            let done = self.write(part.x, part.y, part.w, part.h, src, src_stride);
            if !done.is_empty() {
                written.push(done);
            }
        }
        written
    }

    /// Copies `rect` of `src` (clipped to it) to `x`, `y` here, without an
    /// intermediate buffer. Returns what was written.
    pub fn blit(&mut self, src: &Pixels, rect: Rect, x: u16, y: u16) -> Rect {
        let rect = rect.clip(src.width, src.height);
        let offset = usize::from(rect.y) * src.stride() + usize::from(rect.x) * 4;
        let from = src.data.get(offset..).unwrap_or(&[]);
        self.write(x, y, rect.w, rect.h, from, src.stride())
    }

    /// The pixels of `rect` (clipped), tightly packed.
    pub fn read(&self, rect: Rect) -> (Rect, Vec<u8>) {
        let rect = rect.clip(self.width, self.height);
        let stride = self.stride();
        let row_bytes = usize::from(rect.w) * 4;
        let mut out = Vec::with_capacity(row_bytes * usize::from(rect.h));
        for y in 0..usize::from(rect.h) {
            let start = (usize::from(rect.y) + y) * stride + usize::from(rect.x) * 4;
            out.extend_from_slice(&self.data[start..start + row_bytes]);
        }
        (rect, out)
    }
}

/// The overlap of two rectangles, empty if there is none.
pub(crate) fn intersect(a: &Rect, b: &Rect) -> Rect {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = (u32::from(a.x) + u32::from(a.w)).min(u32::from(b.x) + u32::from(b.w));
    let bottom = (u32::from(a.y) + u32::from(a.h)).min(u32::from(b.y) + u32::from(b.h));
    if right <= u32::from(left) || bottom <= u32::from(top) {
        return Rect::new(0, 0, 0, 0);
    }
    Rect::new(
        left,
        top,
        u16::try_from(right - u32::from(left)).unwrap_or(0),
        u16::try_from(bottom - u32::from(top)).unwrap_or(0),
    )
}

/// A rectangle from exclusive edges, as RDPGFX_RECT16 carries them. Returns
/// an empty one for inverted edges.
pub(crate) fn from_edges(left: u16, top: u16, right: u16, bottom: u16) -> Rect {
    Rect::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &Pixels, x: usize, y: usize) -> [u8; 4] {
        let i = (y * usize::from(p.width) + x) * 4;
        p.data[i..i + 4].try_into().expect("4 bytes")
    }

    #[test]
    fn new_buffers_are_opaque_black() {
        let p = Pixels::new(3, 2);
        assert!(p.data.chunks(4).all(|px| px == [0, 0, 0, 255]));
    }

    #[test]
    fn fill_clips_to_the_buffer() {
        let mut p = Pixels::new(4, 4);
        let filled = p.fill(Rect::new(2, 2, 10, 10), [1, 2, 3, 255]);
        assert_eq!(filled, Rect::new(2, 2, 2, 2));
        assert_eq!(at(&p, 3, 3), [1, 2, 3, 255]);
        assert_eq!(at(&p, 1, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn write_clips_and_survives_short_sources() {
        let mut p = Pixels::new(4, 4);
        let src = vec![9u8; 3 * 3 * 4];
        let done = p.write(2, 2, 3, 3, &src, 12);
        assert_eq!(done, Rect::new(2, 2, 2, 2));
        assert_eq!(at(&p, 3, 3), [9, 9, 9, 9]);
        // A source with only one row writes one row.
        let done = p.write(0, 0, 2, 2, &[5u8; 8], 8);
        assert_eq!(done, Rect::new(0, 0, 2, 1));
    }

    #[test]
    fn write_clipped_only_touches_the_clip() {
        let mut p = Pixels::new(8, 8);
        let src = vec![3u8; 4 * 4 * 4];
        let written = p.write_clipped(0, 0, 4, 4, &src, 16, &[Rect::new(2, 2, 10, 10)]);
        assert_eq!(written, vec![Rect::new(2, 2, 2, 2)]);
        assert_eq!(at(&p, 1, 1), [0, 0, 0, 255]);
        assert_eq!(at(&p, 2, 2), [3, 3, 3, 3]);
    }

    #[test]
    fn blit_copies_between_buffers_and_clips() {
        let mut src = Pixels::new(4, 4);
        src.fill(Rect::new(1, 1, 2, 2), [7, 7, 7, 255]);
        let mut dst = Pixels::new(4, 4);
        let done = dst.blit(&src, Rect::new(1, 1, 10, 10), 2, 2);
        assert_eq!(done, Rect::new(2, 2, 2, 2));
        assert_eq!(at(&dst, 2, 2), [7, 7, 7, 255]);
        assert_eq!(at(&dst, 3, 3), [7, 7, 7, 255]);
        assert_eq!(at(&dst, 1, 1), [0, 0, 0, 255]);
        assert!(dst.blit(&src, Rect::new(4, 4, 1, 1), 0, 0).is_empty());
    }

    #[test]
    fn intersections() {
        assert_eq!(
            intersect(&Rect::new(0, 0, 10, 10), &Rect::new(5, 5, 10, 10)),
            Rect::new(5, 5, 5, 5)
        );
        assert!(intersect(&Rect::new(0, 0, 5, 5), &Rect::new(5, 0, 5, 5)).is_empty());
        assert_eq!(from_edges(2, 3, 10, 4), Rect::new(2, 3, 8, 1));
        assert!(from_edges(5, 5, 2, 9).is_empty());
    }
}
