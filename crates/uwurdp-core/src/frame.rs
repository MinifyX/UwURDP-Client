//! The binary messages the page receives.
//!
//! Every message starts with a one-byte kind; all integers are little-endian.
//! One [`FrameSink::send`](crate::FrameSink::send) carries exactly one message.
//!
//! | kind | name              | payload                                                            |
//! |------|-------------------|--------------------------------------------------------------------|
//! | 1    | `BITMAPS`         | u16 count, then per rect: u16 x, y, w, h + w·h·4 bytes RGBA        |
//! | 2    | `DESKTOP_SIZE`    | u16 width, u16 height                                              |
//! | 3    | `POINTER_BITMAP`  | u16 hot_x, hot_y, w, h + w·h·4 bytes RGBA                          |
//! | 4    | `POINTER_DEFAULT` | —                                                                  |
//! | 5    | `POINTER_HIDDEN`  | —                                                                  |
//! | 6    | `POINTER_POSITION`| u16 x, u16 y                                                       |
//! | 7    | `CLOSED`          | UTF-8 JSON `{"reason":"logoff|disconnect|server|error","message":…}` |
//!
//! Pixels are straight (not premultiplied) RGBA, row-major, without padding,
//! which is exactly what `ImageData` wants. Desktop pixels always have A=255.

use crate::dirty::Rect;
use serde::Serialize;

pub const KIND_BITMAPS: u8 = 1;
pub const KIND_DESKTOP_SIZE: u8 = 2;
pub const KIND_POINTER_BITMAP: u8 = 3;
pub const KIND_POINTER_DEFAULT: u8 = 4;
pub const KIND_POINTER_HIDDEN: u8 = 5;
pub const KIND_POINTER_POSITION: u8 = 6;
pub const KIND_CLOSED: u8 = 7;

const BITMAPS_HEADER: usize = 1 + 2;
const RECT_HEADER: usize = 4 * 2;

/// Upper bound on one `BITMAPS` message. Large enough that a full 4K frame
/// (3840×2160×4 ≈ 31.6 MiB) still goes out in one piece; anything bigger is
/// cut into horizontal strips spread over several messages, so a single
/// message never makes the page decode an unbounded buffer.
pub const MAX_BITMAPS_BYTES: usize = 32 * 1024 * 1024;

/// A `BITMAPS` message can hold at most this many rectangles (the count is a
/// u16). The dirty region keeps far fewer, but the encoder does not rely on it.
const MAX_RECTS_PER_MESSAGE: usize = u16::MAX as usize;

// Compile-time proof that the biggest single strip still fits: one full row
// of the widest desktop RDP allows, plus headers.
const _: () = assert!(BITMAPS_HEADER + RECT_HEADER + 8192 * 4 <= MAX_BITMAPS_BYTES);

/// Why the session ended, as the page sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseReason {
    /// The remote session was logged off.
    Logoff,
    /// Disconnected, the remote session keeps running (by us, by the user
    /// inside the session, by an administrator or another connection).
    Disconnect,
    /// The server ended the connection for another reason.
    Server,
    /// A network or protocol failure.
    Error,
}

#[derive(Serialize)]
struct ClosedPayload<'a> {
    reason: CloseReason,
    message: &'a str,
}

pub fn desktop_size(width: u16, height: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.push(KIND_DESKTOP_SIZE);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out
}

pub fn pointer_default() -> Vec<u8> {
    vec![KIND_POINTER_DEFAULT]
}

pub fn pointer_hidden() -> Vec<u8> {
    vec![KIND_POINTER_HIDDEN]
}

pub fn pointer_position(x: u16, y: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.push(KIND_POINTER_POSITION);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out
}

/// A pointer image. `rgba` must hold `w·h·4` bytes of straight RGBA; if it
/// does not (a malformed pointer from the server), `None` is returned rather
/// than sending the page something it would misread.
pub fn pointer_bitmap(hot_x: u16, hot_y: u16, w: u16, h: u16, rgba: &[u8]) -> Option<Vec<u8>> {
    let expected = usize::from(w) * usize::from(h) * 4;
    if rgba.len() != expected {
        return None;
    }
    let mut out = Vec::with_capacity(9 + expected);
    out.push(KIND_POINTER_BITMAP);
    for v in [hot_x, hot_y, w, h] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(rgba);
    Some(out)
}

