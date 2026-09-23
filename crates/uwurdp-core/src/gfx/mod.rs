//! The graphics pipeline (MS-RDPEGFX): how current Windows servers want to
//! send the desktop.
//!
//! Without it Windows 10/11 and Server 2016+ fall back to the old bitmap
//! path, which they serve slowly and tile by tile — the desktop builds up
//! from the top left like an old CRT. With it they pick a codec per region:
//! H.264 for moving pictures (when the user installed OpenH264, see
//! [`h264`]), RemoteFX Progressive for photos, ClearCodec for text, planar
//! for the rest, plus cheap commands for fills, scrolling and a bitmap cache.
//!
//! ```text
//!  DVC "Microsoft::Windows::RDS::Graphics"
//!     │ zgfx
//!     ▼
//!  GfxChannel ──▶ Pipeline: surfaces ──(mapped)──▶ output ──▶ session flush
//!                     ▲  cache, codecs                  │ dirty rects,
//!                     └──── FrameAcknowledge ◀── EndFrame   size changes
//! ```
//!
//! The channel runs inside IronRDP's `ActiveStage::process`, on the session
//! task, so the lock around [`Pipeline`] is never contended: the session
//! takes the changes out right after each PDU it processed. Updates inside a
//! frame reach the output only at its end, so the page never sees half a
//! frame.
//!
//! Nothing a server sends can take the session down: a PDU or codec payload
//! that does not decode is logged and skipped, sizes and memory are capped.

mod codecs;
#[cfg(feature = "h264")]
pub(crate) mod h264;
mod surface;

use crate::dirty::{DirtyRegion, Rect};
use codecs::Codecs;
use ironrdp_core::{decode, impl_as_any};
use ironrdp_dvc::{DvcClientProcessor, DvcMessage, DvcProcessor};
use ironrdp_egfx::pdu::{
    CapabilitiesAdvertisePdu, CapabilitiesV107Flags, CapabilitiesV81Flags, CapabilitiesV8Flags,
    CapabilitySet, Codec1Type, FrameAcknowledgePdu, GfxPdu, QueueDepth,
};
use ironrdp_graphics::zgfx;
use ironrdp_pdu::geometry::ExclusiveRectangle;
use ironrdp_pdu::PduResult;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use surface::{from_edges, Pixels};
use tracing::{debug, info, warn};

/// Largest surface or desktop edge we accept (what RDP allows for a desktop).
pub const MAX_EDGE: u16 = 8192;
/// All surfaces together; a 4K desktop plus a few offscreen surfaces fits
/// many times over.
const MAX_SURFACE_BYTES: usize = 512 * 1024 * 1024;
/// We advertise the small cache: slots 1..=4096, 16 MiB on the server's
/// books. We allow some slack before refusing entries.
const MAX_CACHE_SLOTS: u16 = 4096;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// How many decode failures are logged as warnings before they go quiet.
const LOUD_ERRORS: u32 = 5;

/// The pipeline, shared by the channel (which fills it) and the session
/// (which takes the changes out and draws from the output).
pub(crate) type Shared = Arc<Mutex<Pipeline>>;

/// Creates the channel and the handle the session reads from.
#[cfg(feature = "h264")]
pub(crate) fn channel(h264: Option<h264::H264Decoder>) -> (GfxChannel, Shared) {
    let mut pipeline = Pipeline::default();
    pipeline.codecs.h264 = h264;
    wrap(pipeline)
}

#[cfg(not(feature = "h264"))]
pub(crate) fn channel() -> (GfxChannel, Shared) {
    wrap(Pipeline::default())
}

fn wrap(pipeline: Pipeline) -> (GfxChannel, Shared) {
    let shared = Arc::new(Mutex::new(pipeline));
    (
        GfxChannel {
            pipeline: shared.clone(),
            decompressor: zgfx::Decompressor::new(),
            buffer: Vec::new(),
        },
        shared,
    )
}

