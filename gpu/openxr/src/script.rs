//! A headset's controllers (`docs/quest.md`, "Controllers"): the pieces that
//! need no headset. Each headset frame the session reads its actions into one
//! [`XrInput`] (the head, and each hand's aim and grip poses, trigger,
//! squeeze, thumbstick and buttons, in the session's reference space) and
//! hands it to the GPU module as numbers ([`XrInput::floats`], the module's
//! `gpu_input_xr`), which delivers it to the canvas as `InputEvent::Xr`.
//! These types mirror `exact_gpu::XrInput`'s, which this artifact does not
//! link.
//!
//! [`Injector`] is the debug stand-in for hands nobody has: a script file
//! (`xr-input`, beside the capture trigger) whose lines pose a hand, press
//! its buttons and wait, so a flow can be driven and looked at with the eye
//! capture while the headset lies on a desk.

use std::collections::VecDeque;

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
    /// At `position`, pointing (its −Z) along `forward` with its +Y toward
    /// `up` (any vector not along `forward`; +Y, else +Z, where it is).
    pub fn looking(position: [f32; 3], forward: [f32; 3], up: [f32; 3]) -> Self {
        let f = normalize(forward).unwrap_or([0.0, 0.0, -1.0]);
        let right = [up, [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]
            .into_iter()
            .find_map(|u| normalize(cross(f, u)))
            .unwrap_or([1.0, 0.0, 0.0]);
        let u = cross(right, f);
        // Columns: +X right, +Y up, +Z back.
        Self {
            position,
            orientation: quat_from_axes(right, u, [-f[0], -f[1], -f[2]]),
        }
    }

    /// The direction the pose points: its −Z.
    pub fn forward(&self) -> [f32; 3] {
        let [x, y, z, w] = self.orientation;
        [
            -2.0 * (x * z + y * w),
            -2.0 * (y * z - x * w),
            -(1.0 - 2.0 * (x * x + y * y)),
        ]
    }

    fn finite(&self) -> bool {
        self.position
            .iter()
            .chain(&self.orientation)
            .all(|v| v.is_finite())
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (length.is_finite() && length > 1e-6).then(|| [v[0] / length, v[1] / length, v[2] / length])
}

/// The unit quaternion of the rotation whose columns are `x`, `y`, `z`
/// (orthonormal, right-handed).
fn quat_from_axes(x: [f32; 3], y: [f32; 3], z: [f32; 3]) -> [f32; 4] {
    let (m00, m11, m22) = (x[0], y[1], z[2]);
    let trace = m00 + m11 + m22;
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            (y[2] - z[1]) / s,
            (z[0] - x[2]) / s,
            (x[1] - y[0]) / s,
            0.25 * s,
        ]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [
            0.25 * s,
            (y[0] + x[1]) / s,
            (z[0] + x[2]) / s,
            (y[2] - z[1]) / s,
        ]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [
            (y[0] + x[1]) / s,
            0.25 * s,
            (z[1] + y[2]) / s,
            (z[0] - x[2]) / s,
        ]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [
            (z[0] + x[2]) / s,
            (z[1] + y[2]) / s,
            0.25 * s,
            (x[1] - y[0]) / s,
        ]
    };
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    q.map(|v| v / n)
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

    /// The numbers the GPU module's `gpu_input_xr` takes, in its order
    /// (`exact_gpu::XrInput::from_floats`): the head's tracked flag and pose
    /// (position, then orientation); per hand active, tracked, aim, grip,
    /// trigger, squeeze, the stick's x and y, and the buttons' bits; then the
    /// recenter count.
    pub fn floats(&self) -> Vec<f32> {
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let pose = |p: &XrPose| p.position.into_iter().chain(p.orientation);
        let mut out = vec![flag(self.head_tracked)];
        out.extend(pose(&self.head));
        for h in &self.hands {
            out.extend([flag(h.active), flag(h.tracked)]);
            out.extend(pose(&h.aim).chain(pose(&h.grip)));
            out.extend([
                h.trigger,
                h.squeeze,
                h.stick[0],
                h.stick[1],
                h.buttons as f32,
            ]);
        }
        out.push(self.recenters as f32);
        out
    }
}

