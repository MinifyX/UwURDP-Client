//! The codecs a graphics pipeline server may use, each decoding straight onto
//! a surface.
//!
//! The heavy lifting is IronRDP's (`ironrdp-graphics`); this module adapts
//! each decoder's output to our RGBA surfaces and keeps every write inside
//! the area the server said it updates. That matters for the lossy codecs:
//! a RemoteFX or H.264 tile reaching past its region would otherwise smear
//! lossy pixels over text another codec drew losslessly.

use super::surface::{from_edges, intersect, Pixels};
use crate::dirty::Rect;
use ironrdp_core::{Decode as _, ReadCursor};
use ironrdp_graphics::color_conversion::{self, YCbCrBuffer};
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_graphics::{dwt, quantization, rlgr, subband_reconstruction};
use ironrdp_graphics_next::clearcodec::ClearCodecDecoder;
use ironrdp_graphics_next::progressive::ProgressiveDecoder;
use ironrdp_pdu::codecs::rfx::{self, Quant, RfxRectangle};

/// RemoteFX and progressive tiles are 64×64.
const TILE: u16 = 64;
const TILE_PIXELS: usize = 64 * 64;
const TILE_STRIDE: usize = 64 * 4;

pub(crate) type CodecResult<T> = Result<T, String>;

#[derive(Default)]
pub(crate) struct Codecs {
    planar: BitmapStreamDecoder,
    planar_rgb: Vec<u8>,
    clear: ClearCodecDecoder,
    progressive: ProgressiveDecoder,
    remotefx: RemoteFx,
    #[cfg(feature = "h264")]
    pub h264: Option<super::h264::H264Decoder>,
}

impl Codecs {
    /// Uncompressed 32-bit pixels, blue first (`XRGB`/`ARGB` little-endian).
    pub fn uncompressed(&self, data: &[u8], dest: Rect, surface: &mut Pixels) -> CodecResult<Rect> {
        let needed = dest.area() as usize * 4;
        if data.len() < needed {
            return Err(format!(
                "uncompressed: {} bytes for {}x{}",
                data.len(),
                dest.w,
                dest.h
            ));
        }
        let rgba = bgra_to_rgba(&data[..needed]);
        Ok(surface.write(
            dest.x,
            dest.y,
            dest.w,
            dest.h,
            &rgba,
            usize::from(dest.w) * 4,
        ))
    }

