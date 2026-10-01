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

#[test]
fn the_confirmed_version_decides_which_avc_codecs_are_taken() {
    let mut p = desktop(16, 16);
    let confirm = |set: CapabilitySet| {
        GfxPdu::CapabilitiesConfirm(ironrdp_egfx::pdu::CapabilitiesConfirmPdu::from_typed(&set))
    };
    assert_eq!(p.allowed, Allowed::default(), "nothing before the confirm");
    p.handle(confirm(CapabilitySet::V10_7 {
        flags: CapabilitiesV107Flags::SMALL_CACHE,
    }));
    assert!(p.allowed.avc420 && p.allowed.avc444);
    p.handle(confirm(CapabilitySet::V10_7 {
        flags: CapabilitiesV107Flags::AVC_DISABLED,
    }));
    assert_eq!(p.allowed, Allowed::default());
    p.handle(confirm(CapabilitySet::V8_1 {
        flags: CapabilitiesV81Flags::AVC420_ENABLED,
    }));
    assert!(p.allowed.avc420 && !p.allowed.avc444);
    p.handle(confirm(CapabilitySet::V10_4 {
        flags: CapabilitiesV104Flags::SMALL_CACHE,
    }));
    assert!(p.allowed.avc444);
}

#[cfg(feature = "h264")]
mod avc {
    use super::*;
    use crate::gfx::avc::Avc;
    use crate::gfx::avc444_split::Yuv444;
    use ironrdp_core::encode_vec;
    use ironrdp_egfx::pdu::{
        encode_avc420_bitmap_stream, Avc420BitmapStream, Avc420Region, Avc444BitmapStream,
        CapabilitiesConfirmPdu, Encoding,
    };
    use openh264::encoder::{BitRate, Encoder, EncoderConfig, QpRange};
    use openh264::formats::YUVBuffer;
    use openh264::OpenH264API;

    /// An encoder at the quality Windows uses for a desktop that stands
    /// still, and that never skips a picture.
    fn encoder() -> Encoder {
        let config = EncoderConfig::new()
            .skip_frames(false)
            .bitrate(BitRate::from_bps(50_000_000))
            .qp(QpRange::new(0, 12));
        Encoder::with_api_config(OpenH264API::from_source(), config).expect("encoder")
    }

    fn encode(encoder: &mut Encoder, i420: Vec<u8>, width: usize, height: usize) -> Vec<u8> {
        let yuv = YUVBuffer::from_vec(i420, width, height);
        encoder.encode(&yuv).expect("encode").to_vec()
    }

    /// An H.264 access unit showing a flat colour, Annex B, as Windows sends it.
    fn encoded(width: usize, height: usize, rgb: [u8; 3]) -> Vec<u8> {
        let picture = Yuv444::from_rgb(&rgb.repeat(width * height), width, height);
        encode(&mut encoder(), picture.main_view(), width, height)
    }

    fn close_to(actual: [u8; 4], expected: [u8; 3]) -> bool {
        actual[..3]
            .iter()
            .zip(expected)
            .all(|(a, e)| a.abs_diff(e) <= 12)
    }