// ---------------------------------------------------------------- injection

/// The script file's name in the app's external files directory.
pub const INJECT_FILE: &str = "xr-input";
/// Frames a `click` holds its button, and waits after releasing it: long
/// enough for a 60 Hz world under a 72 Hz headset to see each edge.
pub const CLICK_FRAMES: u32 = 8;

#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// Replace a hand's aim and grip, or one of them.
    Pose {
        hand: usize,
        aim: Option<XrPose>,
        grip: Option<XrPose>,
    },
    Head(XrPose),
    Button {
        hand: usize,
        bit: u32,
        down: bool,
    },
    Stick {
        hand: usize,
        stick: [f32; 2],
    },
    /// Give the hand (0, 1), the head (2) or all of them (3) back to tracking.
    Release(usize),
    Wait(u32),
    Message(String),
    /// Count a recenter, as the runtime's event does.
    Recenter,
}

/// Scripted hands and head that stand in for the tracked ones
/// (`docs/quest.md`, "Driving the controllers without hands"). A hand the
/// script posed or pressed stays the script's, held where it was left, until
/// the script releases it.
#[derive(Debug, Default)]
pub struct Injector {
    steps: VecDeque<Step>,
    waiting: u32,
    hands: [Option<XrHand>; 2],
    head: Option<XrPose>,
    /// Recenters the script counted, added to the runtime's.
    recenters: u32,
}

impl Injector {
    /// Queue a script's steps after those still pending. One step a line
    /// (or between `;`), `#` to the end of a line a comment:
    ///
    /// - `<hand> aim <ox oy oz> <dx dy dz>`: the hand (`left`, `right`) at
    ///   the origin, pointing along the direction; its grip there too.
    /// - `<hand> point <ox oy oz> <tx ty tz>`: the same, pointing at a target.
    /// - `<hand> grip <px py pz> <fx fy fz> [<ux uy uz>]`: the grip alone, its
    ///   −Z along `f` and its +Y toward `u`.
    /// - `<hand> <button> down|up`, `<hand> click <button>`: `trigger`,
    ///   `squeeze`, `a`/`x`/`primary`, `b`/`y`/`secondary`, `menu`, `stick`.
    /// - `<hand> stick <x> <y>`: the thumbstick.
    /// - `head <px py pz> <fx fy fz>`: the head's pose as the canvas hears it
    ///   (the eyes still draw from the tracked head, or a capture's pose).
    /// - `<hand> release`, `head release`, `release`: back to tracking.
    /// - `wait <frames>`, `message <text>` (a posted message, as Contract's
    ///   `postMessage`).
    /// - `recenter`: count a recenter ([`XrInput::recenters`]), as the
    ///   runtime does when the person holds the Meta button; the poses stay
    ///   where they are (script a `head` first to stand somewhere else).
    pub fn script(&mut self, text: &str) -> Result<(), String> {
        let mut steps = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("");
            for part in line.split(';') {
                if part.trim().is_empty() {
                    continue;
                }
                parse_step(part.trim(), &mut steps).map_err(|e| format!("line {}: {e}", n + 1))?;
            }
        }
        self.steps.extend(steps);
        Ok(())
    }

    /// Whether anything is scripted: a pending step or a held hand or head.
    pub fn active(&self) -> bool {
        !self.steps.is_empty() || self.hands.iter().any(Option::is_some) || self.head.is_some()
    }

    /// One frame: run the steps up to the next wait, lay the scripted hands
    /// and head over `input`, and return the messages to post.
    pub fn frame(&mut self, input: &mut XrInput) -> Vec<String> {
        let mut messages = Vec::new();
        if self.waiting > 0 {
            self.waiting -= 1;
        }
        while self.waiting == 0 {
            let Some(step) = self.steps.pop_front() else {
                break;
            };
            match step {
                Step::Pose { hand, aim, grip } => {
                    let h = self.hand(hand, input);
                    if let Some(aim) = aim {
                        h.aim = aim;
                    }
                    if let Some(grip) = grip {
                        h.grip = grip;
                    }
                }
                Step::Head(pose) => self.head = Some(pose),
                Step::Button { hand, bit, down } => {
                    let h = self.hand(hand, input);
                    h.buttons = if down {
                        h.buttons | bit
                    } else {
                        h.buttons & !bit
                    };
                    if bit == button::TRIGGER {
                        h.trigger = f32::from(u8::from(down));
                    }
                    if bit == button::SQUEEZE {
                        h.squeeze = f32::from(u8::from(down));
                    }
                }
                Step::Stick { hand, stick } => self.hand(hand, input).stick = stick,
                Step::Release(which) => {
                    for hand in 0..2 {
                        if which == hand || which == 3 {
                            self.hands[hand] = None;
                        }
                    }
                    if which >= 2 {
                        self.head = None;
                    }
                }
                Step::Wait(frames) => self.waiting = frames,
                Step::Message(text) => messages.push(text),
                Step::Recenter => self.recenters += 1,
            }
        }
        input.recenters = input.recenters.wrapping_add(self.recenters);
        for (slot, scripted) in input.hands.iter_mut().zip(&self.hands) {
            if let Some(hand) = scripted {
                *slot = *hand;
            }
        }
        if let Some(head) = self.head {
            input.head = head;
            input.head_tracked = true;
        }
        messages
    }

    /// The scripted hand, starting from the tracked one's state.
    fn hand(&mut self, hand: usize, input: &XrInput) -> &mut XrHand {
        self.hands[hand].get_or_insert_with(|| XrHand {
            active: true,
            tracked: true,
            buttons: 0,
            trigger: 0.0,
            squeeze: 0.0,
            stick: [0.0; 2],
            ..input.hands[hand]
        })
    }
}

