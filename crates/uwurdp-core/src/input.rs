//! Keyboard and mouse input from the page, turned into RDP fast-path events.
//!
//! [`ironrdp_input::Database`] keeps track of what is pressed, so a key that
//! is already down is not pressed twice, a release of something never
//! pressed is dropped, and `ReleaseAll` (sent on blur) releases exactly what
//! the server believes is held — otherwise alt-tabbing away from the window
//! leaves a stuck Alt on the remote side.

use ironrdp_input::{Database, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp_pdu::input::fast_path::FastPathInputEvent;
use serde::Deserialize;

/// One input event from the page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputEvent {
    /// PS/2 set-1 scancode as the page maps `KeyboardEvent.code`.
    Key {
        code: u16,
        extended: bool,
        down: bool,
    },
    /// For characters without a scancode mapping.
    Unicode {
        ch: String,
        down: bool,
    },
    Move {
        x: u16,
        y: u16,
    },
    Button {
        button: MouseButton,
        down: bool,
        x: u16,
        y: u16,
    },
    /// 120 per notch, positive = away from the user / to the right.
    Wheel {
        delta: i16,
        horizontal: bool,
    },
    /// On blur: release every pressed key and button.
    ReleaseAll,
    CtrlAltDel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl From<MouseButton> for ironrdp_input::MouseButton {
    fn from(value: MouseButton) -> Self {
        match value {
            MouseButton::Left => Self::Left,
            MouseButton::Right => Self::Right,
            MouseButton::Middle => Self::Middle,
            MouseButton::X1 => Self::X1,
            MouseButton::X2 => Self::X2,
        }
    }
}

/// One wheel event carries at most this much rotation: the PDU field is a
/// 9-bit signed value, and one notch per event is what Windows itself sends.
const WHEEL_STEP: i16 = 120;

const SCANCODE_CTRL: u8 = 0x1D;
const SCANCODE_ALT: u8 = 0x38;
const SCANCODE_DELETE: u8 = 0x53; // extended

/// Pressed-state tracking plus the desktop bounds pointer coordinates are
/// clamped to.
pub(crate) struct InputState {
    db: Database,
    width: u16,
    height: u16,
}

impl InputState {
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Self {
            db: Database::new(),
            width,
            height,
        }
    }

    pub(crate) fn set_desktop_size(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    fn position(&self, x: u16, y: u16) -> MousePosition {
        MousePosition {
            x: x.min(self.width.saturating_sub(1)),
            y: y.min(self.height.saturating_sub(1)),
        }
    }

    /// Translates page events into fast-path events, in order.
    pub(crate) fn translate(&mut self, events: &[InputEvent]) -> Vec<FastPathInputEvent> {
        let mut out = Vec::new();
        for event in events {
            match event {
                InputEvent::Key {
                    code,
                    extended,
                    down,
                } => {
                    // Accept both the bare code with a flag and the 0xE0xx form.
                    let extended = *extended || code & 0xFF00 == 0xE000;
                    let scancode = Scancode::from_u8(extended, (code & 0xFF) as u8);
                    let op = if *down {
                        Operation::KeyPressed(scancode)
                    } else {
                        Operation::KeyReleased(scancode)
                    };
                    out.extend(self.db.apply([op]));
                }
                InputEvent::Unicode { ch, down } => {
                    let ops: Vec<Operation> = ch
                        .chars()
                        .map(|c| {
                            if *down {
                                Operation::UnicodeKeyPressed(c)
                            } else {
                                Operation::UnicodeKeyReleased(c)
                            }
                        })
                        .collect();
                    out.extend(self.db.apply(ops));
                }
                InputEvent::Move { x, y } => {
                    let pos = self.position(*x, *y);
                    out.extend(self.db.apply([Operation::MouseMove(pos)]));
                }
                InputEvent::Button { button, down, x, y } => {
                    // The press/release PDU carries the database's position,
                    // so move there first (a no-op if already there).
                    let pos = self.position(*x, *y);
                    let button = ironrdp_input::MouseButton::from(*button);
                    let op = if *down {
                        Operation::MouseButtonPressed(button)
                    } else {
                        Operation::MouseButtonReleased(button)
                    };
                    out.extend(self.db.apply([Operation::MouseMove(pos), op]));
                }
                InputEvent::Wheel { delta, horizontal } => {
                    let mut remaining = *delta;
                    while remaining != 0 {
                        let step = remaining.clamp(-WHEEL_STEP, WHEEL_STEP);
                        remaining -= step;
                        out.extend(self.db.apply([Operation::WheelRotations(WheelRotations {
                            is_vertical: !horizontal,
                            rotation_units: step,
                        })]));
                    }
                }
                InputEvent::ReleaseAll => out.extend(self.db.release_all()),
                InputEvent::CtrlAltDel => {
                    let ctrl = Scancode::from_u8(false, SCANCODE_CTRL);
                    let alt = Scancode::from_u8(false, SCANCODE_ALT);
                    let del = Scancode::from_u8(true, SCANCODE_DELETE);
                    out.extend(self.db.apply([
                        Operation::KeyPressed(ctrl),
                        Operation::KeyPressed(alt),
                        Operation::KeyPressed(del),
                        Operation::KeyReleased(del),
                        Operation::KeyReleased(alt),
                        Operation::KeyReleased(ctrl),
                    ]));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_pdu::input::fast_path::KeyboardFlags;
    use ironrdp_pdu::input::mouse::PointerFlags;

    fn state() -> InputState {
        InputState::new(1024, 768)
    }

    #[test]
    fn events_deserialize_from_the_page_shape() {
        let events: Vec<InputEvent> = serde_json::from_value(serde_json::json!([
            { "type": "key", "code": 30, "extended": false, "down": true },
            { "type": "unicode", "ch": "é", "down": true },
            { "type": "move", "x": 5, "y": 6 },
            { "type": "button", "button": "x1", "down": true, "x": 1, "y": 2 },
            { "type": "wheel", "delta": -120, "horizontal": false },
            { "type": "releaseAll" },
            { "type": "ctrlAltDel" }
        ]))
        .expect("parse");
        assert_eq!(events.len(), 7);
        assert_eq!(
            events[3],
            InputEvent::Button {
                button: MouseButton::X1,
                down: true,
                x: 1,
                y: 2
            }
        );
        assert_eq!(events[5], InputEvent::ReleaseAll);
    }

    #[test]
    fn key_press_and_release() {
        let mut s = state();
        let out = s.translate(&[
            InputEvent::Key {
                code: 0x1E,
                extended: false,
                down: true,
            },
            InputEvent::Key {
                code: 0x1E,
                extended: false,
                down: false,
            },
        ]);
        assert_eq!(
            out,
            vec![
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1E),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1E),
            ]
        );
    }

    #[test]
    fn e0_prefixed_codes_are_extended() {
        let mut s = state();
        let out = s.translate(&[InputEvent::Key {
            code: 0xE048,
            extended: false,
            down: true,
        }]);
        assert_eq!(
            out,
            vec![FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED,
                0x48
            )]
        );
    }

    #[test]
    fn releasing_an_unpressed_key_sends_nothing() {
        let mut s = state();
        let out = s.translate(&[InputEvent::Key {
            code: 0x1E,
            extended: false,
            down: false,
        }]);
        assert!(out.is_empty());
    }

    #[test]
    fn release_all_releases_what_is_held() {
        let mut s = state();
        s.translate(&[
            InputEvent::Key {
                code: 0x38,
                extended: false,
                down: true,
            },
            InputEvent::Button {
                button: MouseButton::Left,
                down: true,
                x: 10,
                y: 10,
            },
        ]);
        let out = s.translate(&[InputEvent::ReleaseAll]);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&FastPathInputEvent::KeyboardEvent(
            KeyboardFlags::RELEASE,
            0x38
        )));
        // And nothing is left to release.
        assert!(s.translate(&[InputEvent::ReleaseAll]).is_empty());
    }

    #[test]
    fn button_moves_first_and_carries_the_position() {
        let mut s = state();
        let out = s.translate(&[InputEvent::Button {
            button: MouseButton::Left,
            down: true,
            x: 100,
            y: 200,
        }]);
        assert_eq!(out.len(), 2);
        match (&out[0], &out[1]) {
            (FastPathInputEvent::MouseEvent(mv), FastPathInputEvent::MouseEvent(press)) => {
                assert_eq!(mv.flags, PointerFlags::MOVE);
                assert_eq!((press.x_position, press.y_position), (100, 200));
                assert!(press
                    .flags
                    .contains(PointerFlags::DOWN | PointerFlags::LEFT_BUTTON));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pointer_is_clamped_to_the_desktop() {
        let mut s = state();
        let out = s.translate(&[InputEvent::Move { x: 5000, y: 5000 }]);
        match &out[..] {
            [FastPathInputEvent::MouseEvent(mv)] => {
                assert_eq!((mv.x_position, mv.y_position), (1023, 767))
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn big_wheel_deltas_are_split_into_notches() {
        let mut s = state();
        let out = s.translate(&[InputEvent::Wheel {
            delta: -300,
            horizontal: false,
        }]);
        let units: Vec<i16> = out
            .iter()
            .map(|e| match e {
                FastPathInputEvent::MouseEvent(m) => {
                    assert!(m.flags.contains(PointerFlags::VERTICAL_WHEEL));
                    m.number_of_wheel_rotation_units
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(units, vec![-120, -120, -60]);

        let out = s.translate(&[InputEvent::Wheel {
            delta: 120,
            horizontal: true,
        }]);
        match &out[..] {
            [FastPathInputEvent::MouseEvent(m)] => {
                assert!(m.flags.contains(PointerFlags::HORIZONTAL_WHEEL));
                assert_eq!(m.number_of_wheel_rotation_units, 120);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unicode_uses_utf16_code_units() {
        let mut s = state();
        let out = s.translate(&[InputEvent::Unicode {
            ch: "😀".into(),
            down: true,
        }]);
        // Outside the BMP: a surrogate pair.
        assert_eq!(
            out,
            vec![
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), 0xD83D),
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), 0xDE00),
            ]
        );
    }

    #[test]
    fn ctrl_alt_del_is_a_full_press_and_release() {
        let mut s = state();
        let out = s.translate(&[InputEvent::CtrlAltDel]);
        assert_eq!(
            out,
            vec![
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1D),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x38),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED, 0x53),
                FastPathInputEvent::KeyboardEvent(
                    KeyboardFlags::RELEASE | KeyboardFlags::EXTENDED,
                    0x53
                ),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1D),
            ]
        );
    }
}
