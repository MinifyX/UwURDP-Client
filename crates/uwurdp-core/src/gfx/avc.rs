//! H.264 the way the graphics pipeline carries it: AVC420 and AVC444
//! (MS-RDPEGFX 2.2.4.4, 2.2.4.5, 3.3.8.3).
//!
//! AVC420 is plain 4:2:0: every 2×2 block shares one colour, which smears
//! coloured text and ClearType edges. AVC444 sends two 4:2:0 pictures
//! through the same H.264 stream: the main view (luma plus averaged chroma,
//! an AVC420 picture on its own) and the auxiliary view with the chroma
//! samples the main view left out. Put back together they make full 4:4:4.
//! A server may send them together or one at a time (luma first while
//! something moves, the chroma once it stands still), so each surface keeps
//! the 4:4:4 picture it has built so far.
//!
//! The reconstruction follows FreeRDP's (`prim_YUV.c`, the inverse of
//! `general_YUV444SplitToYUV420`), but maps every destination sample to its
//! source directly, so a region that does not start on a 16-pixel boundary
//! is put together correctly too. Colour is BT.709 at full range, as Windows
//! encodes it.

use super::codecs::CodecResult;
use super::h264::{H264Decoder, Library, Yuv420};
use super::surface::{from_edges, intersect, Pixels};
use crate::dirty::Rect;
use ironrdp_core::{Decode as _, ReadCursor};
use ironrdp_egfx::pdu::{Avc420BitmapStream, Avc444BitmapStream, Encoding};
use std::collections::HashMap;

/// The chroma reconstruction only replaces the main view's averaged sample
/// when the difference is this large; below it, H.264 noise in the other
/// three samples (multiplied by the reconstruction) would do more harm than
/// the average. FreeRDP's threshold.
const FILTER_THRESHOLD: u8 = 30;

/// Which of the two pictures a stream holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// Luma and averaged chroma: AVC420, or AVC444's main view.
    Main,
    /// AVC444's auxiliary view, v1 layout (3.3.8.3.2).
    AuxV1,
    /// AVC444v2's auxiliary view (3.3.8.3.3).
    AuxV2,
}

pub(crate) struct Avc {
    library: Library,
    streams: HashMap<u16, Stream>,
}

/// One surface's H.264 stream and the picture it builds.
struct Stream {
    decoder: H264Decoder,
    picture: Yuv444,
}

impl Avc {
    pub fn new(library: Library) -> Self {
        Self {
            library,
            streams: HashMap::new(),
        }
    }

    pub fn delete_surface(&mut self, surface_id: u16) {
        self.streams.remove(&surface_id);
    }

    /// An `RFX_AVC420_BITMAP_STREAM`: the picture covers `dest`, but only
    /// the region rectangles (surface coordinates, exclusive edges) hold new
    /// pixels.
    pub fn avc420(
        &mut self,
        surface_id: u16,
        data: &[u8],
        dest: Rect,
        surface: &mut Pixels,
    ) -> CodecResult<Vec<Rect>> {
        let stream = Avc420BitmapStream::decode(&mut ReadCursor::new(data))
            .map_err(|e| format!("AVC420: {e}"))?;
        let state = self.stream(surface_id)?;
        let regions = state.decode(&stream, View::Main, dest)?;
        Ok(state.picture.draw(&regions, surface))
    }

    /// An `RFX_AVC444_BITMAP_STREAM` (v1 or v2): luma, chroma or both.
    pub fn avc444(
        &mut self,
        surface_id: u16,
        data: &[u8],
        dest: Rect,
        v2: bool,
        surface: &mut Pixels,
    ) -> CodecResult<Vec<Rect>> {
        let stream = Avc444BitmapStream::decode(&mut ReadCursor::new(data))
            .map_err(|e| format!("AVC444: {e}"))?;
        let aux = if v2 { View::AuxV2 } else { View::AuxV1 };
        let state = self.stream(surface_id)?;
        let mut regions = Vec::new();
        if stream.encoding == Encoding::CHROMA {
            regions.extend(state.decode(&stream.stream1, aux, dest)?);
        } else {
            regions.extend(state.decode(&stream.stream1, View::Main, dest)?);
            if let Some(chroma) = &stream.stream2 {
                regions.extend(state.decode(chroma, aux, dest)?);
            }
        }
        Ok(state.picture.draw(&regions, surface))
    }

