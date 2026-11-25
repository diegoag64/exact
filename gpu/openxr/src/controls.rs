//! The headset's controllers (`docs/quest.md`, "Controllers"): one OpenXR
//! action set read once a frame into an [`XrInput`], which the frame loop
//! delivers to the canvas as an input event. Bindings are suggested for the
//! Touch controllers and for the Khronos simple controller, so any runtime
//! with one or the other answers. Where the instance has Touch Plus (a PC's
//! OpenXR 1.1 instance, or `XR_META_touch_controller_plus`), the same
//! bindings are suggested for it first; the runtime picks the profile of the
//! controllers it has (Virtual Desktop always reports Touch).
//!
//! The debug script file (`xr-input`, [`crate::script::Injector`]) stands
//! in for hands nobody has.

use crate::platform::log;
use crate::script::{button, Injector, XrInput, XrPose, INJECT_FILE};
use openxr as xr;
use std::path::PathBuf;
use std::time::SystemTime;

/// Frames between looks for the script file: about six times a second.
const INJECT_POLL_FRAMES: u32 = 12;

pub(crate) struct Controls {
    set: xr::ActionSet,
    /// `/user/hand/left`, `/user/hand/right`.
    hands: [xr::Path; 2],
    aim: [xr::Space; 2],
    grip: [xr::Space; 2],
    /// The head: the `VIEW` reference space.
    head: xr::Space,
    pose_actions: [xr::Action<xr::Posef>; 2],
    trigger: xr::Action<f32>,
    trigger_down: xr::Action<bool>,
    squeeze: xr::Action<f32>,
    squeeze_down: xr::Action<bool>,
    stick: xr::Action<xr::Vector2f>,
    stick_down: xr::Action<bool>,
    primary: xr::Action<bool>,
    secondary: xr::Action<bool>,
    menu: xr::Action<bool>,
    /// What the tracked devices last read (poses hold while untracked).
    tracked: XrInput,
    /// What was last delivered: an unchanged frame is not an event.
    sent: Option<XrInput>,
    inject: Injector,
    inject_dir: Option<Option<PathBuf>>,
    inject_seen: Option<(SystemTime, u64)>,
    since_poll: u32,
    /// Which hands had a controller at the last frame, for the log.
    active: [bool; 2],
}

fn pose(p: xr::Posef) -> XrPose {
    XrPose {
        position: [p.position.x, p.position.y, p.position.z],
        orientation: [
            p.orientation.x,
            p.orientation.y,
            p.orientation.z,
            p.orientation.w,
        ],
    }
}

