use super::*;
use ironrdp_core::encode_vec;
use ironrdp_egfx::pdu::{
    CacheToSurfacePdu, Color, CreateSurfacePdu, EndFramePdu, MapSurfaceToOutputPdu, PixelFormat,
    Point, ResetGraphicsPdu, SolidFillPdu, StartFramePdu, SurfaceToCachePdu, SurfaceToSurfacePdu,
    Timestamp, WireToSurface1Pdu,
};

fn rect(left: u16, top: u16, right: u16, bottom: u16) -> ExclusiveRectangle {
    ExclusiveRectangle {
        left,
        top,
        right,
        bottom,
    }
}

fn start(frame_id: u32) -> GfxPdu {
    GfxPdu::StartFrame(StartFramePdu {
        timestamp: Timestamp {
            milliseconds: 0,
            seconds: 0,
            minutes: 0,
            hours: 0,
        },
        frame_id,
    })
}

fn end(frame_id: u32) -> GfxPdu {
    GfxPdu::EndFrame(EndFramePdu { frame_id })
}

fn fill(surface_id: u16, rgb: [u8; 3], rects: Vec<ExclusiveRectangle>) -> GfxPdu {
    GfxPdu::SolidFill(SolidFillPdu {
        surface_id,
        fill_pixel: Color {
            b: rgb[2],
            g: rgb[1],
            r: rgb[0],
            xa: 0,
        },
        rectangles: rects,
    })
}

/// A pipeline with a `width`×`height` desktop and surface 1 mapped at 0,0.
fn desktop(width: u16, height: u16) -> Pipeline {
    let mut p = Pipeline::default();
    for pdu in [
        GfxPdu::ResetGraphics(ResetGraphicsPdu {
            width: u32::from(width),
            height: u32::from(height),
            monitors: Vec::new(),
        }),
        GfxPdu::CreateSurface(CreateSurfacePdu {
            surface_id: 1,
            width,
            height,
            pixel_format: PixelFormat::XRgb,
        }),
        GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
            surface_id: 1,
            output_origin_x: 0,
            output_origin_y: 0,
        }),
    ] {
        assert!(p.handle(pdu).is_none());
    }
    p
}

fn pixel(p: &Pipeline, x: usize, y: usize) -> [u8; 4] {
    let out = p.output().expect("output");
    let i = (y * usize::from(out.width) + x) * 4;
    out.data[i..i + 4].try_into().expect("4 bytes")
}

#[test]
fn nothing_is_drawn_before_the_server_resets_graphics() {
    let mut p = Pipeline::default();
    assert!(p.output().is_none());
    assert_eq!(p.take_changes(), Changes::default());
}

#[test]
fn reset_reports_the_new_size() {
    let mut p = desktop(64, 32);
    let changes = p.take_changes();
    assert_eq!(changes.resized, Some((64, 32)));
    assert_eq!(p.take_changes().resized, None);
}

#[test]
fn a_frame_shows_up_only_when_it_ends_and_is_acknowledged() {
    let mut p = desktop(16, 16);
    p.take_changes();
    assert!(p.handle(start(7)).is_none());
    p.handle(fill(1, [10, 20, 30], vec![rect(2, 2, 6, 6)]));
    assert!(p.take_changes().dirty.is_empty(), "nothing before EndFrame");
    assert_eq!(pixel(&p, 3, 3), [0, 0, 0, 255]);

    let ack = p.handle(end(7));
    let Some(GfxPdu::FrameAcknowledge(ack)) = ack else {
        panic!("expected an acknowledgement, got {ack:?}");
    };
    assert_eq!(ack.frame_id, 7);
    assert_eq!(ack.total_frames_decoded, 1);
    assert_eq!(p.take_changes().dirty, vec![Rect::new(2, 2, 4, 4)]);
    assert_eq!(pixel(&p, 3, 3), [10, 20, 30, 255]);
    assert_eq!(
        pixel(&p, 6, 6),
        [0, 0, 0, 255],
        "right/bottom are exclusive"
    );
}

#[test]
fn surfaces_show_at_their_origin() {
    let mut p = Pipeline::default();
    p.handle(GfxPdu::ResetGraphics(ResetGraphicsPdu {
        width: 32,
        height: 32,
        monitors: Vec::new(),
    }));
    p.handle(GfxPdu::CreateSurface(CreateSurfacePdu {
        surface_id: 5,
        width: 8,
        height: 8,
        pixel_format: PixelFormat::XRgb,
    }));
    // Drawn before it is mapped: shows once mapped.
    p.handle(fill(5, [1, 2, 3], vec![rect(0, 0, 8, 8)]));
    assert_eq!(pixel(&p, 10, 10), [0, 0, 0, 255]);
    p.handle(GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
        surface_id: 5,
        output_origin_x: 10,
        output_origin_y: 12,
    }));
    assert_eq!(pixel(&p, 10, 12), [1, 2, 3, 255]);
    assert_eq!(pixel(&p, 9, 12), [0, 0, 0, 255]);
    // A surface hanging over the edge is cut off, not a panic.
    p.handle(GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
        surface_id: 5,
        output_origin_x: 28,
        output_origin_y: 30,
    }));
    assert_eq!(pixel(&p, 31, 31), [1, 2, 3, 255]);
}