    fn stream(&mut self, surface_id: u16) -> CodecResult<&mut Stream> {
        if !self.streams.contains_key(&surface_id) {
            let decoder = self.library.decoder().map_err(|e| format!("H.264: {e}"))?;
            self.streams.insert(
                surface_id,
                Stream {
                    decoder,
                    picture: Yuv444::default(),
                },
            );
        }
        self.streams
            .get_mut(&surface_id)
            .ok_or_else(|| "H.264: no stream".to_owned())
    }
}

impl Stream {
    /// Decodes one H.264 picture and folds it into the 4:4:4 picture inside
    /// its regions. Returns the regions in picture coordinates; none when
    /// the decoder had no picture yet.
    fn decode(
        &mut self,
        stream: &Avc420BitmapStream<'_>,
        view: View,
        dest: Rect,
    ) -> CodecResult<Vec<Rect>> {
        let picture = &mut self.picture;
        let regions = self
            .decoder
            .decode(stream.data, |yuv| {
                picture.ensure(yuv.width, yuv.height);
                let regions = regions(stream, dest, yuv.width, yuv.height);
                for region in &regions {
                    match view {
                        View::Main => picture.combine_main(yuv, *region),
                        View::AuxV1 | View::AuxV2 => picture.combine_aux(yuv, *region, view),
                    }
                }
                regions
            })
            .map_err(|e| format!("H.264: {e}"))?;
        Ok(regions.unwrap_or_default())
    }
}

/// The stream's region rectangles (exclusive edges) inside `dest` and the
/// picture. No rectangles means all of `dest`. Like FreeRDP, the picture
/// lies on the surface from its corner: the rectangles are in surface and
/// picture coordinates at once.
fn regions(stream: &Avc420BitmapStream<'_>, dest: Rect, width: usize, height: usize) -> Vec<Rect> {
    let mut rects: Vec<Rect> = stream
        .rectangles
        .iter()
        .map(|r| intersect(&from_edges(r.left, r.top, r.right, r.bottom), &dest))
        .collect();
    if stream.rectangles.is_empty() {
        rects.push(dest);
    }
    let w = u16::try_from(width).unwrap_or(u16::MAX);
    let h = u16::try_from(height).unwrap_or(u16::MAX);
    rects
        .into_iter()
        .map(|r| r.clip(w, h))
        .filter(|r| !r.is_empty())
        .collect()
}

/// The 4:4:4 picture of one surface, with the main view's chroma kept
/// apart: the auxiliary view's reconstruction needs it, and keeping it
/// means a second chroma update builds on the main view, not on itself.
#[derive(Default)]
struct Yuv444 {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// The main view's chroma, `chroma_width` × (`height` + 1) / 2.
    main_u: Vec<u8>,
    main_v: Vec<u8>,
    chroma_width: usize,
}

impl Yuv444 {
    /// Takes the decoder's picture size; a new size starts over with black.
    fn ensure(&mut self, width: usize, height: usize) {
        if (self.width, self.height) == (width, height) && !self.y.is_empty() {
            return;
        }
        self.width = width;
        self.height = height;
        self.chroma_width = width.div_ceil(2);
        let samples = width * height;
        self.y = vec![0; samples];
        self.u = vec![128; samples];
        self.v = vec![128; samples];
        let chroma = self.chroma_width * height.div_ceil(2);
        self.main_u = vec![128; chroma];
        self.main_v = vec![128; chroma];
    }

    /// The main view: luma as it is (B1), chroma kept and spread over each
    /// 2×2 block (B2, B3), which is what the picture is until the auxiliary
    /// view for it arrives.
    fn combine_main(&mut self, yuv: &Yuv420<'_>, r: Rect) {
        let (w, cw) = (self.width, self.chroma_width);
        let (x0, x1) = (usize::from(r.x), usize::from(r.x) + usize::from(r.w));
        for row in usize::from(r.y)..usize::from(r.y) + usize::from(r.h) {
            let Some(src) = yuv.y.get(row * yuv.y_stride + x0..row * yuv.y_stride + x1) else {
                continue;
            };
            self.y[row * w + x0..row * w + x1].copy_from_slice(src);
        }
        // Chroma for whole 2×2 blocks, so a region on odd edges still gets
        // its blocks consistent.
        let (bx0, bx1) = (x0 / 2, x1.div_ceil(2));
        let by0 = usize::from(r.y) / 2;
        let by1 = (usize::from(r.y) + usize::from(r.h)).div_ceil(2);
        for by in by0..by1 {
            let src = by * yuv.uv_stride;
            let (Some(su), Some(sv)) = (
                yuv.u.get(src + bx0..src + bx1),
                yuv.v.get(src + bx0..src + bx1),
            ) else {
                continue;
            };
            self.main_u[by * cw + bx0..by * cw + bx1].copy_from_slice(su);
            self.main_v[by * cw + bx0..by * cw + bx1].copy_from_slice(sv);
            for row in [2 * by, 2 * by + 1] {
                if row >= self.height {
                    continue;
                }
                for (i, (&cu, &cv)) in su.iter().zip(sv).enumerate() {
                    let x = 2 * (bx0 + i);
                    for col in [x, x + 1] {
                        if col < w {
                            self.u[row * w + col] = cu;
                            self.v[row * w + col] = cv;
                        }
                    }
                }
            }
        }
    }

