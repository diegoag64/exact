//! A headset's controllers as a canvas takes them (`docs/quest.md`,
//! "Controllers"). Each headset frame a headset host reads the head and each
//! hand's aim and grip poses, trigger, squeeze, thumbstick and buttons, in
//! the session's reference space, and delivers them as
//! [`crate::InputEvent::Xr`], the way every other device reaches a canvas.
//! A host in another artifact hands them over the C ABI as
//! [`XR_INPUT_FLOATS`] numbers (`gpu_input_xr`, [`XrInput::from_floats`]).

/// A rigid pose in the session's reference space (metres, +Y up, the person
/// facing −Z when the session started): a position and a unit quaternion
/// `[x, y, z, w]`. A pose points along its own −Z with +Y up, as OpenXR's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct XrPose {
    /// Metres.
    pub position: [f32; 3],
    /// A unit quaternion, `[x, y, z, w]`.
    pub orientation: [f32; 4],
}

impl Default for XrPose {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            orientation: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl XrPose {
    fn finite(&self) -> bool {
        self.position
            .iter()
            .chain(&self.orientation)
            .all(|v| v.is_finite())
    }
}

/// A controller's buttons, as bits of [`XrHand::buttons`]. The face buttons
/// are named by place, not letter: on Touch controllers the right hand's
/// primary is A and secondary B, the left hand's X and Y.
pub mod button {
    /// The index trigger, past the runtime's press threshold.
    pub const TRIGGER: u32 = 1;
    /// The grip (squeeze) button, past the runtime's press threshold.
    pub const SQUEEZE: u32 = 1 << 1;
    /// A (right) or X (left).
    pub const PRIMARY: u32 = 1 << 2;
    /// B (right) or Y (left).
    pub const SECONDARY: u32 = 1 << 3;
    /// The menu button (the left controller's, on Touch).
    pub const MENU: u32 = 1 << 4;
    /// The thumbstick pressed in.
    pub const STICK: u32 = 1 << 5;
    /// Every bit a host may set.
    pub const ALL: u32 = (1 << 6) - 1;
}

/// One hand's controller for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct XrHand {
    /// The runtime has a controller in this hand (its actions are active).
    pub active: bool,
    /// The poses below are tracked this frame; they keep their last values
    /// while not.
    pub tracked: bool,
    /// The pointing pose: a ray from its position along its −Z.
    pub aim: XrPose,
    /// The holding pose, at the palm: what a held object is drawn at.
    pub grip: XrPose,
    /// The index trigger's travel in `[0, 1]`.
    pub trigger: f32,
    /// The grip button's travel in `[0, 1]`.
    pub squeeze: f32,
    /// The thumbstick in `[-1, 1]`: +x right, +y forward.
    pub stick: [f32; 2],
    /// Held buttons, bits of [`button`].
    pub buttons: u32,
}

/// The head and both hands for one frame, in the session's reference space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct XrInput {
    /// The head's pose is tracked this frame.
    pub head_tracked: bool,
    /// The head (the midpoint of the eyes).
    pub head: XrPose,
    /// Left, right.
    pub hands: [XrHand; 2],
    /// How many times the runtime has moved the reference space under the
    /// person since the session began (a recenter: on a Quest, the Meta
    /// button held). Every pose after one is in the new space.
    pub recenters: u32,
}

/// Numbers in one [`XrInput`] on the C ABI ([`XrInput::from_floats`]).
pub const XR_INPUT_FLOATS: usize = 8 + 2 * HAND_FLOATS + 1;
const HAND_FLOATS: usize = 2 + 7 + 7 + 2 + 2 + 1;

impl XrInput {
    /// Whether every number is finite and in range: what a host may deliver.
    pub fn valid(&self) -> bool {
        self.head.finite()
            && self.hands.iter().all(|h| {
                h.aim.finite()
                    && h.grip.finite()
                    && (0.0..=1.0).contains(&h.trigger)
                    && (0.0..=1.0).contains(&h.squeeze)
                    && h.stick.iter().all(|v| (-1.0..=1.0).contains(v))
                    && h.buttons & !button::ALL == 0
            })
    }

    /// The input a host in another artifact sent as [`XR_INPUT_FLOATS`]
    /// numbers, in this order (a flag is 0 or 1, a pose its position then
    /// its orientation): the head's tracked flag and pose; per hand (left,
    /// right) active, tracked, aim, grip, trigger, squeeze, the stick's x and
    /// y, and the buttons' bits; then the recenter count. None for another
    /// length, or a flag or count that is not one.
    pub fn from_floats(floats: &[f32]) -> Option<Self> {
        if floats.len() != XR_INPUT_FLOATS {
            return None;
        }
        let flag = |v: f32| match v {
            0.0 => Some(false),
            1.0 => Some(true),
            _ => None,
        };
        let count =
            |v: f32| (v >= 0.0 && v.fract() == 0.0 && v <= 16_777_216.0).then_some(v as u32);
        let pose = |p: &[f32]| XrPose {
            position: [p[0], p[1], p[2]],
            orientation: [p[3], p[4], p[5], p[6]],
        };
        let mut hands = [XrHand::default(); 2];
        for (hand, h) in hands
            .iter_mut()
            .zip(floats[8..].as_chunks::<HAND_FLOATS>().0)
        {
            *hand = XrHand {
                active: flag(h[0])?,
                tracked: flag(h[1])?,
                aim: pose(&h[2..9]),
                grip: pose(&h[9..16]),
                trigger: h[16],
                squeeze: h[17],
                stick: [h[18], h[19]],
                buttons: count(h[20])?,
            };
        }
        Some(Self {
            head_tracked: flag(floats[0])?,
            head: pose(&floats[1..8]),
            hands,
            recenters: count(floats[XR_INPUT_FLOATS - 1])?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_abi_numbers_read_back_in_their_order() {
        let mut floats = vec![0.0; XR_INPUT_FLOATS];
        floats[0] = 1.0;
        floats[1..8].copy_from_slice(&[0.0, 1.6, 0.0, 0.0, 0.0, 0.0, 1.0]);
        let right = 8 + HAND_FLOATS;
        floats[right] = 1.0;
        floats[right + 1] = 1.0;
        floats[right + 2..right + 9].copy_from_slice(&[0.2, 1.2, -0.3, 0.0, 0.0, 0.0, 1.0]);
        floats[right + 16] = 0.75;
        floats[right + 19] = -1.0;
        floats[right + 20] = (button::TRIGGER | button::PRIMARY) as f32;
        floats[XR_INPUT_FLOATS - 1] = 2.0;
        let input = XrInput::from_floats(&floats).unwrap();
        assert!(input.head_tracked && input.valid());
        assert_eq!(input.head.position, [0.0, 1.6, 0.0]);
        assert!(!input.hands[0].active);
        let r = input.hands[1];
        assert!(r.active && r.tracked);
        assert_eq!(r.aim.position, [0.2, 1.2, -0.3]);
        assert_eq!((r.trigger, r.stick), (0.75, [0.0, -1.0]));
        assert_eq!(r.buttons, button::TRIGGER | button::PRIMARY);
        assert_eq!(input.recenters, 2);

        floats[0] = 0.5;
        assert_eq!(XrInput::from_floats(&floats), None, "a flag is 0 or 1");
        assert_eq!(XrInput::from_floats(&floats[1..]), None);
    }
}