#[test]
fn surface_to_surface_and_the_cache_copy_pixels() {
    let mut p = desktop(32, 8);
    p.handle(fill(1, [9, 9, 9], vec![rect(0, 0, 4, 4)]));
    p.handle(GfxPdu::SurfaceToSurface(SurfaceToSurfacePdu {
        source_surface_id: 1,
        destination_surface_id: 1,
        source_rectangle: rect(0, 0, 4, 4),
        destination_points: vec![Point { x: 10, y: 0 }, Point { x: 20, y: 2 }],
    }));
    assert_eq!(pixel(&p, 11, 1), [9, 9, 9, 255]);
    assert_eq!(pixel(&p, 23, 5), [9, 9, 9, 255]);

    p.handle(GfxPdu::SurfaceToCache(SurfaceToCachePdu {
        surface_id: 1,
        cache_key: 42,
        cache_slot: 3,
        source_rectangle: rect(0, 0, 2, 2),
    }));
    p.handle(GfxPdu::CacheToSurface(CacheToSurfacePdu {
        cache_slot: 3,
        surface_id: 1,
        destination_points: vec![Point { x: 30, y: 6 }],
    }));
    assert_eq!(pixel(&p, 31, 7), [9, 9, 9, 255]);
    assert_eq!(p.cache_bytes, 2 * 2 * 4);

    p.handle(GfxPdu::EvictCacheEntry(
        ironrdp_egfx::pdu::EvictCacheEntryPdu { cache_slot: 3 },
    ));
    assert_eq!(p.cache_bytes, 0);
    // An empty slot is an error for the log, nothing more.
    p.handle(GfxPdu::CacheToSurface(CacheToSurfacePdu {
        cache_slot: 3,
        surface_id: 1,
        destination_points: vec![Point { x: 0, y: 0 }],
    }));
    assert_eq!(p.stats.errors, 1);
}

#[test]
fn cache_slots_outside_the_small_cache_are_refused() {
    let mut p = desktop(8, 8);
    for slot in [0, MAX_CACHE_SLOTS + 1] {
        p.handle(GfxPdu::SurfaceToCache(SurfaceToCachePdu {
            surface_id: 1,
            cache_key: 1,
            cache_slot: slot,
            source_rectangle: rect(0, 0, 2, 2),
        }));
    }
    assert!(p.cache.is_empty());
}

#[test]
fn oversized_surfaces_are_refused() {
    let mut p = desktop(8, 8);
    p.handle(GfxPdu::CreateSurface(CreateSurfacePdu {
        surface_id: 2,
        width: MAX_EDGE + 1,
        height: 10,
        pixel_format: PixelFormat::XRgb,
    }));
    assert!(!p.surfaces.contains_key(&2));
    // And updates for it are skipped.
    p.handle(fill(2, [1, 1, 1], vec![rect(0, 0, 1, 1)]));
}

#[test]
fn uncompressed_updates_decode() {
    let mut p = desktop(8, 8);
    p.take_changes();
    p.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
        surface_id: 1,
        codec_id: Codec1Type::Uncompressed,
        pixel_format: PixelFormat::XRgb,
        destination_rectangle: rect(4, 4, 6, 5),
        bitmap_data: [30, 20, 10, 0].repeat(2),
    }));
    assert_eq!(pixel(&p, 5, 4), [10, 20, 30, 255]);
    assert_eq!(p.take_changes().dirty, vec![Rect::new(4, 4, 2, 1)]);
}

#[test]
fn reset_clears_the_old_picture() {
    let mut p = desktop(8, 8);
    p.handle(fill(1, [5, 5, 5], vec![rect(0, 0, 8, 8)]));
    p.handle(GfxPdu::ResetGraphics(ResetGraphicsPdu {
        width: 8,
        height: 8,
        monitors: Vec::new(),
    }));
    assert_eq!(pixel(&p, 1, 1), [0, 0, 0, 255]);
    // The surface kept its place but lost its contents.
    p.handle(GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
        surface_id: 1,
        output_origin_x: 0,
        output_origin_y: 0,
    }));
    assert_eq!(pixel(&p, 1, 1), [0, 0, 0, 255]);
}

#[test]
fn split_pdus_stops_at_a_bad_length() {
    let a = encode_vec(&end(1)).expect("encode");
    let b = encode_vec(&end(2)).expect("encode");
    let mut data = [a.clone(), b.clone()].concat();
    assert_eq!(split_pdus(&data).collect::<Vec<_>>(), vec![&a[..], &b[..]]);
    data.extend_from_slice(&[1, 0, 0, 0, 0xFF, 0, 0, 0]);
    assert_eq!(split_pdus(&data).count(), 2);
}

/// Wraps PDUs the way a server does: zgfx segment, uncompressed.
fn wire(pdus: &[GfxPdu]) -> Vec<u8> {
    let mut plain = Vec::new();
    for pdu in pdus {
        plain.extend(encode_vec(pdu).expect("encode"));
    }
    zgfx::wrap_uncompressed(&plain)
}