fn parse_step(text: &str, steps: &mut Vec<Step>) -> Result<(), String> {
    let mut words = text.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    let numbers = |from: usize| -> Result<Vec<f32>, String> {
        rest[from.min(rest.len())..]
            .iter()
            .map(|w| match w.parse::<f32>() {
                Ok(v) if v.is_finite() => Ok(v),
                _ => Err(format!("`{w}` is not a number")),
            })
            .collect()
    };
    let three = |n: &[f32], i: usize| [n[i], n[i + 1], n[i + 2]];
    match first {
        "wait" => {
            let frames = rest
                .first()
                .and_then(|w| w.parse::<u32>().ok())
                .filter(|_| rest.len() == 1)
                .ok_or("wait takes a count of frames")?;
            steps.push(Step::Wait(frames));
        }
        "message" => {
            let at = text.find("message").unwrap_or(0) + "message".len();
            let body = text[at..].trim();
            if body.is_empty() {
                return Err("message takes its text".into());
            }
            steps.push(Step::Message(body.to_owned()));
        }
        "release" if rest.is_empty() => steps.push(Step::Release(3)),
        "recenter" if rest.is_empty() => steps.push(Step::Recenter),
        "head" => match rest.first() {
            Some(&"release") if rest.len() == 1 => steps.push(Step::Release(2)),
            _ => {
                let n = numbers(0)?;
                if n.len() != 6 {
                    return Err("head takes a position and a forward direction".into());
                }
                steps.push(Step::Head(XrPose::looking(
                    three(&n, 0),
                    three(&n, 3),
                    [0.0, 1.0, 0.0],
                )));
            }
        },
        "left" | "right" => {
            let hand = usize::from(first == "right");
            let verb = *rest.first().ok_or("a hand takes a verb")?;
            match verb {
                "aim" | "point" => {
                    let n = numbers(1)?;
                    if n.len() != 6 {
                        return Err(format!(
                            "{verb} takes an origin and a {}",
                            if verb == "aim" { "direction" } else { "target" }
                        ));
                    }
                    let (origin, mut toward) = (three(&n, 0), three(&n, 3));
                    if verb == "point" {
                        toward = [
                            toward[0] - origin[0],
                            toward[1] - origin[1],
                            toward[2] - origin[2],
                        ];
                    }
                    if normalize(toward).is_none() {
                        return Err(format!("{verb}: no direction"));
                    }
                    let pose = XrPose::looking(origin, toward, [0.0, 1.0, 0.0]);
                    steps.push(Step::Pose {
                        hand,
                        aim: Some(pose),
                        grip: Some(pose),
                    });
                }
                "grip" => {
                    let n = numbers(1)?;
                    if n.len() != 6 && n.len() != 9 {
                        return Err(
                            "grip takes a position, a forward direction and maybe an up direction"
                                .into(),
                        );
                    }
                    let up = if n.len() == 9 {
                        three(&n, 6)
                    } else {
                        [0.0, 1.0, 0.0]
                    };
                    let pose = XrPose::looking(three(&n, 0), three(&n, 3), up);
                    steps.push(Step::Pose {
                        hand,
                        aim: None,
                        grip: Some(pose),
                    });
                }
                "stick" if rest.len() == 3 => {
                    let n = numbers(1)?;
                    if n.iter().any(|v| !(-1.0..=1.0).contains(v)) {
                        return Err("the thumbstick is within -1..1".into());
                    }
                    steps.push(Step::Stick {
                        hand,
                        stick: [n[0], n[1]],
                    });
                }
                "release" if rest.len() == 1 => steps.push(Step::Release(hand)),
                "click" => {
                    let bit = rest
                        .get(1)
                        .and_then(|w| button_bit(w))
                        .filter(|_| rest.len() == 2)
                        .ok_or("click takes a button")?;
                    steps.extend([
                        Step::Button {
                            hand,
                            bit,
                            down: true,
                        },
                        Step::Wait(CLICK_FRAMES),
                        Step::Button {
                            hand,
                            bit,
                            down: false,
                        },
                        Step::Wait(CLICK_FRAMES),
                    ]);
                }
                name => {
                    let bit = button_bit(name).ok_or_else(|| format!("unknown `{name}`"))?;
                    let down = match rest.get(1).copied() {
                        Some("down") | Some("1") => true,
                        Some("up") | Some("0") => false,
                        _ => return Err(format!("{name} takes down or up")),
                    };
                    if rest.len() != 2 {
                        return Err(format!("{name} takes down or up"));
                    }
                    steps.push(Step::Button { hand, bit, down });
                }
            }
        }
        other => return Err(format!("unknown `{other}`")),
    }
    Ok(())
}