    /// The auxiliary view: the three chroma samples of every 2×2 block the
    /// main view averaged away, then the fourth worked out from the average.
    fn combine_aux(&mut self, yuv: &Yuv420<'_>, r: Rect, view: View) {
        // Whole 2×2 blocks.
        let x0 = usize::from(r.x) & !1;
        let y0 = usize::from(r.y) & !1;
        let x1 = (usize::from(r.x) + usize::from(r.w)).next_multiple_of(2);
        let y1 = (usize::from(r.y) + usize::from(r.h)).next_multiple_of(2);
        let w = self.width;
        // Odd rows whose samples the view did not have (v1 on a height
        // that is not a multiple of 16): their blocks keep the average.
        let mut missing = Vec::new();
        for row in y0..y1.min(self.height) {
            let ok = match view {
                View::AuxV1 => self.aux_v1_row(yuv, row, x0, x1.min(w)),
                _ => self.aux_v2_row(yuv, row, x0, x1.min(w)),
            };
            if !ok {
                missing.push(row / 2);
            }
        }
        self.reconstruct(x0 / 2, x1 / 2, y0 / 2, y1 / 2, &missing);
    }

    /// One row of a v1 auxiliary view: odd rows come whole from the aux
    /// luma plane, eight U rows then eight V rows in every sixteen (B4, B5);
    /// even rows get their odd columns from the aux chroma planes (B6, B7).
    fn aux_v1_row(&mut self, yuv: &Yuv420<'_>, row: usize, x0: usize, x1: usize) -> bool {
        let w = self.width;
        if row % 2 == 1 {
            let k = row / 2;
            let u_row = (k & !7) + k;
            let mut complete = true;
            for (source_row, plane, main) in [
                (u_row, &mut self.u, &self.main_u),
                (u_row + 8, &mut self.v, &self.main_v),
            ] {
                let target = &mut plane[row * w + x0..row * w + x1];
                let start = source_row * yuv.y_stride;
                match yuv.y.get(start + x0..start + x1) {
                    Some(source) if source_row < yuv.height => target.copy_from_slice(source),
                    // Past the picture: the main view's average it is.
                    _ => {
                        complete = false;
                        let averages = &main[k * self.chroma_width..];
                        for (i, sample) in target.iter_mut().enumerate() {
                            *sample = averages[(x0 + i) / 2];
                        }
                    }
                }
            }
            return complete;
        }
        let src = (row / 2) * yuv.uv_stride;
        for x in (x0 + 1..x1).step_by(2) {
            let (Some(&cu), Some(&cv)) = (yuv.u.get(src + x / 2), yuv.v.get(src + x / 2)) else {
                return false;
            };
            self.u[row * w + x] = cu;
            self.v[row * w + x] = cv;
        }
        true
    }

    /// One row of a v2 auxiliary view: the odd columns of U and V sit side
    /// by side in the aux luma plane (B4, B5); odd rows get columns 4n from
    /// the aux U plane and 4n+2 from the aux V plane, U left and V right
    /// (B6–B9).
    fn aux_v2_row(&mut self, yuv: &Yuv420<'_>, row: usize, x0: usize, x1: usize) -> bool {
        let w = self.width;
        let half = yuv.width / 2;
        let quarter = yuv.width / 4;
        let luma = row * yuv.y_stride;
        for x in (x0 + 1..x1).step_by(2) {
            let (Some(&cu), Some(&cv)) = (yuv.y.get(luma + x / 2), yuv.y.get(luma + half + x / 2))
            else {
                return false;
            };
            self.u[row * w + x] = cu;
            self.v[row * w + x] = cv;
        }
        if row % 2 == 1 {
            let src = (row / 2) * yuv.uv_stride;
            for x in (x0..x1).step_by(2) {
                let plane = if x % 4 == 0 { yuv.u } else { yuv.v };
                let (Some(&cu), Some(&cv)) =
                    (plane.get(src + x / 4), plane.get(src + quarter + x / 4))
                else {
                    return false;
                };
                self.u[row * w + x] = cu;
                self.v[row * w + x] = cv;
            }
        }
        true
    }