pub fn closed(reason: CloseReason, message: &str) -> Vec<u8> {
    let json = serde_json::to_vec(&ClosedPayload { reason, message })
        // Serializing two plain fields cannot fail; keep a valid message anyway.
        .unwrap_or_else(|_| br#"{"reason":"error","message":""}"#.to_vec());
    let mut out = Vec::with_capacity(1 + json.len());
    out.push(KIND_CLOSED);
    out.extend_from_slice(&json);
    out
}

/// Bytes a `BITMAPS` message with these rectangles takes.
pub fn bitmaps_size(rects: &[Rect]) -> usize {
    BITMAPS_HEADER
        + rects
            .iter()
            .map(|r| RECT_HEADER + usize::from(r.w) * usize::from(r.h) * 4)
            .sum::<usize>()
}

/// Splits `rects` into groups that each fit in one `BITMAPS` message,
/// cutting rectangles that are too big on their own into horizontal strips.
/// Together the groups cover exactly the input.
pub fn plan_bitmaps(rects: &[Rect]) -> Vec<Vec<Rect>> {
    let mut messages = Vec::new();
    let mut current: Vec<Rect> = Vec::new();
    let mut current_size = BITMAPS_HEADER;

    let mut pieces = Vec::new();
    for rect in rects.iter().filter(|r| !r.is_empty()) {
        let row_bytes = usize::from(rect.w) * 4;
        let max_rows = (MAX_BITMAPS_BYTES - BITMAPS_HEADER - RECT_HEADER) / row_bytes;
        // At least one row per strip; the const assert above makes that fit.
        let max_rows = u16::try_from(max_rows.max(1)).unwrap_or(u16::MAX);
        let mut y = rect.y;
        let mut remaining = rect.h;
        while remaining > 0 {
            let h = remaining.min(max_rows);
            pieces.push(Rect::new(rect.x, y, rect.w, h));
            y = y.saturating_add(h);
            remaining -= h;
        }
    }

    for piece in pieces {
        let piece_size = RECT_HEADER + usize::from(piece.w) * usize::from(piece.h) * 4;
        if !current.is_empty()
            && (current_size + piece_size > MAX_BITMAPS_BYTES
                || current.len() >= MAX_RECTS_PER_MESSAGE)
        {
            messages.push(std::mem::take(&mut current));
            current_size = BITMAPS_HEADER;
        }
        current.push(piece);
        current_size += piece_size;
    }
    if !current.is_empty() {
        messages.push(current);
    }
    messages
}

/// Encodes one `BITMAPS` message from an RGBA framebuffer (`stride` bytes per
/// row). Rectangles must lie inside the framebuffer — the caller clips — and
/// should come from [`plan_bitmaps`] so the message stays under the cap.
/// Alpha is forced to 255: RDP desktops are opaque, but some codecs leave the
/// padding byte at zero, which would show up as holes in the canvas.
pub fn bitmaps(framebuffer: &[u8], stride: usize, rects: &[Rect]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bitmaps_size(rects));
    out.push(KIND_BITMAPS);
    let count = u16::try_from(rects.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for rect in rects.iter().take(usize::from(count)) {
        for v in [rect.x, rect.y, rect.w, rect.h] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let row_bytes = usize::from(rect.w) * 4;
        for row in 0..usize::from(rect.h) {
            let start = (usize::from(rect.y) + row) * stride + usize::from(rect.x) * 4;
            let begin = out.len();
            match framebuffer.get(start..start + row_bytes) {
                Some(src) => out.extend_from_slice(src),
                // Out of bounds would be a caller bug; keep the message well
                // formed (black) instead of panicking in the session task.
                None => out.resize(begin + row_bytes, 0),
            }
            for alpha in out[begin..].iter_mut().skip(3).step_by(4) {
                *alpha = 0xFF;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u16(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    #[test]
    fn desktop_size_layout() {
        assert_eq!(desktop_size(1920, 1080), vec![2, 0x80, 0x07, 0x38, 0x04]);
    }

    #[test]
    fn pointer_messages_layout() {
        assert_eq!(pointer_default(), vec![4]);
        assert_eq!(pointer_hidden(), vec![5]);
        assert_eq!(pointer_position(258, 3), vec![6, 2, 1, 3, 0]);

        let rgba = vec![9u8; 2 * 3 * 4];
        let msg = pointer_bitmap(1, 2, 2, 3, &rgba).expect("well formed");
        assert_eq!(msg[0], KIND_POINTER_BITMAP);
        assert_eq!(
            [
                read_u16(&msg, 1),
                read_u16(&msg, 3),
                read_u16(&msg, 5),
                read_u16(&msg, 7)
            ],
            [1, 2, 2, 3]
        );
        assert_eq!(&msg[9..], &rgba[..]);
    }

    #[test]
    fn malformed_pointer_is_refused() {
        assert!(pointer_bitmap(0, 0, 4, 4, &[0; 10]).is_none());
    }

    #[test]
    fn closed_carries_json() {
        let msg = closed(CloseReason::Logoff, "You were \"logged off\".");
        assert_eq!(msg[0], KIND_CLOSED);
        let value: serde_json::Value = serde_json::from_slice(&msg[1..]).expect("json");
        assert_eq!(value["reason"], "logoff");
        assert_eq!(value["message"], "You were \"logged off\".");
    }

    #[test]
    fn bitmaps_copy_the_right_pixels_and_force_alpha() {
        // A 4×3 framebuffer where each pixel encodes its own position.
        let (w, h) = (4usize, 3usize);
        let mut fb = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                fb[i] = x as u8;
                fb[i + 1] = y as u8;
                fb[i + 2] = 7;
                fb[i + 3] = 0; // as some codecs leave it
            }
        }
        let rects = [Rect::new(1, 1, 2, 2), Rect::new(3, 0, 1, 1)];
        let msg = bitmaps(&fb, w * 4, &rects);
        assert_eq!(msg.len(), bitmaps_size(&rects));
        assert_eq!(msg[0], KIND_BITMAPS);
        assert_eq!(read_u16(&msg, 1), 2);
        // First rect header, then its 4 pixels.
        assert_eq!(
            [
                read_u16(&msg, 3),
                read_u16(&msg, 5),
                read_u16(&msg, 7),
                read_u16(&msg, 9)
            ],
            [1, 1, 2, 2]
        );
        let px = &msg[11..11 + 16];
        assert_eq!(
            px,
            &[1, 1, 7, 255, 2, 1, 7, 255, 1, 2, 7, 255, 2, 2, 7, 255]
        );
        // Second rect.
        let second = 11 + 16;
        assert_eq!(read_u16(&msg, second), 3);
        assert_eq!(&msg[second + 8..], &[3, 0, 7, 255]);
    }

    #[test]
    fn plan_keeps_small_updates_in_one_message() {
        let rects = [Rect::new(0, 0, 100, 100), Rect::new(200, 200, 50, 50)];
        assert_eq!(plan_bitmaps(&rects), vec![rects.to_vec()]);
    }

    #[test]
    fn a_4k_frame_fits_one_message() {
        let full = [Rect::new(0, 0, 3840, 2160)];
        let plan = plan_bitmaps(&full);
        assert_eq!(plan, vec![full.to_vec()]);
        assert!(bitmaps_size(&plan[0]) <= MAX_BITMAPS_BYTES);
    }

    #[test]
    fn a_huge_frame_is_split_into_strips_that_cover_it() {
        let full = Rect::new(0, 0, 8192, 8192);
        let plan = plan_bitmaps(&[full]);
        assert!(plan.len() > 1);
        let mut next_y = 0u32;
        for message in &plan {
            assert!(bitmaps_size(message) <= MAX_BITMAPS_BYTES);
            for strip in message {
                assert_eq!((strip.x, strip.w), (0, 8192));
                assert_eq!(u32::from(strip.y), next_y);
                next_y += u32::from(strip.h);
            }
        }
        assert_eq!(next_y, 8192);
    }

    #[test]
    fn out_of_bounds_rect_does_not_panic() {
        let fb = vec![0u8; 4 * 4 * 4];
        let msg = bitmaps(&fb, 16, &[Rect::new(2, 2, 4, 4)]);
        assert_eq!(msg.len(), bitmaps_size(&[Rect::new(2, 2, 4, 4)]));
    }
}
