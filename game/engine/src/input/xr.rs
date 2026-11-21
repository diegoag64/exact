//! A headset's head and controllers (`docs/quest.md`, "Controllers"). A
//! headset host delivers one [`InputEvent::Xr`](crate::InputEvent::Xr) each
//! frame it draws: the whole state, in the session's reference space (metres,
//! +Y up, the person facing −Z where the session started). The latest stands
//! until the next, as a pad's does, and the buttons that went down or up
//! since the last tick are that tick's edges. A tick reads them from
//! [`Input::xr`], [`Input::xr_pressed`] and [`Input::xr_released`], and
//! places them in its world with [`crate::XrStage::to_world`].
//!
//! What the events leave behind is world state, a resource ([`XrInput`]), so
//! it saves and hashes; a world no headset ever reported to has none.
use super::Input;
use crate::{Data, Quat, Resource, Vec2, Vec3};

/// Which hand a controller is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Data)]
pub enum XrHand {
    /// The left hand.
    #[default]
    Left,
    /// The right hand.
    Right,
}
impl XrHand {
    /// Left, then right: the order of [`XrInput::hands`].
    pub const BOTH: [XrHand; 2] = [XrHand::Left, XrHand::Right];
    /// 0 for left, 1 for right.
    pub fn index(self) -> usize {
        self as usize
    }
    /// The other hand.
    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// A controller's buttons. The face buttons are named by place, not letter:
/// on Touch controllers the right hand's primary is A and secondary B, the
/// left hand's X and Y.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Data)]
pub enum XrButton {
    /// The index trigger, past the runtime's press threshold.
    #[default]
    Trigger,
    /// The grip (squeeze) button, past the runtime's press threshold.
    Squeeze,
    /// A (right) or X (left).
    Primary,
    /// B (right) or Y (left).
    Secondary,
    /// The menu button (the left controller's, on Touch).
    Menu,
    /// The thumbstick pressed in.
    Stick,
}
impl XrButton {
    /// Every button, in bit order.
    pub const ALL: [XrButton; 6] = [
        Self::Trigger,
        Self::Squeeze,
        Self::Primary,
        Self::Secondary,
        Self::Menu,
        Self::Stick,
    ];
    /// Its bit in [`XrController::buttons`].
    pub fn bit(self) -> u32 {
        1 << self as u32
    }
}
/// Every button bit a host may set.
const BUTTON_BITS: u32 = (1 << XrButton::ALL.len()) - 1;

/// A rigid pose in the session's reference space: it points along its own −Z
/// with +Y up, as a `Transform` does.
#[derive(Clone, Copy, Debug, PartialEq, Data)]
pub struct XrPose {
    /// Metres.
    pub position: Vec3,
    /// A unit quaternion.
    pub rotation: Quat,
}
impl Default for XrPose {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
        }
    }
}
impl XrPose {
    /// The direction it points: its −Z.
    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::NEG_Z
    }
    /// Its +Y.
    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::Y
    }
    /// Its +X.
    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }
    /// The ray it points: its position and [`XrPose::forward`].
    pub fn ray(&self) -> (Vec3, Vec3) {
        (self.position, self.forward())
    }
    /// A point given in the pose's own frame, in the reference space.
    pub fn point(&self, local: Vec3) -> Vec3 {
        self.position + self.rotation * local
    }
    fn finite(&self) -> bool {
        self.position.is_finite() && self.rotation.is_finite()
    }
}

/// One hand's controller as the host last reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Data)]
pub struct XrController {
    /// The runtime has a controller in this hand.
    pub active: bool,
    /// The poses are tracked; they keep their last values while not.
    pub tracked: bool,
    /// The pointing pose: aim a ray with [`XrPose::ray`].
    pub aim: XrPose,
    /// The holding pose, at the palm: draw a held thing here.
    pub grip: XrPose,
    /// The index trigger's travel in `[0, 1]`.
    pub trigger: f32,
    /// The grip button's travel in `[0, 1]`.
    pub squeeze: f32,
    /// The thumbstick in `[-1, 1]`: +x right, +y forward.
    pub stick: Vec2,
    /// Held buttons, by [`XrButton::bit`].
    pub buttons: u32,
}
impl XrController {
    /// Whether `button` is held.
    pub fn down(&self, button: XrButton) -> bool {
        self.buttons & button.bit() != 0
    }
}