    /// The top-left sample of every 2×2 block in the given block range: the
    /// main view holds the block's average, so it is four times that minus
    /// the other three (FreeRDP's `general_ChromaFilter`).
    fn reconstruct(&mut self, bx0: usize, bx1: usize, by0: usize, by1: usize, missing: &[usize]) {
        let (w, cw) = (self.width, self.chroma_width);
        for by in by0..by1 {
            let (top, bottom) = (2 * by, 2 * by + 1);
            if bottom >= self.height {
                continue;
            }
            let lacking = missing.contains(&by);
            for bx in bx0..bx1 {
                let (left, right) = (2 * bx, 2 * bx + 1);
                if right >= w {
                    continue;
                }
                for (plane, main) in [(&mut self.u, &self.main_u), (&mut self.v, &self.main_v)] {
                    let average = main[by * cw + bx];
                    plane[top * w + left] = if lacking {
                        average
                    } else {
                        let others = i32::from(plane[top * w + right])
                            + i32::from(plane[bottom * w + left])
                            + i32::from(plane[bottom * w + right]);
                        conditional_clip(4 * i32::from(average) - others, average)
                    };
                }
            }
        }
    }

    /// Converts `regions` to RGBA on the surface (the picture lies on the
    /// surface from its corner). Returns what was written.
    fn draw(&self, regions: &[Rect], surface: &mut Pixels) -> Vec<Rect> {
        let mut written = Vec::new();
        let stride = surface.stride();
        let (w, h) = (
            u16::try_from(self.width).unwrap_or(u16::MAX),
            u16::try_from(self.height).unwrap_or(u16::MAX),
        );
        for r in regions {
            // A size change between the two views voids the first's regions.
            let target = r.clip(w, h).clip(surface.width, surface.height);
            if target.is_empty() {
                continue;
            }
            let cols = usize::from(target.w);
            for i in 0..usize::from(target.h) {
                let row = usize::from(target.y) + i;
                let from = row * self.width + usize::from(target.x);
                let to = row * stride + usize::from(target.x) * 4;
                let out = &mut surface.data[to..to + cols * 4];
                let samples = self.y[from..from + cols]
                    .iter()
                    .zip(&self.u[from..from + cols])
                    .zip(&self.v[from..from + cols]);
                for (rgba, ((&y, &u), &v)) in out.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
                    *rgba = yuv_to_rgba(y, u, v);
                }
            }
            written.push(target);
        }
        written
    }
}

fn conditional_clip(value: i32, original: u8) -> u8 {
    let clipped = clip(value);
    if clipped.abs_diff(original) < FILTER_THRESHOLD {
        original
    } else {
        clipped
    }
}