fn button_bit(name: &str) -> Option<u32> {
    Some(match name {
        "trigger" => button::TRIGGER,
        "squeeze" => button::SQUEEZE,
        "a" | "x" | "primary" => button::PRIMARY,
        "b" | "y" | "secondary" => button::SECONDARY,
        "menu" => button::MENU,
        "stick" => button::STICK,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn a_pose_points_where_it_was_told() {
        for forward in [
            [0.0, 0.0, -1.0],
            [0.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.3, -0.7, -0.4],
            [0.0, 0.0, 1.0],
        ] {
            let pose = XrPose::looking([1.0, 2.0, 3.0], forward, [0.0, 1.0, 0.0]);
            let n = normalize(forward).unwrap();
            assert!(
                close(pose.forward(), n),
                "{forward:?}: {:?}",
                pose.forward()
            );
            let q = pose.orientation;
            assert!((q.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-5);
        }
        assert_eq!(XrPose::default().forward(), [0.0, 0.0, -1.0]);
        // The view of a pose puts what is ahead of it on −Z.
        let pose = XrPose::looking([0.0, 1.0, 0.0], [0.0, -1.0, -1.0], [0.0, 1.0, 0.0]);
        let view = crate::pose::pose_view(pose.orientation, pose.position);
        let ahead = [0.0, 0.0, -1.0f32];
        let z = view[2] * ahead[0] + view[6] * ahead[1] + view[10] * ahead[2] + view[14];
        assert!(z < 0.0);
    }

    #[test]
    fn a_script_poses_presses_and_waits() {
        let mut inject = Injector::default();
        inject
            .script("right point 0.2 1.2 -0.2  0 0.85 -0.9 # at the table\nright click trigger; wait 3\nmessage wave\nright release")
            .unwrap();
        let mut frames = Vec::new();
        let mut posted = Vec::new();
        for _ in 0..(2 * CLICK_FRAMES + 8) {
            let mut input = XrInput::default();
            posted.extend(inject.frame(&mut input));
            assert!(input.valid());
            frames.push(input.hands[1]);
        }
        // Posed and pressed at once, held for a click, released, then gone.
        assert!(frames[0].active && frames[0].tracked);
        assert!(close(frames[0].aim.position, [0.2, 1.2, -0.2]));
        let toward = normalize([-0.2, -0.35, -0.7]).unwrap();
        assert!(close(frames[0].aim.forward(), toward));
        let held = frames
            .iter()
            .filter(|h| h.buttons & button::TRIGGER != 0)
            .count();
        assert_eq!(held, CLICK_FRAMES as usize);
        assert_eq!(frames[0].trigger, 1.0);
        assert_eq!(posted, ["wave"]);
        assert!(!frames.last().unwrap().active, "released back to tracking");
        assert!(!inject.active());
        // The left hand was never the script's.
        let mut input = XrInput::default();
        input.hands[0].stick = [0.5, 0.0];
        inject.frame(&mut input);
        assert_eq!(input.hands[0].stick, [0.5, 0.0]);
    }

    #[test]
    fn a_script_names_what_it_refuses() {
        let mut inject = Injector::default();
        for (text, word) in [
            ("right aim 0 0 0", "origin"),
            ("right aim 0 0 0 0 0 0", "no direction"),
            ("up trigger down", "unknown `up`"),
            ("left jump down", "unknown `jump`"),
            ("left trigger sideways", "down or up"),
            ("wait soon", "frames"),
            ("right stick 2 0", "-1..1"),
            ("head 0 1 0", "position"),
        ] {
            let e = inject.script(text).unwrap_err();
            assert!(e.contains(word), "{text}: {e}");
        }
        assert!(!inject.active(), "a refused script queues nothing");
        inject
            .script("left grip 0 1 -0.5  0 -1 0  -1 0 0\nhead 0 1.6 0 0 -0.5 -1\nleft y down")
            .unwrap();
        let mut input = XrInput::default();
        inject.frame(&mut input);
        assert!(input.head_tracked && close(input.head.position, [0.0, 1.6, 0.0]));
        assert!(close(input.hands[0].grip.forward(), [0.0, -1.0, 0.0]));
        assert_eq!(input.hands[0].buttons, button::SECONDARY);
        inject.script("release").unwrap();
        let mut input = XrInput::default();
        inject.frame(&mut input);
        assert_eq!(input, XrInput::default());
    }

    #[test]
    fn a_scripted_recenter_is_counted_on_top_of_the_runtimes() {
        let mut inject = Injector::default();
        inject
            .script("head 0.4 1.2 0.3  1 0 0; recenter; wait 2; recenter")
            .unwrap();
        let mut input = XrInput {
            recenters: 3,
            ..XrInput::default()
        };
        inject.frame(&mut input);
        assert_eq!(input.recenters, 4);
        assert!(close(input.head.position, [0.4, 1.2, 0.3]));
        // The count stands on later frames, and the next adds to it.
        let mut input = XrInput {
            recenters: 3,
            ..XrInput::default()
        };
        inject.frame(&mut input);
        assert_eq!(input.recenters, 4);
        let mut input = XrInput {
            recenters: 3,
            ..XrInput::default()
        };
        inject.frame(&mut input);
        assert_eq!(input.recenters, 5);
        assert!(inject.script("recenter now").is_err());
    }

    #[test]
    fn the_numbers_for_the_module_follow_its_order() {
        let mut input = XrInput {
            head_tracked: true,
            recenters: 3,
            ..XrInput::default()
        };
        input.hands[1].active = true;
        input.hands[1].aim.position = [0.2, 1.2, -0.3];
        input.hands[1].trigger = 0.75;
        input.hands[1].buttons = button::TRIGGER | button::PRIMARY;
        let floats = input.floats();
        // `exact_gpu::XR_INPUT_FLOATS`.
        assert_eq!(floats.len(), 51);
        assert_eq!(floats[0], 1.0);
        // The right hand from 29: active, tracked, then its aim.
        assert_eq!(floats[29], 1.0);
        assert_eq!(&floats[31..34], &[0.2, 1.2, -0.3]);
        assert_eq!(floats[45], 0.75);
        assert_eq!(floats[49], 5.0);
        assert_eq!(floats[50], 3.0);
    }
}