    /// A desktop whose server confirmed 10.7 with AVC on, decoding with
    /// OpenH264 built from source.
    fn h264_desktop(width: u16, height: u16) -> Pipeline {
        let mut p = desktop(width, height);
        p.codecs.h264 = Some(Avc::new(h264::Library::source()));
        p.handle(GfxPdu::CapabilitiesConfirm(
            CapabilitiesConfirmPdu::from_typed(&CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::SMALL_CACHE,
            }),
        ));
        p
    }

    fn whole(width: u16, height: u16) -> Avc420Region {
        // Exclusive edges, as Windows sends them.
        Avc420Region::new(0, 0, width, height, 10, 100)
    }

    #[test]
    fn with_h264_every_version_offers_avc() {
        let mut p = Pipeline::default();
        p.codecs.h264 = Some(Avc::new(h264::Library::source()));
        let caps = p.capabilities();
        assert!(
            matches!(caps[0], CapabilitySet::V10_7 { flags } if !flags.contains(CapabilitiesV107Flags::AVC_DISABLED) && !flags.contains(CapabilitiesV107Flags::AVC_THIN_CLIENT))
        );
        for version in [
            CapabilitySet::V10_6 {
                flags: CapabilitiesV104Flags::SMALL_CACHE,
            },
            CapabilitySet::V10_1,
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
            },
        ] {
            assert!(caps.contains(&version), "{version:?} missing");
        }
        assert!(caps
            .iter()
            .all(|c| Allowed::from_confirmed(c).avc420 || matches!(c, CapabilitySet::V8 { .. })));
    }

    #[test]
    fn avc420_draws_only_its_regions() {
        let mut p = h264_desktop(64, 64);
        let h264 = encoded(64, 64, [200, 40, 90]);
        // The left half only.
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

    /// Coloured strokes one and two pixels wide on white, like ClearType
    /// text: what 4:2:0 smears.
    fn text_like(width: usize, height: usize) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                let px: [u8; 3] = match (x % 6, y % 8) {
                    (_, 7) => [255, 255, 255],
                    (0, _) => [200, 20, 20],
                    (1, _) | (4, 2..=5) => [255, 255, 255],
                    (2, _) => [20, 40, 210],
                    (3, _) => [20, 150, 40],
                    _ => [255, 255, 255],
                };
                rgb.extend_from_slice(&px);
            }
        }
        rgb
    }

    /// The average and the largest difference of any channel between the
    /// desktop and `rgb`.
    fn error(p: &Pipeline, rgb: &[u8]) -> (f64, u8) {
        let out = p.output().expect("output");
        let mut sum = 0u64;
        let mut worst = 0u8;
        for (px, expected) in out.data.chunks(4).zip(rgb.chunks(3)) {
            for c in 0..3 {
                let d = px[c].abs_diff(expected[c]);
                sum += u64::from(d);
                worst = worst.max(d);
            }
        }
        (sum as f64 / rgb.len() as f64, worst)
    }

    fn avc444_pdu(codec_id: Codec1Type, encoding: Encoding, streams: &[&[u8]]) -> GfxPdu {
        let region = whole(64, 64);
        let stream = |data| Avc420BitmapStream {
            rectangles: vec![region.to_rectangle()],
            quant_qual_vals: vec![region.to_quant_quality()],
            data,
        };
        GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 64, 64),
            bitmap_data: encode_vec(&Avc444BitmapStream {
                encoding,
                stream1: stream(streams[0]),
                stream2: streams.get(1).map(|data| stream(data)),
            })
            .expect("encode"),
        })
    }

    #[test]
    fn avc444_keeps_the_colour_of_thin_strokes() {
        let (w, h) = (64, 64);
        let rgb = text_like(w, h);
        let picture = Yuv444::from_rgb(&rgb, w, h);

        // AVC420 for comparison: the same main view on its own.
        let mut plain = h264_desktop(64, 64);
        let main = encode(&mut encoder(), picture.main_view(), w, h);
        plain.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc420,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 64, 64),
            bitmap_data: encode_avc420_bitmap_stream(&[whole(64, 64)], &main),
        }));
        let (smeared, _) = error(&plain, &rgb);

        for (codec, aux) in [
            (Codec1Type::Avc444, picture.aux_view_v1()),
            (Codec1Type::Avc444v2, picture.aux_view_v2()),
        ] {
            // Both views through one encoder, one after the other.
            let mut encoder = encoder();
            let luma = encode(&mut encoder, picture.main_view(), w, h);
            let chroma = encode(&mut encoder, aux, w, h);
            let mut p = h264_desktop(64, 64);
            p.handle(avc444_pdu(
                codec,
                Encoding::LUMA_AND_CHROMA,
                &[&luma, &chroma],
            ));
            assert_eq!(p.stats.errors, 0);
            let (average, worst) = error(&p, &rgb);
            assert!(
                average < 6.0 && worst < 100,
                "{codec:?}: average {average:.2}, worst {worst}, 4:2:0 {smeared:.2}"
            );
            assert!(
                average * 4.0 < smeared,
                "{codec:?}: {average:.2} against AVC420's {smeared:.2}"
            );

            // Luma alone first, as for something moving, then the chroma
            // on its own: the same picture in the end.
            let mut q = h264_desktop(64, 64);
            q.handle(avc444_pdu(codec, Encoding::LUMA, &[&luma]));
            assert!(error(&q, &rgb).0 > average * 2.0, "luma alone is 4:2:0");
            q.handle(avc444_pdu(codec, Encoding::CHROMA, &[&chroma]));
            assert_eq!(q.stats.errors, 0);
            assert_eq!(q.output(), p.output());
        }
    }

    #[test]
    fn avc444_is_refused_when_the_server_confirmed_8_1() {
        let mut p = desktop(16, 16);
        p.codecs.h264 = Some(Avc::new(h264::Library::source()));
        p.handle(GfxPdu::CapabilitiesConfirm(
            CapabilitiesConfirmPdu::from_typed(&CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED,
            }),
        ));
        p.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc444,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 16, 16),
            bitmap_data: vec![0; 8],
        }));
        assert_eq!(p.stats.errors, 1);
        assert_eq!(p.stats.avc444, 0);
    }

    /// Cisco's binary itself, when `UWURDP_OPENH264` names a downloaded copy
    /// (the app never gets it from anywhere else, so neither do the tests).
    #[test]
    fn ciscos_binary_decodes_like_the_source_build() {
        let Some(path) = std::env::var_os("UWURDP_OPENH264") else {
            eprintln!("UWURDP_OPENH264 not set; skipping");
            return;
        };
        let mut p = h264_desktop(64, 64);
        p.codecs.h264 = Some(Avc::new(
            h264::Library::load(path.as_ref()).expect("Cisco's OpenH264"),
        ));
        let h264 = encoded(64, 64, [30, 180, 220]);
        p.handle(GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc420,
            pixel_format: PixelFormat::XRgb,
            destination_rectangle: rect(0, 0, 64, 64),
            bitmap_data: encode_avc420_bitmap_stream(&[whole(64, 64)], &h264),
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
        p.allowed.avc420 = true;
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