    /// RDP 6.0 bitmap compression ("planar", MS-RDPEGDI 2.2.2.5.1), top-down.
    pub fn planar(&mut self, data: &[u8], dest: Rect, surface: &mut Pixels) -> CodecResult<Rect> {
        let (w, h) = (usize::from(dest.w), usize::from(dest.h));
        self.planar_rgb.clear();
        self.planar
            .decode_bitmap_stream_to_rgb24(data, &mut self.planar_rgb, w, h)
            .map_err(|e| format!("planar: {e}"))?;
        if self.planar_rgb.len() < w * h * 3 {
            return Err("planar: short output".into());
        }
        // IronRDP writes AYCoCg without an alpha plane blue first, every
        // other variant red first (checked in the tests below).
        let blue_first = data.first().is_some_and(|&header| {
            let color_loss_level = header & 0x07;
            let no_alpha = header & 0x20 != 0;
            color_loss_level != 0 && no_alpha
        });
        let mut rgba = Vec::with_capacity(w * h * 4);
        for px in self.planar_rgb[..w * h * 3].as_chunks::<3>().0 {
            if blue_first {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
            } else {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        }
        Ok(surface.write(dest.x, dest.y, dest.w, dest.h, &rgba, w * 4))
    }

    /// ClearCodec (MS-RDPEGFX 2.2.4.1), lossless; used for text and UI.
    pub fn clear(&mut self, data: &[u8], dest: Rect, surface: &mut Pixels) -> CodecResult<Rect> {
        let bgra = self
            .clear
            .decode(data, dest.w, dest.h)
            .map_err(|e| format!("ClearCodec: {e}"))?;
        let rgba = bgra_to_rgba(&bgra);
        Ok(surface.write(
            dest.x,
            dest.y,
            dest.w,
            dest.h,
            &rgba,
            usize::from(dest.w) * 4,
        ))
    }

    /// RemoteFX Progressive (`WireToSurface2`). Tiles land at their place on
    /// the surface, clipped to the region the server updates. Its state lives
    /// per surface and codec context, across frames.
    pub fn progressive(
        &mut self,
        surface_id: u16,
        context: u32,
        data: &[u8],
        surface: &mut Pixels,
    ) -> CodecResult<Vec<Rect>> {
        let tiles = self
            .progressive
            .decode_bitmap(surface_id, context, surface.width, surface.height, data)
            .map_err(|e| format!("progressive: {e}"))?;
        let mut written = Vec::new();
        for tile in tiles {
            let (Some(x), Some(y)) = (tile.x_idx.checked_mul(TILE), tile.y_idx.checked_mul(TILE))
            else {
                continue;
            };
            let clip: Vec<Rect> = tile
                .update_rectangles
                .iter()
                .map(|r| from_edges(r.left, r.top, r.right, r.bottom))
                .collect();
            written.extend(surface.write_clipped(
                x,
                y,
                TILE,
                TILE,
                &tile.pixels,
                TILE_STRIDE,
                &clip,
            ));
        }
        Ok(written)
    }

    /// Progressive REGION blocks of one graphics frame may build on each
    /// other, so the decoder has to know where frames begin and end.
    pub fn start_frame(&mut self) {
        self.progressive.begin_frame();
    }

    pub fn end_frame(&mut self) {
        self.progressive.end_frame();
    }

    pub fn delete_progressive_context(&mut self, surface_id: u16, context: u32) {
        self.progressive.delete_context(surface_id, context);
    }

    pub fn delete_surface(&mut self, surface_id: u16) {
        self.progressive.delete_surface(surface_id);
    }

    /// RemoteFX (`CAVIDEO`, MS-RDPRFX messages): tiles and region rectangles
    /// are relative to `dest`.
    pub fn remotefx(
        &mut self,
        data: &[u8],
        dest: Rect,
        surface: &mut Pixels,
    ) -> CodecResult<Vec<Rect>> {
        self.remotefx.decode(data, dest, surface)
    }

    /// H.264 in an `RFX_AVC420_BITMAP_STREAM`: the picture covers `dest`,
    /// but only the region rectangles (surface coordinates, exclusive
    /// edges) hold new pixels.
    #[cfg(feature = "h264")]
    pub fn avc420(
        &mut self,
        data: &[u8],
        dest: Rect,
        surface: &mut Pixels,
    ) -> CodecResult<Vec<Rect>> {
        use ironrdp_egfx::pdu::Avc420BitmapStream;

        let stream = Avc420BitmapStream::decode(&mut ReadCursor::new(data))
            .map_err(|e| format!("AVC420: {e}"))?;
        let Some(decoder) = self.h264.as_mut() else {
            return Err("AVC420 without an H.264 decoder".into());
        };
        let Some(picture) = decoder
            .decode(stream.data)
            .map_err(|e| format!("H.264: {e}"))?
        else {
            return Ok(Vec::new());
        };
        let mut clip: Vec<Rect> = stream
            .rectangles
            .iter()
            .map(|r| intersect(&from_edges(r.left, r.top, r.right, r.bottom), &dest))
            .filter(|r| !r.is_empty())
            .collect();
        if stream.rectangles.is_empty() {
            clip.push(dest);
        }
        Ok(surface.write_clipped(
            dest.x,
            dest.y,
            dest.w.min(picture.width),
            dest.h.min(picture.height),
            picture.rgba,
            usize::from(picture.width) * 4,
            &clip,
        ))
    }
}

/// Scratch buffers for decoding one RemoteFX tile.
struct RemoteFx {
    coefficients: [Vec<i16>; 3],
    temp: Vec<i16>,
    rgba: Vec<u8>,
}

impl Default for RemoteFx {
    fn default() -> Self {
        Self {
            coefficients: [
                vec![0; TILE_PIXELS],
                vec![0; TILE_PIXELS],
                vec![0; TILE_PIXELS],
            ],
            temp: vec![0; TILE_PIXELS],
            rgba: vec![0; TILE_PIXELS * 4],
        }
    }
}

impl RemoteFx {
    fn decode(&mut self, data: &[u8], dest: Rect, surface: &mut Pixels) -> CodecResult<Vec<Rect>> {
        let mut cursor = ReadCursor::new(data);
        let mut region: Vec<RfxRectangle> = Vec::new();
        let mut written = Vec::new();
        while !cursor.is_empty() {
            let block = rfx::Block::decode(&mut cursor).map_err(|e| format!("RemoteFX: {e}"))?;
            match block {
                rfx::Block::CodecChannel(rfx::CodecChannel::Region(r)) => region = r.rectangles,
                rfx::Block::CodecChannel(rfx::CodecChannel::TileSet(set)) => {
                    let mut clip: Vec<Rect> = region
                        .iter()
                        .map(|r| {
                            let r = rfx_rect(r);
                            let moved = Rect::new(
                                dest.x.saturating_add(r.x),
                                dest.y.saturating_add(r.y),
                                r.w,
                                r.h,
                            );
                            intersect(&moved, &dest)
                        })
                        .filter(|r| !r.is_empty())
                        .collect();
                    if region.is_empty() {
                        clip.push(dest);
                    }
                    for tile in &set.tiles {
                        let quant = |index: u8| set.quants.get(usize::from(index));
                        let (Some(qy), Some(qcb), Some(qcr)) = (
                            quant(tile.y_quant_index),
                            quant(tile.cb_quant_index),
                            quant(tile.cr_quant_index),
                        ) else {
                            return Err("RemoteFX: tile names a missing quantizer".into());
                        };
                        self.decode_tile(
                            set.entropy_algorithm,
                            [(tile.y_data, qy), (tile.cb_data, qcb), (tile.cr_data, qcr)],
                        )?;
                        let (Some(x), Some(y)) = (
                            tile.x.checked_mul(TILE).and_then(|x| x.checked_add(dest.x)),
                            tile.y.checked_mul(TILE).and_then(|y| y.checked_add(dest.y)),
                        ) else {
                            continue;
                        };
                        written.extend(surface.write_clipped(
                            x,
                            y,
                            TILE,
                            TILE,
                            &self.rgba,
                            TILE_STRIDE,
                            &clip,
                        ));
                    }
                }
                // Sync, codec versions, channels, context and frame markers
                // carry nothing a tile needs beyond what the tile set holds.
                _ => {}
            }
        }
        Ok(written)
    }