impl Controls {
    /// Make the action set, suggest its bindings and attach it to `session`.
    pub(crate) fn new<G>(
        instance: &xr::Instance,
        session: &xr::Session<G>,
        touch_plus: Option<&'static str>,
    ) -> Result<Self, String> {
        let path = |text: &str| {
            instance
                .string_to_path(text)
                .map_err(|e| format!("{text}: {e}"))
        };
        let hands = [path("/user/hand/left")?, path("/user/hand/right")?];
        let set = instance
            .create_action_set("exact", "Exact", 0)
            .map_err(|e| format!("action set: {e}"))?;
        macro_rules! action {
            ($ty:ty, $name:literal, $shown:literal) => {
                set.create_action::<$ty>($name, $shown, &hands)
                    .map_err(|e| format!(concat!("action ", $name, ": {}"), e))?
            };
        }
        let aim = action!(xr::Posef, "aim", "Aim");
        let grip = action!(xr::Posef, "grip", "Grip");
        let trigger = action!(f32, "trigger", "Trigger");
        let trigger_down = action!(bool, "trigger_down", "Trigger pressed");
        let squeeze = action!(f32, "squeeze", "Grip button");
        let squeeze_down = action!(bool, "squeeze_down", "Grip button pressed");
        let stick = action!(xr::Vector2f, "thumbstick", "Thumbstick");
        let stick_down = action!(bool, "thumbstick_down", "Thumbstick pressed");
        let primary = action!(bool, "primary", "A or X");
        let secondary = action!(bool, "secondary", "B or Y");
        let menu = action!(bool, "menu", "Menu");

        // Each profile's bindings; a runtime that refuses one keeps the other.
        let mut suggested = Vec::new();
        {
            let both = |suffix: &str| -> Result<[xr::Path; 2], String> {
                Ok([
                    path(&format!("/user/hand/left/input/{suffix}"))?,
                    path(&format!("/user/hand/right/input/{suffix}"))?,
                ])
            };
            let mut touch = Vec::new();
            for p in both("aim/pose")? {
                touch.push(xr::Binding::new(&aim, p));
            }
            for p in both("grip/pose")? {
                touch.push(xr::Binding::new(&grip, p));
            }
            for p in both("trigger/value")? {
                touch.push(xr::Binding::new(&trigger, p));
                touch.push(xr::Binding::new(&trigger_down, p));
            }
            for p in both("squeeze/value")? {
                touch.push(xr::Binding::new(&squeeze, p));
                touch.push(xr::Binding::new(&squeeze_down, p));
            }
            for p in both("thumbstick")? {
                touch.push(xr::Binding::new(&stick, p));
            }
            for p in both("thumbstick/click")? {
                touch.push(xr::Binding::new(&stick_down, p));
            }
            touch.push(xr::Binding::new(
                &primary,
                path("/user/hand/left/input/x/click")?,
            ));
            touch.push(xr::Binding::new(
                &primary,
                path("/user/hand/right/input/a/click")?,
            ));
            touch.push(xr::Binding::new(
                &secondary,
                path("/user/hand/left/input/y/click")?,
            ));
            touch.push(xr::Binding::new(
                &secondary,
                path("/user/hand/right/input/b/click")?,
            ));
            touch.push(xr::Binding::new(
                &menu,
                path("/user/hand/left/input/menu/click")?,
            ));
            // Touch Plus takes the same inputs, by the same paths.
            if let Some(profile) = touch_plus {
                match instance.suggest_interaction_profile_bindings(path(profile)?, &touch) {
                    Ok(()) => suggested.push(profile),
                    Err(e) => log(true, &format!("controllers: {profile} refused: {e}")),
                }
            }
            let profile = "/interaction_profiles/oculus/touch_controller";
            match instance.suggest_interaction_profile_bindings(path(profile)?, &touch) {
                Ok(()) => suggested.push(profile),
                Err(e) => log(true, &format!("controllers: {profile} refused: {e}")),
            }

            let mut simple = Vec::new();
            for p in both("aim/pose")? {
                simple.push(xr::Binding::new(&aim, p));
            }
            for p in both("grip/pose")? {
                simple.push(xr::Binding::new(&grip, p));
            }
            for p in both("select/click")? {
                simple.push(xr::Binding::new(&trigger, p));
                simple.push(xr::Binding::new(&trigger_down, p));
            }
            for p in both("menu/click")? {
                simple.push(xr::Binding::new(&menu, p));
            }
            let profile = "/interaction_profiles/khr/simple_controller";
            match instance.suggest_interaction_profile_bindings(path(profile)?, &simple) {
                Ok(()) => suggested.push(profile),
                Err(e) => log(true, &format!("controllers: {profile} refused: {e}")),
            }
        }
        if suggested.is_empty() {
            return Err("no interaction profile took the bindings".into());
        }
        session
            .attach_action_sets(&[&set])
            .map_err(|e| format!("attach: {e}"))?;
        let space = |action: &xr::Action<xr::Posef>, hand: xr::Path| {
            action
                .create_space(session, hand, xr::Posef::IDENTITY)
                .map_err(|e| format!("action space: {e}"))
        };
        let aim_spaces = [space(&aim, hands[0])?, space(&aim, hands[1])?];
        let grip_spaces = [space(&grip, hands[0])?, space(&grip, hands[1])?];
        let head = session
            .create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)
            .map_err(|e| format!("view space: {e}"))?;
        log(
            false,
            &format!("controllers: actions attached ({})", suggested.join(", ")),
        );
        Ok(Self {
            set,
            hands,
            aim: aim_spaces,
            grip: grip_spaces,
            head,
            pose_actions: [aim, grip],
            trigger,
            trigger_down,
            squeeze,
            squeeze_down,
            stick,
            stick_down,
            primary,
            secondary,
            menu,
            tracked: XrInput::default(),
            sent: None,
            inject: Injector::default(),
            inject_dir: None,
            inject_seen: None,
            since_poll: 0,
            active: [false; 2],
        })
    }

    /// The runtime changed a hand's interaction profile: say which, once.
    pub(crate) fn profile_changed<G>(&self, instance: &xr::Instance, session: &xr::Session<G>) {
        for (hand, name) in self.hands.iter().zip(["left", "right"]) {
            let profile = session
                .current_interaction_profile(*hand)
                .ok()
                .filter(|p| *p != xr::Path::NULL)
                .and_then(|p| instance.path_to_string(p).ok())
                .unwrap_or_else(|| "none".into());
            log(false, &format!("controllers: {name} hand {profile}"));
        }
    }

    /// Look for the script file every [`INJECT_POLL_FRAMES`] frames; `dir`
    /// finds the app's external files directory, once.
    fn poll_script(&mut self, dir: impl FnOnce() -> Option<PathBuf>) {
        self.since_poll += 1;
        if self.since_poll < INJECT_POLL_FRAMES {
            return;
        }
        self.since_poll = 0;
        let Some(dir) = self.inject_dir.get_or_insert_with(dir).clone() else {
            return;
        };
        let file = dir.join(INJECT_FILE);
        let Ok(meta) = std::fs::metadata(&file) else {
            self.inject_seen = None;
            return;
        };
        let stamp = (
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
        );
        if self.inject_seen == Some(stamp) {
            return;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if std::fs::remove_file(&file).is_err() {
            self.inject_seen = Some(stamp);
        }
        match self.inject.script(&text) {
            Ok(()) => log(
                false,
                &format!("xr-input: {} lines taken", text.lines().count()),
            ),
            Err(e) => log(true, &format!("xr-input: {e}")),
        }
    }

    /// The runtime moved the reference space under the person (a recenter):
    /// counted, so the canvas hears that every pose from here is in the new
    /// space ([`XrInput::recenters`]).
    pub(crate) fn recentered(&mut self) {
        self.tracked.recenters = self.tracked.recenters.wrapping_add(1);
    }

    /// One frame: sync the actions, locate the poses in `base` at `time`,
    /// run the script, and return the frame's input when it differs from
    /// what was last delivered, with the messages the script posted.
    pub(crate) fn frame<G>(
        &mut self,
        session: &xr::Session<G>,
        base: &xr::Space,
        time: xr::Time,
        dir: impl FnOnce() -> Option<PathBuf>,
    ) -> (Option<XrInput>, Vec<String>) {
        self.poll_script(dir);
        // Not focused, the sync succeeds and every action reads inactive.
        if let Err(e) = session.sync_actions(&[xr::ActiveActionSet::new(&self.set)]) {
            if self.active != [false; 2] {
                log(true, &format!("controllers: sync: {e}"));
            }
        }
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        let mut input = self.tracked;
        match self.head.locate(base, time) {
            Ok(at) if at.location_flags.contains(valid) => {
                input.head_tracked = true;
                input.head = pose(at.pose);
            }
            _ => input.head_tracked = false,
        }
        for (i, hand) in self.hands.iter().enumerate() {
            let slot = &mut input.hands[i];
            let poses = self
                .pose_actions
                .iter()
                .any(|a| a.is_active(session, *hand).unwrap_or(false));
            let boolean = |action: &xr::Action<bool>| {
                action
                    .state(session, *hand)
                    .is_ok_and(|s| s.is_active && s.current_state)
            };
            let float = |action: &xr::Action<f32>| {
                action
                    .state(session, *hand)
                    .ok()
                    .filter(|s| s.is_active && s.current_state.is_finite())
                    .map_or(0.0, |s| s.current_state.clamp(0.0, 1.0))
            };
            slot.active = poses;
            slot.trigger = float(&self.trigger);
            slot.squeeze = float(&self.squeeze);
            slot.stick = self
                .stick
                .state(session, *hand)
                .ok()
                .filter(|s| {
                    s.is_active && s.current_state.x.is_finite() && s.current_state.y.is_finite()
                })
                .map_or([0.0; 2], |s| {
                    [
                        s.current_state.x.clamp(-1.0, 1.0),
                        s.current_state.y.clamp(-1.0, 1.0),
                    ]
                });
            slot.buttons = [
                (&self.trigger_down, button::TRIGGER),
                (&self.squeeze_down, button::SQUEEZE),
                (&self.primary, button::PRIMARY),
                (&self.secondary, button::SECONDARY),
                (&self.menu, button::MENU),
                (&self.stick_down, button::STICK),
            ]
            .into_iter()
            .filter(|(action, _)| boolean(action))
            .fold(0, |bits, (_, bit)| bits | bit);
            let located = |space: &xr::Space| {
                space
                    .locate(base, time)
                    .ok()
                    .filter(|at| at.location_flags.contains(valid))
                    .map(|at| pose(at.pose))
            };
            match (
                poses.then(|| located(&self.aim[i])).flatten(),
                poses.then(|| located(&self.grip[i])).flatten(),
            ) {
                (Some(aim), Some(grip)) => {
                    slot.tracked = true;
                    slot.aim = aim;
                    slot.grip = grip;
                }
                _ => slot.tracked = false,
            }
            if self.active[i] != slot.active {
                self.active[i] = slot.active;
                let name = ["left", "right"][i];
                log(
                    false,
                    &format!(
                        "controllers: {name} hand {}",
                        if slot.active { "active" } else { "inactive" }
                    ),
                );
            }
        }
        self.tracked = input;
        let messages = self.inject.frame(&mut input);
        if !input.valid() {
            log(true, "controllers: a frame's input was not finite; dropped");
            return (None, messages);
        }
        let changed = self.sent != Some(input);
        if changed {
            self.sent = Some(input);
        }
        (changed.then_some(input), messages)
    }
}