/// The head and both controllers as the headset last reported them, in the
/// session's reference space. Input works on a copy; the world's is the one
/// that saves and hashes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Resource)]
pub struct XrInput {
    /// The head's pose is tracked.
    pub head_tracked: bool,
    /// The head (the midpoint of the eyes); its last pose while untracked.
    pub head: XrPose,
    /// Left, right ([`XrHand::index`]).
    pub hands: [XrController; 2],
    /// How many times the runtime has moved the reference space under the
    /// person since the session began: a recenter (on a Quest, the Meta
    /// button held). Poses after one are in the new space, so what a game
    /// placed about the person at the start is placed again when this grows.
    pub recenters: u32,
}
impl XrInput {
    /// One hand's controller.
    pub fn hand(&self, hand: XrHand) -> &XrController {
        &self.hands[hand.index()]
    }
    /// One hand's controller, to set in a test.
    pub fn hand_mut(&mut self, hand: XrHand) -> &mut XrController {
        &mut self.hands[hand.index()]
    }
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        let hands = self.hands.iter().all(|h| {
            h.aim.finite()
                && h.grip.finite()
                && (0.0..=1.0).contains(&h.trigger)
                && (0.0..=1.0).contains(&h.squeeze)
                && h.stick.is_finite()
                && h.stick.abs().max_element() <= 1.0
                && h.buttons & !BUTTON_BITS == 0
        });
        if !(hands && self.head.finite()) {
            return Err(
                "headset input needs finite poses, trigger and squeeze in [0, 1], a thumbstick in [-1, 1] and known buttons"
                    .into(),
            );
        }
        Ok(())
    }
    /// Whether `other` holds the same buttons with the same controllers
    /// present: only the poses and the analog travel moved.
    pub(crate) fn same_buttons(&self, other: &Self) -> bool {
        self.hands
            .iter()
            .zip(&other.hands)
            .all(|(a, b)| a.buttons == b.buttons && a.active == b.active)
    }
}

impl Input {
    /// The headset's head and controllers as last reported; the default
    /// (nothing active or tracked) where no headset reports.
    pub fn xr(&self) -> &XrInput {
        &self.xr
    }
    /// Whether `button` on `hand` went down since the last tick.
    pub fn xr_pressed(&self, hand: XrHand, button: XrButton) -> bool {
        self.xr_edges[hand.index()].0 & button.bit() != 0
    }
    /// Whether `button` on `hand` came up since the last tick.
    pub fn xr_released(&self, hand: XrHand, button: XrButton) -> bool {
        self.xr_edges[hand.index()].1 & button.bit() != 0
    }
    pub(crate) fn apply_xr(&mut self, mut next: XrInput) {
        next.head.rotation = next.head.rotation.normalize();
        for (i, hand) in next.hands.iter_mut().enumerate() {
            hand.aim.rotation = hand.aim.rotation.normalize();
            hand.grip.rotation = hand.grip.rotation.normalize();
            let before = self.xr.hands[i].buttons;
            self.xr_edges[i].0 |= hand.buttons & !before;
            self.xr_edges[i].1 |= before & !hand.buttons;
        }
        self.xr = next;
    }
    pub(crate) fn clear_xr_edges(&mut self) {
        self.xr_edges = [(0, 0); 2];
    }
    pub(crate) fn xr_state(&self) -> &XrInput {
        &self.xr
    }
    pub(crate) fn load_xr(&mut self, state: XrInput) {
        self.xr = state;
    }
}