#[cfg(feature = "h264")]
fn new_channel() -> (GfxChannel, Shared) {
    channel(None)
}

#[cfg(not(feature = "h264"))]
fn new_channel() -> (GfxChannel, Shared) {
    channel()
}

#[test]
fn the_channel_decodes_a_message_and_answers_end_frame() {
    let (mut channel, shared) = new_channel();
    let answers = channel
        .process(
            1,
            &wire(&[
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: 4,
                    height: 4,
                    monitors: Vec::new(),
                }),
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 1,
                    width: 4,
                    height: 4,
                    pixel_format: PixelFormat::XRgb,
                }),
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: 1,
                    output_origin_x: 0,
                    output_origin_y: 0,
                }),
                start(1),
                fill(1, [200, 100, 50], vec![rect(0, 0, 4, 4)]),
                end(1),
            ]),
        )
        .expect("process");
    assert_eq!(answers.len(), 1, "one FrameAcknowledge");
    assert_eq!(pixel(&shared.lock(), 2, 2), [200, 100, 50, 255]);

    // Garbage costs nothing but a log line.
    assert!(channel
        .process(1, &[0xE0, 0x04, 1, 2, 3])
        .expect("process")
        .is_empty());
    assert!(channel.process(1, &[]).expect("process").is_empty());
}

#[test]
fn capabilities_depend_on_h264() {
    let p = Pipeline::default();
    let caps = p.capabilities();
    assert!(
        matches!(caps[0], CapabilitySet::V10_7 { flags } if flags.contains(CapabilitiesV107Flags::AVC_DISABLED))
    );
    assert!(caps
        .iter()
        .all(|c| !matches!(c, CapabilitySet::V8_1 { flags } if flags.contains(CapabilitiesV81Flags::AVC420_ENABLED))));
}

#[cfg(feature = "h264")]
mod avc {
    use super::*;
    use ironrdp_egfx::pdu::{encode_avc420_bitmap_stream, Avc420Region};
    use openh264::encoder::Encoder;
    use openh264::formats::{RgbSliceU8, YUVBuffer};

    /// An H.264 access unit showing a flat colour, Annex B, as Windows sends it.
    fn encoded(width: usize, height: usize, rgb: [u8; 3]) -> Vec<u8> {
        let frame = rgb.repeat(width * height);
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&frame, (width, height)));
        let mut encoder = Encoder::new().expect("encoder");
        encoder.encode(&yuv).expect("encode").to_vec()
    }

    fn close_to(actual: [u8; 4], expected: [u8; 3]) -> bool {
        actual[..3]
            .iter()
            .zip(expected)
            .all(|(a, e)| a.abs_diff(e) <= 12)
    }

    #[test]
    fn avc420_draws_only_its_regions() {
        let mut p = desktop(64, 64);
        p.codecs.h264 = Some(h264::H264Decoder::from_source().expect("decoder"));
        let caps = p.capabilities();
        assert!(
            matches!(caps[0], CapabilitySet::V8_1 { flags } if flags.contains(CapabilitiesV81Flags::AVC420_ENABLED))
        );

        let h264 = encoded(64, 64, [200, 40, 90]);
        // Exclusive edges, as Windows sends them: the left half only.
        let region = Avc420Region::new(0, 0, 32, 64, 22, 100);
        p.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc420,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 64, 64),
            bitmap_data: encode_avc420_bitmap_stream(&[region], &h264),
        }));
        assert_eq!(p.stats.errors, 0);
        assert!(
            close_to(pixel(&p, 10, 10), [200, 40, 90]),
            "{:?}",
            pixel(&p, 10, 10)
        );
        assert_eq!(pixel(&p, 40, 10), [0, 0, 0, 255], "outside the region");
    }

    /// Cisco's binary itself, when `UWURDP_OPENH264` names a downloaded copy
    /// (the app never gets it from anywhere else, so neither do the tests).
    #[test]
    fn ciscos_binary_decodes_like_the_source_build() {
        let Some(path) = std::env::var_os("UWURDP_OPENH264") else {
            eprintln!("UWURDP_OPENH264 not set; skipping");
            return;
        };
        let mut p = desktop(64, 64);
        p.codecs.h264 = Some(h264::H264Decoder::load(path.as_ref()).expect("Cisco's OpenH264"));
        let h264 = encoded(64, 64, [30, 180, 220]);
        let region = Avc420Region::new(0, 0, 64, 64, 22, 100);
        p.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc420,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 64, 64),
            bitmap_data: encode_avc420_bitmap_stream(&[region], &h264),
        }));
        assert_eq!(p.stats.errors, 0);
        for (x, y) in [(0, 0), (32, 32), (63, 63)] {
            assert!(
                close_to(pixel(&p, x, y), [30, 180, 220]),
                "{:?}",
                pixel(&p, x, y)
            );
        }
    }

    #[test]
    fn avc420_without_a_decoder_is_an_error_not_a_crash() {
        let mut p = desktop(16, 16);
        p.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc420,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 16, 16),
            bitmap_data: vec![0, 0, 0, 0],
        }));
        assert_eq!(p.stats.errors, 1);
    }
}