/// What changed on the output since the session last looked.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// The desktop has this new size; everything before is void.
    pub resized: Option<(u16, u16)>,
    pub dirty: Vec<Rect>,
}

struct Surface {
    pixels: Pixels,
    /// Where on the output it shows, if it is mapped.
    origin: Option<(u32, u32)>,
}

/// Counters for the log, so a report tells which codecs a server used.
#[derive(Debug, Default)]
struct Stats {
    frames: u32,
    uncompressed: u32,
    planar: u32,
    clear: u32,
    progressive: u32,
    remotefx: u32,
    avc420: u32,
    other: u32,
    fills: u32,
    copies: u32,
    cache_hits: u32,
    errors: u32,
}

#[derive(Default)]
pub(crate) struct Pipeline {
    codecs: Codecs,
    surfaces: BTreeMap<u16, Surface>,
    surface_bytes: usize,
    cache: HashMap<u16, Pixels>,
    cache_bytes: usize,
    /// `None` until the server's first ResetGraphics: the pipeline is not
    /// drawing yet.
    output: Option<Pixels>,
    output_dirty: Vec<Rect>,
    resized: Option<(u16, u16)>,
    /// Surface areas updated in the frame that is still open.
    pending: BTreeMap<u16, DirtyRegion>,
    in_frame: bool,
    frames_decoded: u32,
    stats: Stats,
}

impl Pipeline {
    /// The picture to draw from, once the server started the pipeline.
    pub fn output(&self) -> Option<&Pixels> {
        self.output.as_ref()
    }

    pub fn take_changes(&mut self) -> Changes {
        Changes {
            resized: self.resized.take(),
            dirty: std::mem::take(&mut self.output_dirty),
        }
    }

    fn has_h264(&self) -> bool {
        #[cfg(feature = "h264")]
        return self.codecs.h264.is_some();
        #[cfg(not(feature = "h264"))]
        false
    }