    fn decode_tile(
        &mut self,
        entropy: rfx::EntropyAlgorithm,
        components: [(&[u8], &Quant); 3],
    ) -> CodecResult<()> {
        for ((data, quant), coefficients) in
            components.into_iter().zip(self.coefficients.iter_mut())
        {
            rlgr::decode(entropy, data, coefficients).map_err(|e| format!("RemoteFX: {e}"))?;
            subband_reconstruction::decode(&mut coefficients[4032..]);
            quantization::decode(coefficients, quant);
            dwt::decode(coefficients, &mut self.temp);
        }
        color_conversion::ycbcr_to_rgba(
            YCbCrBuffer {
                y: &self.coefficients[0],
                cb: &self.coefficients[1],
                cr: &self.coefficients[2],
            },
            &mut self.rgba,
        )
        .map_err(|e| format!("RemoteFX: {e}"))
    }
}

fn rfx_rect(r: &RfxRectangle) -> Rect {
    Rect::new(r.x, r.y, r.width, r.height)
}

/// Blue-first 32-bit pixels to opaque RGBA.
fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &Pixels, x: usize, y: usize) -> [u8; 4] {
        let i = (y * usize::from(p.width) + x) * 4;
        p.data[i..i + 4].try_into().expect("4 bytes")
    }

    /// A Progressive stream as Windows sends it: a first-pass tile at full
    /// quality (0xFF) with no progressive quant table, SYNC + CONTEXT only in
    /// the context's first frame.
    fn windows_progressive(with_context: bool, x_idx: u16, y_data: &[u8]) -> Vec<u8> {
        use ironrdp_pdu_next::codecs::rfx::progressive::*;
        use ironrdp_pdu_next::codecs::rfx::RfxRectangle;
        let mut blocks = vec![ProgressiveBlock::Sync(ProgressiveSyncPdu)];
        if with_context {
            blocks.push(ProgressiveBlock::Context(ProgressiveContextPdu {
                context_id: 0,
                tile_size: 0x40,
                flags: 0,
            }));
        }
        blocks.extend([
            ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
                frame_index: 0,
                region_count: 1,
            }),
            ProgressiveBlock::Region(ProgressiveRegion {
                tile_size: 0x40,
                rects: vec![RfxRectangle {
                    x: x_idx * 64,
                    y: 0,
                    width: 64,
                    height: 64,
                }],
                quant_vals: vec![ComponentCodecQuant::LOSSLESS],
                quant_prog_vals: vec![],
                flags: 0,
                tiles: vec![ProgressiveTile::First(TileFirst {
                    quant_idx_y: 0,
                    quant_idx_cb: 0,
                    quant_idx_cr: 0,
                    x_idx,
                    y_idx: 0,
                    flags: 0,
                    quality: 0xFF,
                    y_data,
                    cb_data: y_data,
                    cr_data: y_data,
                    tail_data: &[],
                })],
            }),
            ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
        ]);
        encode_progressive_stream(&blocks).expect("progressive stream")
    }

    /// One component of a flat tile, RLGR1-coded.
    fn flat_component(value: i16) -> Vec<u8> {
        use ironrdp_graphics_next::progressive::{encode_first_pass, COEFFICIENTS_PER_COMPONENT};
        use ironrdp_pdu_next::codecs::rfx::progressive::ComponentCodecQuant;
        let mut coefficients = [value; COEFFICIENTS_PER_COMPONENT];
        let mut out = vec![0; 8192];
        let len = encode_first_pass(
            &mut coefficients,
            &mut out,
            &ComponentCodecQuant::LOSSLESS,
            &ComponentCodecQuant::LOSSLESS,
            false,
        )
        .expect("encode");
        out.truncate(len);
        out
    }

    #[test]
    fn progressive_decodes_windows_streams() {
        let mut codecs = Codecs::default();
        let mut surface = Pixels::new(192, 64);
        let red = [255, 0, 0, 255];
        surface.fill(surface.bounds(), red);
        let component = flat_component(0);

        // The first frame establishes the context...
        codecs.start_frame();
        let written = codecs
            .progressive(
                1,
                7,
                &windows_progressive(true, 0, &component),
                &mut surface,
            )
            .expect("first frame");
        codecs.end_frame();
        assert_eq!(written, vec![Rect::new(0, 0, 64, 64)]);
        assert_ne!(at(&surface, 10, 10), red);

        // ...and the next one relies on it, as Windows does.
        codecs.start_frame();
        let written = codecs
            .progressive(
                1,
                7,
                &windows_progressive(false, 1, &component),
                &mut surface,
            )
            .expect("frame without CONTEXT");
        codecs.end_frame();
        assert_eq!(written, vec![Rect::new(64, 0, 64, 64)]);
        assert_eq!(at(&surface, 100, 10), at(&surface, 10, 10));
        assert_eq!(at(&surface, 150, 10), red, "outside the region");
    }

    #[test]
    fn uncompressed_is_blue_first() {
        let codecs = Codecs::default();
        let mut surface = Pixels::new(4, 4);
        let data = [10, 20, 30, 0].repeat(4);
        let done = codecs
            .uncompressed(&data, Rect::new(1, 1, 2, 2), &mut surface)
            .expect("decode");
        assert_eq!(done, Rect::new(1, 1, 2, 2));
        assert_eq!(at(&surface, 1, 1), [30, 20, 10, 255]);
        assert_eq!(at(&surface, 0, 0), [0, 0, 0, 255]);
        assert!(codecs
            .uncompressed(&data[..8], Rect::new(0, 0, 2, 2), &mut surface)
            .is_err());
    }

    /// A raw (not RLE) planar bitmap: format header, planes, pad byte.
    fn raw_planar(header: u8, planes: &[&[u8]]) -> Vec<u8> {
        let mut data = vec![header];
        for plane in planes {
            data.extend_from_slice(plane);
        }
        data.push(0);
        data
    }

    #[test]
    fn planar_argb_without_alpha_is_red_first() {
        let mut codecs = Codecs::default();
        let mut surface = Pixels::new(2, 1);
        // NA set, no colour loss: R, G, B planes.
        let data = raw_planar(0x20, &[&[100, 1], &[50, 2], &[20, 3]]);
        codecs
            .planar(&data, Rect::new(0, 0, 2, 1), &mut surface)
            .expect("decode");
        assert_eq!(at(&surface, 0, 0), [100, 50, 20, 255]);
        assert_eq!(at(&surface, 1, 0), [1, 2, 3, 255]);
    }

    #[test]
    fn planar_aycocg_without_alpha_comes_out_red_first() {
        let mut codecs = Codecs::default();
        let mut surface = Pixels::new(1, 1);
        // CLL 1, NA: Y 55, Co 40, Cg -5 is R 100, G 50, B 20.
        let data = raw_planar(0x21, &[&[55], &[40], &[0xFB]]);
        codecs
            .planar(&data, Rect::new(0, 0, 1, 1), &mut surface)
            .expect("decode");
        assert_eq!(at(&surface, 0, 0), [100, 50, 20, 255]);
    }

    #[test]
    fn planar_aycocg_with_alpha_comes_out_red_first() {
        let mut codecs = Codecs::default();
        let mut surface = Pixels::new(1, 1);
        // CLL 1 with an alpha plane first.
        let data = raw_planar(0x01, &[&[255], &[55], &[40], &[0xFB]]);
        codecs
            .planar(&data, Rect::new(0, 0, 1, 1), &mut surface)
            .expect("decode");
        assert_eq!(at(&surface, 0, 0), [100, 50, 20, 255]);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut codecs = Codecs::default();
        let mut surface = Pixels::new(64, 64);
        let dest = Rect::new(0, 0, 64, 64);
        let junk = [0xFFu8; 37];
        assert!(codecs.planar(&junk, dest, &mut surface).is_err());
        assert!(codecs.clear(&junk, dest, &mut surface).is_err());
        assert!(codecs.progressive(1, 1, &junk, &mut surface).is_err());
        assert!(codecs.remotefx(&junk, dest, &mut surface).is_err());
    }
}