fn clip(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

/// BT.709 at full range, in 16.16 fixed point.
fn yuv_to_rgba(y: u8, u: u8, v: u8) -> [u8; 4] {
    let y = i32::from(y) << 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let round = 1 << 15;
    let r = (y + 103_206 * e + round) >> 16;
    let g = (y - 12_276 * d - 30_679 * e + round) >> 16;
    let b = (y + 121_609 * d + round) >> 16;
    [clip(r), clip(g), clip(b), 0xFF]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::avc444_split as split;

    /// A picture's planes as the decoder would hand them over, from I420.
    fn planes(i420: &[u8], width: usize, height: usize) -> Yuv420<'_> {
        let luma = width * height;
        let chroma = luma / 4;
        Yuv420 {
            width,
            height,
            y: &i420[..luma],
            u: &i420[luma..luma + chroma],
            v: &i420[luma + chroma..],
            y_stride: width,
            uv_stride: width / 2,
        }
    }

    /// Sharp coloured stripes and dots: what 4:2:0 smears.
    fn pattern(width: usize, height: usize) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(match (x % 3, (x + y) % 5) {
                    (0, _) => &[220, 30, 30],
                    (1, 0) => &[20, 40, 230],
                    (1, _) => &[255, 255, 255],
                    _ => &[30, 200, 60],
                });
            }
        }
        rgb
    }

    const W: usize = 48;
    const H: usize = 32;

    /// Splits the pattern, puts it back together inside `region` (no H.264
    /// in between) and returns the drawn picture.
    fn round_trip(view: View, region: Rect) -> Pixels {
        let source = split::Yuv444::from_rgb(&pattern(W, H), W, H);
        let main = source.main_view();
        let aux = match view {
            View::AuxV2 => source.aux_view_v2(),
            _ => source.aux_view_v1(),
        };
        let mut picture = Yuv444::default();
        picture.ensure(W, H);
        picture.combine_main(&planes(&main, W, H), region);
        if view != View::Main {
            picture.combine_aux(&planes(&aux, W, H), region, view);
        }
        let mut surface = Pixels::new(48, 32);
        picture.draw(&[region], &mut surface);
        surface
    }

    /// The average and the largest difference of any channel from the
    /// pattern.
    fn error(surface: &Pixels) -> (f64, u8) {
        let rgb = pattern(W, H);
        let mut sum = 0u32;
        let mut worst = 0;
        for (px, expected) in surface.data.chunks(4).zip(rgb.chunks(3)) {
            for c in 0..3 {
                let d = px[c].abs_diff(expected[c]);
                sum += u32::from(d);
                worst = worst.max(d);
            }
        }
        (f64::from(sum) / rgb.len() as f64, worst)
    }

    #[test]
    fn both_aux_layouts_give_back_full_chroma() {
        let whole = Rect::new(0, 0, 48, 32);
        let (smeared, _) = error(&round_trip(View::Main, whole));
        for view in [View::AuxV1, View::AuxV2] {
            let full = round_trip(view, whole);
            let (average, worst) = error(&full);
            // Three of four chroma samples come back exactly; the fourth
            // keeps the block's average when it is within FreeRDP's
            // threshold of it.
            assert!(
                average * 4.0 < smeared && worst < 60,
                "{view:?}: average {average:.2} (4:2:0 {smeared:.2}), worst {worst}"
            );
            // A region off the 16-pixel grid comes out the same as that
            // part of the whole picture.
            let region = Rect::new(6, 10, 20, 14);
            let part = round_trip(view, region);
            assert_eq!(part.read(region), full.read(region), "{view:?}");
        }
    }

    #[test]
    fn a_second_chroma_update_builds_on_the_main_view() {
        let (w, h) = (16, 16);
        let rgb = pattern(w, h);
        let source = split::Yuv444::from_rgb(&rgb, w, h);
        let (main, aux) = (source.main_view(), source.aux_view_v1());
        let region = Rect::new(0, 0, 16, 16);
        let mut picture = Yuv444::default();
        picture.ensure(w, h);
        picture.combine_main(&planes(&main, w, h), region);
        picture.combine_aux(&planes(&aux, w, h), region, View::AuxV1);
        let once = picture.u.clone();
        picture.combine_aux(&planes(&aux, w, h), region, View::AuxV1);
        assert_eq!(picture.u, once);
    }

    #[test]
    fn v1_rows_past_the_picture_keep_the_average() {
        // 20 rows: the V rows of the last block would sit past row 20.
        let (w, h) = (16, 20);
        let rgb = pattern(w, h);
        let source = split::Yuv444::from_rgb(&rgb, w, h);
        let mut picture = Yuv444::default();
        picture.ensure(w, h);
        let region = Rect::new(0, 0, 16, 20);
        picture.combine_main(&planes(&source.main_view(), w, h), region);
        picture.combine_aux(&planes(&source.aux_view_v1(), w, h), region, View::AuxV1);
        // Row 18 belongs to a block whose odd row had no aux samples.
        assert_eq!(picture.u[18 * w], picture.main_u[9 * picture.chroma_width]);
    }

    #[test]
    fn bt709_full_range() {
        assert_eq!(yuv_to_rgba(0, 128, 128), [0, 0, 0, 255]);
        assert_eq!(yuv_to_rgba(255, 128, 128), [255, 255, 255, 255]);
        // Pure red: Y 54, U 99, V 255 (BT.709 full range).
        let red = yuv_to_rgba(54, 99, 255);
        assert!(red[0] >= 252 && red[1] <= 3 && red[2] <= 3, "{red:?}");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut avc = Avc::new(Library::source());
        let mut surface = Pixels::new(16, 16);
        let dest = surface.bounds();
        assert!(avc.avc420(1, &[1, 2], dest, &mut surface).is_err());
        assert!(avc
            .avc444(1, &[0xFF; 3], dest, false, &mut surface)
            .is_err());
    }
}