    /// What we tell the server we can do. With H.264 that is AVC420 in
    /// version 8.1 (AVC444 would need a second decoder pass we do not
    /// have); without, 10.7 with AVC off, which gets us the same codecs
    /// Windows uses for everything else. The small cache keeps the server's
    /// bitmap cache at 16 MiB per session.
    fn capabilities(&self) -> Vec<CapabilitySet> {
        let v8 = CapabilitySet::V8 {
            flags: CapabilitiesV8Flags::SMALL_CACHE,
        };
        if self.has_h264() {
            vec![
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
                },
                v8,
            ]
        } else {
            vec![
                CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::AVC_DISABLED
                        | CapabilitiesV107Flags::SMALL_CACHE
                        | CapabilitiesV107Flags::SCALEDMAP_DISABLE,
                },
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::SMALL_CACHE,
                },
                v8,
            ]
        }
    }

    /// Handles one PDU; returns what to answer.
    fn handle(&mut self, pdu: GfxPdu) -> Option<GfxPdu> {
        match pdu {
            GfxPdu::CapabilitiesConfirm(confirm) => {
                info!(
                    version = format_args!("{:#x}", confirm.0.version.0),
                    h264 = self.has_h264(),
                    "graphics pipeline active"
                );
            }
            GfxPdu::ResetGraphics(reset) => {
                let width = clamp_edge(reset.width);
                let height = clamp_edge(reset.height);
                debug!(
                    width,
                    height,
                    monitors = reset.monitors.len(),
                    "reset graphics"
                );
                // Surfaces stay (the server may keep using them), but their
                // old contents and anything half-done are void.
                for surface in self.surfaces.values_mut() {
                    surface.pixels.clear();
                }
                self.pending.clear();
                self.output_dirty.clear();
                self.output = Some(Pixels::new(width, height));
                self.resized = Some((width, height));
            }
            GfxPdu::CreateSurface(create) => {
                self.create_surface(create.surface_id, create.width, create.height);
            }
            GfxPdu::DeleteSurface(delete) => {
                if let Some(old) = self.surfaces.remove(&delete.surface_id) {
                    self.surface_bytes -= old.pixels.data.len();
                }
                self.pending.remove(&delete.surface_id);
            }
            GfxPdu::MapSurfaceToOutput(map) => {
                self.map(map.surface_id, map.output_origin_x, map.output_origin_y);
            }
            GfxPdu::MapSurfaceToScaledOutput(map) => {
                // We ask for no scaled mapping; should it come anyway, show
                // the surface unscaled rather than not at all.
                debug!("scaled output mapping shown unscaled");
                self.map(map.surface_id, map.output_origin_x, map.output_origin_y);
            }
            GfxPdu::StartFrame(_) => self.in_frame = true,
            GfxPdu::EndFrame(end) => {
                self.in_frame = false;
                self.compose_pending();
                self.frames_decoded = self.frames_decoded.wrapping_add(1);
                self.stats.frames = self.stats.frames.wrapping_add(1);
                return Some(GfxPdu::FrameAcknowledge(FrameAcknowledgePdu {
                    queue_depth: QueueDepth::Unavailable,
                    frame_id: end.frame_id,
                    total_frames_decoded: self.frames_decoded,
                }));
            }
            GfxPdu::WireToSurface1(pdu) => {
                let Some(surface) = self.surfaces.get_mut(&pdu.surface_id) else {
                    self.error(format_args!(
                        "update for unknown surface {}",
                        pdu.surface_id
                    ));
                    return None;
                };
                let dest = exclusive(&pdu.destination_rectangle);
                let pixels = &mut surface.pixels;
                let result = match pdu.codec_id {
                    Codec1Type::Uncompressed => {
                        self.stats.uncompressed += 1;
                        self.codecs
                            .uncompressed(&pdu.bitmap_data, dest, pixels)
                            .map(|r| vec![r])
                    }
                    Codec1Type::Planar => {
                        self.stats.planar += 1;
                        self.codecs
                            .planar(&pdu.bitmap_data, dest, pixels)
                            .map(|r| vec![r])
                    }
                    Codec1Type::ClearCodec => {
                        self.stats.clear += 1;
                        self.codecs
                            .clear(&pdu.bitmap_data, dest, pixels)
                            .map(|r| vec![r])
                    }
                    Codec1Type::RemoteFx => {
                        self.stats.remotefx += 1;
                        self.codecs.remotefx(&pdu.bitmap_data, dest, pixels)
                    }
                    #[cfg(feature = "h264")]
                    Codec1Type::Avc420 => {
                        self.stats.avc420 += 1;
                        self.codecs.avc420(&pdu.bitmap_data, dest, pixels)
                    }
                    // Alpha only matters for surfaces shown with
                    // transparency, which a desktop never is.
                    Codec1Type::Alpha => Ok(Vec::new()),
                    other => {
                        self.stats.other += 1;
                        Err(format!("codec {other:?} was not offered"))
                    }
                };
                self.updated(pdu.surface_id, result);
            }
            GfxPdu::WireToSurface2(pdu) => {
                self.stats.progressive += 1;
                let Some(surface) = self.surfaces.get_mut(&pdu.surface_id) else {
                    self.error(format_args!(
                        "update for unknown surface {}",
                        pdu.surface_id
                    ));
                    return None;
                };
                let result = self.codecs.progressive(
                    pdu.codec_context_id,
                    &pdu.bitmap_data,
                    &mut surface.pixels,
                );
                self.updated(pdu.surface_id, result);
            }
            GfxPdu::DeleteEncodingContext(pdu) => {
                self.codecs.delete_progressive_context(pdu.codec_context_id);
            }
            GfxPdu::SolidFill(pdu) => {
                self.stats.fills += 1;
                let color = [pdu.fill_pixel.r, pdu.fill_pixel.g, pdu.fill_pixel.b, 0xFF];
                let surface = self.surfaces.get_mut(&pdu.surface_id)?;
                let filled: Vec<Rect> = pdu
                    .rectangles
                    .iter()
                    .map(|r| surface.pixels.fill(exclusive(r), color))
                    .collect();
                self.updated(pdu.surface_id, Ok(filled));
            }
            GfxPdu::SurfaceToSurface(pdu) => {
                self.stats.copies += 1;
                let source = exclusive(&pdu.source_rectangle);
                let (rect, block) = self
                    .surfaces
                    .get(&pdu.source_surface_id)
                    .map(|s| s.pixels.read(source))?;
                let target = self.surfaces.get_mut(&pdu.destination_surface_id)?;
                let written: Vec<Rect> = pdu
                    .destination_points
                    .iter()
                    .map(|p| {
                        target.pixels.write(
                            p.x,
                            p.y,
                            rect.w,
                            rect.h,
                            &block,
                            usize::from(rect.w) * 4,
                        )
                    })
                    .collect();
                self.updated(pdu.destination_surface_id, Ok(written));
            }
            GfxPdu::SurfaceToCache(pdu) => {
                let surface = self.surfaces.get(&pdu.surface_id)?;
                let (rect, data) = surface.pixels.read(exclusive(&pdu.source_rectangle));
                self.cache_put(
                    pdu.cache_slot,
                    Pixels {
                        width: rect.w,
                        height: rect.h,
                        data,
                    },
                );
            }
            GfxPdu::CacheToSurface(pdu) => {
                self.stats.cache_hits += 1;
                let (Some(entry), Some(surface)) = (
                    self.cache.get(&pdu.cache_slot),
                    self.surfaces.get_mut(&pdu.surface_id),
                ) else {
                    self.error(format_args!("cache slot {} is empty", pdu.cache_slot));
                    return None;
                };
                let written: Vec<Rect> = pdu
                    .destination_points
                    .iter()
                    .map(|p| {
                        surface.pixels.write(
                            p.x,
                            p.y,
                            entry.width,
                            entry.height,
                            &entry.data,
                            entry.stride(),
                        )
                    })
                    .collect();
                self.updated(pdu.surface_id, Ok(written));
            }
            GfxPdu::EvictCacheEntry(pdu) => {
                if let Some(old) = self.cache.remove(&pdu.cache_slot) {
                    self.cache_bytes -= old.data.len();
                }
            }
            // RAIL windows, QoE and cache import are nothing we offered.
            other => debug!(?other, "graphics PDU ignored"),
        }
        None
    }

    fn create_surface(&mut self, id: u16, width: u16, height: u16) {
        if let Some(old) = self.surfaces.remove(&id) {
            self.surface_bytes -= old.pixels.data.len();
        }
        let bytes = usize::from(width) * usize::from(height) * 4;
        if width == 0
            || height == 0
            || width > MAX_EDGE
            || height > MAX_EDGE
            || self.surface_bytes + bytes > MAX_SURFACE_BYTES
        {
            self.error(format_args!("surface {id} of {width}x{height} refused"));
            return;
        }
        self.surface_bytes += bytes;
        self.surfaces.insert(
            id,
            Surface {
                pixels: Pixels::new(width, height),
                origin: None,
            },
        );
    }

    fn map(&mut self, id: u16, x: u32, y: u32) {
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return;
        };
        surface.origin = Some((x, y));
        let whole = surface.pixels.bounds();
        self.touch(id, whole);
    }

    fn cache_put(&mut self, slot: u16, entry: Pixels) {
        if slot == 0 || slot > MAX_CACHE_SLOTS {
            self.error(format_args!("cache slot {slot} out of range"));
            return;
        }
        if let Some(old) = self.cache.remove(&slot) {
            self.cache_bytes -= old.data.len();
        }
        if self.cache_bytes + entry.data.len() > MAX_CACHE_BYTES {
            self.error(format_args!("bitmap cache full"));
            return;
        }
        self.cache_bytes += entry.data.len();
        self.cache.insert(slot, entry);
    }

    fn updated(&mut self, surface: u16, result: Result<Vec<Rect>, String>) {
        match result {
            Ok(rects) => {
                for rect in rects {
                    self.touch(surface, rect);
                }
            }
            Err(message) => self.error(format_args!("{message}")),
        }
    }

    fn error(&mut self, message: std::fmt::Arguments<'_>) {
        self.stats.errors += 1;
        if self.stats.errors <= LOUD_ERRORS {
            warn!("graphics pipeline: {message}");
        } else {
            debug!("graphics pipeline: {message}");
        }
    }

    /// A surface area changed: shown at the end of the frame, or now if the
    /// server sends it outside of one.
    fn touch(&mut self, surface: u16, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        if self.in_frame {
            self.pending.entry(surface).or_default().add(rect);
        } else {
            self.compose(surface, rect);
        }
    }

    fn compose_pending(&mut self) {
        for (surface, mut region) in std::mem::take(&mut self.pending) {
            for rect in region.take() {
                self.compose(surface, rect);
            }
        }
    }

    /// Copies a surface area to where it shows on the output.
    fn compose(&mut self, id: u16, rect: Rect) {
        let (Some(output), Some(surface)) = (self.output.as_mut(), self.surfaces.get(&id)) else {
            return;
        };
        let Some((ox, oy)) = surface.origin else {
            return;
        };
        let rect = rect.clip(surface.pixels.width, surface.pixels.height);
        let (Ok(x), Ok(y)) = (
            u16::try_from(ox + u32::from(rect.x)),
            u16::try_from(oy + u32::from(rect.y)),
        ) else {
            return;
        };
        let written = output.blit(&surface.pixels, rect, x, y);
        if !written.is_empty() {
            self.output_dirty.push(written);
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if self.stats.frames > 0 {
            info!(stats = ?self.stats, "graphics pipeline done");
        }
    }
}

fn clamp_edge(value: u32) -> u16 {
    u16::try_from(value.clamp(1, u32::from(MAX_EDGE))).unwrap_or(MAX_EDGE)
}

fn exclusive(rect: &ExclusiveRectangle) -> Rect {
    from_edges(rect.left, rect.top, rect.right, rect.bottom)
}

/// The dynamic channel IronRDP drives.
pub(crate) struct GfxChannel {
    pipeline: Shared,
    decompressor: zgfx::Decompressor,
    buffer: Vec<u8>,
}

impl_as_any!(GfxChannel);

impl DvcProcessor for GfxChannel {
    fn channel_name(&self) -> &str {
        ironrdp_egfx::CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let caps = self.pipeline.lock().capabilities();
        debug!(?caps, "advertising graphics capabilities");
        let pdu = GfxPdu::CapabilitiesAdvertise(CapabilitiesAdvertisePdu::from_typed(&caps));
        Ok(vec![Box::new(pdu)])
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        self.buffer.clear();
        let mut pipeline = self.pipeline.lock();
        if let Err(e) = self.decompressor.decompress(payload, &mut self.buffer) {
            pipeline.error(format_args!("zgfx: {e}"));
            return Ok(Vec::new());
        }
        let mut answers: Vec<DvcMessage> = Vec::new();
        for pdu in split_pdus(&self.buffer) {
            match decode::<GfxPdu>(pdu) {
                Ok(pdu) => {
                    if let Some(answer) = pipeline.handle(pdu) {
                        answers.push(Box::new(answer));
                    }
                }
                Err(e) => pipeline.error(format_args!("undecodable PDU: {e}")),
            }
        }
        Ok(answers)
    }
}

impl DvcClientProcessor for GfxChannel {}

/// The PDUs in a decompressed message, each with its header (u16 cmdId,
/// u16 flags, u32 length including the header). Decoding them one by one
/// means one bad PDU costs only itself.
fn split_pdus(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let length = data.get(4..8)?;
        let length = usize::try_from(u32::from_le_bytes(length.try_into().ok()?)).ok()?;
        if length < 8 || length > data.len() {
            warn!(length, left = data.len(), "graphics PDU with a bad length");
            return None;
        }
        let (pdu, rest) = data.split_at(length);
        data = rest;
        Some(pdu)
    })
}

#[cfg(test)]
mod tests;
