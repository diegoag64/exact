//! Where a world stands in an immersive (WebXR, OpenXR) session. A game opts
//! in with app.json's `game.xr` and places its world with [`Game::xr_stage`]
//! (`crate::Game`); the renderer then draws each eye from the headset's pose
//! and ignores the world's own camera. The engine saves and hashes nothing
//! here: a game that moves its stage (a hand dragging the board,
//! [`XrStage::dragged`], [`XrStage::pinched`]) keeps the placement in its own
//! world and answers `xr_stage` from it.
//!
//! The same placement takes a headset's controllers ([`crate::XrInput`], in
//! the session's reference space) into the world: [`XrStage::to_world`],
//! [`XrStage::ray_to_world`].

use glam::{Mat4, Quat, Vec3};

/// The world → reference-space placement of an immersive session, in the
/// session's `local-floor` space (metres, +Y up, the person facing −Z):
/// the world point `center` lands at `position`, scaled by `scale` and turned
/// by `yaw` radians about +Y. The default is one world unit to the metre with
/// the world's origin on the floor where the session started.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct XrStage {
    /// The world point that lands at `position`.
    pub center: Vec3,
    /// Metres per world unit: a 24-unit board at 0.05 is 1.2 m across.
    pub scale: f32,
    /// Turn about +Y, radians, applied after the scale.
    pub yaw: f32,
    /// Where `center` stands in the reference space, metres.
    pub position: Vec3,
    /// World units across the placed scene about `center` (the tabletop's
    /// `size`), zero when unknown. A headset frame fits the sun's single
    /// shadow layer to it, the same for both eyes and every frame.
    pub extent: f32,
}

impl Default for XrStage {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            scale: 1.0,
            yaw: 0.0,
            position: Vec3::ZERO,
            extent: 0.0,
        }
    }
}

impl XrStage {
    /// A tabletop: the world point `center` at `height` metres above the
    /// floor, `distance` metres in front of where the session started, and
    /// `size` world units across `metres` metres.
    pub fn tabletop(center: Vec3, size: f32, metres: f32, height: f32, distance: f32) -> Self {
        Self {
            center,
            scale: if size > 0.0 { metres / size } else { 1.0 },
            yaw: 0.0,
            position: Vec3::new(0.0, height, -distance),
            extent: if size > 0.0 && size.is_finite() { size } else { 0.0 },
        }
    }

    /// The scale, positive and finite, else one.
    pub fn metres_per_unit(&self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }

    /// The rigid part of the placement (world → reference space without its
    /// scale), about `center`: `reference = position + R·(scale·(world − center))`.
    fn rotation(&self) -> Quat {
        let yaw = if self.yaw.is_finite() { self.yaw } else { 0.0 };
        // The engine's own sine and cosine: a tick may place a hand with it.
        let (s, c) = crate::math::sin_cos(yaw * 0.5);
        Quat::from_xyzw(0.0, s, 0.0, c)
    }

    /// A world point in the session's reference space (metres):
    /// `position + R·(scale·(world − center))`.
    pub fn to_reference(&self, world: Vec3) -> Vec3 {
        self.position + self.rotation() * ((world - self.center) * self.metres_per_unit())
    }

    /// A reference-space point (a controller's position) in the world.
    pub fn to_world(&self, reference: Vec3) -> Vec3 {
        self.center + self.rotation().inverse() * (reference - self.position) / self.metres_per_unit()
    }

    /// A reference-space direction in the world, its length kept.
    pub fn direction_to_world(&self, direction: Vec3) -> Vec3 {
        self.rotation().inverse() * direction
    }

    /// A reference-space ray (a controller's aim, [`crate::XrPose::ray`]) in
    /// the world: its origin placed, its direction turned, still a unit long.
    pub fn ray_to_world(&self, origin: Vec3, direction: Vec3) -> (Vec3, Vec3) {
        (self.to_world(origin), self.direction_to_world(direction))
    }

    /// A reference-space orientation in the world.
    pub fn rotation_to_world(&self, rotation: Quat) -> Quat {
        self.rotation().inverse() * rotation
    }

    /// The placement after a hand holding the world moved from `from` to
    /// `to` (reference space): the world point that was in the hand still is.
    pub fn dragged(&self, from: Vec3, to: Vec3) -> Self {
        Self {
            position: self.position + (to - from),
            ..*self
        }
    }

    /// The placement after two hands holding the world moved from `from` to
    /// `to`: the world turns about +Y and grows or shrinks with the line
    /// between them, about its midpoint, and follows the midpoint. `scale`
    /// bounds the result's metres per world unit (lowest, highest). Hands on
    /// top of each other change nothing.
    pub fn pinched(&self, from: [Vec3; 2], to: [Vec3; 2], scale: (f32, f32)) -> Self {
        let (a, b) = (from[1] - from[0], to[1] - to[0]);
        let (la, lb) = (a.length(), b.length());
        if !(la > 1e-4 && lb > 1e-4 && la.is_finite() && lb.is_finite()) {
            return *self;
        }
        let s = self.metres_per_unit();
        let k = ((s * lb / la).clamp(scale.0.max(1e-6), scale.1.max(scale.0.max(1e-6)))) / s;
        // The turn about +Y of the line's footprint; none where either stands on end.
        let (fa, fb) = (Vec3::new(a.x, 0.0, a.z), Vec3::new(b.x, 0.0, b.z));
        let turn = if fa.length() > 1e-4 && fb.length() > 1e-4 {
            // The angle about +Y from a to b: positive turns +Z toward +X.
            crate::math::atan2(fa.z * fb.x - fa.x * fb.z, fa.x * fb.x + fa.z * fb.z)
        } else {
            0.0
        };
        let (sin, cos) = crate::math::sin_cos(turn);
        let (m0, m1) = ((from[0] + from[1]) * 0.5, (to[0] + to[1]) * 0.5);
        let d = (self.position - m0) * k;
        Self {
            position: m1 + Vec3::new(cos * d.x + sin * d.z, d.y, -sin * d.x + cos * d.z),
            yaw: crate::math::wrap_angle(self.yaw + turn),
            scale: s * k,
            ..*self
        }
    }

    /// One eye's world-to-view and view-to-clip matrices (0–1 depth) from the
    /// session's reference-to-view matrix (`XRView.transform.inverse.matrix`)
    /// and projection (`XRView.projectionMatrix`, already 0–1 depth in a
    /// WebGPU session), and the eye's world position. The view stays rigid in
    /// world units: the stage's scale moves into the projection's near and far
    /// planes, so depth, shadows and fog read world distances.
    pub fn eye(&self, reference_view: Mat4, projection: Mat4) -> (Mat4, Mat4, Vec3) {
        let s = self.metres_per_unit();
        // world → reference = T(position) · R · S(s) · T(−center)
        // eye = V · T(position) · R, a rigid map; V·M = S(s) · [Rq | tq/s] · T(−center).
        let rigid = reference_view
            * Mat4::from_translation(self.position)
            * Mat4::from_quat(self.rotation());
        let (_, rotation, translation) = rigid.to_scale_rotation_translation();
        let view = Mat4::from_rotation_translation(rotation, translation / s)
            * Mat4::from_translation(-self.center);
        // P · S(s) is P with its translation column scaled by 1/s, up to the
        // homogeneous factor s, which clipping ignores.
        let mut proj = projection;
        proj.w_axis /= s;
        let camera = view.inverse().w_axis.truncate();
        (view, proj, camera)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_matches_the_scaled_world_to_clip() {
        let stage = XrStage {
            center: Vec3::new(3.0, 0.0, -2.0),
            scale: 0.05,
            yaw: 0.4,
            position: Vec3::new(0.1, 0.85, -0.9),
            extent: 0.0,
        };
        let reference_view = Mat4::look_at_rh(
            Vec3::new(0.03, 1.6, 0.0),
            Vec3::new(0.0, 0.85, -0.9),
            Vec3::Y,
        );
        let projection = glam::camera::rh::proj::directx::perspective(1.6, 1.0, 0.1, 100.0);
        let placement = Mat4::from_translation(stage.position)
            * Mat4::from_quat(Quat::from_rotation_y(stage.yaw))
            * Mat4::from_scale(Vec3::splat(stage.scale))
            * Mat4::from_translation(-stage.center);
        let (view, proj, camera) = stage.eye(reference_view, projection);
        for p in [
            Vec3::new(3.0, 0.0, -2.0),
            Vec3::new(10.0, 2.0, 4.0),
            Vec3::new(-7.0, 1.0, 9.0),
        ] {
            let expected = (projection * reference_view * placement).project_point3(p);
            let got = (proj * view).project_point3(p);
            assert!((expected - got).length() < 1e-4, "{expected} {got}");
        }
        let eye = placement
            .inverse()
            .transform_point3(Vec3::new(0.03, 1.6, 0.0));
        assert!((camera - eye).length() < 1e-3, "{camera} {eye}");
        // Rigid: the view keeps world lengths.
        let d = view.transform_vector3(Vec3::X).length();
        assert!((d - 1.0).abs() < 1e-5);
    }

    fn stage() -> XrStage {
        XrStage {
            center: Vec3::new(3.0, 0.0, -2.0),
            scale: 0.05,
            yaw: 0.4,
            position: Vec3::new(0.1, 0.85, -0.9),
            extent: 24.0,
        }
    }

    #[test]
    fn a_reference_point_and_ray_land_in_the_world() {
        let stage = stage();
        let placement = Mat4::from_translation(stage.position)
            * Mat4::from_quat(Quat::from_rotation_y(stage.yaw))
            * Mat4::from_scale(Vec3::splat(stage.scale))
            * Mat4::from_translation(-stage.center);
        for p in [Vec3::new(3.0, 0.0, -2.0), Vec3::new(10.0, 2.0, 4.0)] {
            let reference = placement.transform_point3(p);
            assert!((stage.to_reference(p) - reference).length() < 1e-5);
            assert!((stage.to_world(reference) - p).length() < 1e-3);
        }
        assert!((stage.to_reference(stage.center) - stage.position).length() < 1e-6);
        // A hand above the table pointing down hits the board under it.
        let hand = stage.position + Vec3::new(0.2, 0.3, 0.1);
        let (origin, direction) = stage.ray_to_world(hand, Vec3::NEG_Y);
        assert!((direction - Vec3::NEG_Y).length() < 1e-6);
        let hit = origin + direction * (origin.y / -direction.y);
        assert!((stage.to_reference(hit) - (hand - Vec3::Y * 0.3)).length() < 1e-4);
        assert!((origin.y - 0.3 / 0.05).abs() < 1e-3);
    }

    #[test]
    fn a_held_world_follows_one_hand_and_two() {
        let stage = stage();
        let grabbed = Vec3::new(0.3, 0.9, -0.7);
        let held = stage.to_world(grabbed);
        let to = grabbed + Vec3::new(-0.1, 0.05, 0.2);
        let moved = stage.dragged(grabbed, to);
        assert!((moved.to_reference(held) - to).length() < 1e-5);
        assert_eq!((moved.scale, moved.yaw), (stage.scale, stage.yaw));

        // Two hands: both held points stay in their hands.
        let from = [Vec3::new(-0.2, 0.9, -0.8), Vec3::new(0.25, 0.95, -0.7)];
        let held = from.map(|p| stage.to_world(p));
        // Apart by half again, turned a quarter turn about +Y, moved.
        let mid = (from[0] + from[1]) * 0.5;
        let turn = Quat::from_rotation_y(0.5);
        let to = from.map(|p| mid + Vec3::new(0.1, 0.0, -0.05) + turn * ((p - mid) * 1.5));
        let pinched = stage.pinched(from, to, (0.001, 10.0));
        for (world, hand) in held.iter().zip(to) {
            assert!((pinched.to_reference(*world) - hand).length() < 1e-4, "{} {hand}", pinched.to_reference(*world));
        }
        assert!((pinched.scale - stage.scale * 1.5).abs() < 1e-6);
        assert!((pinched.yaw - (stage.yaw + 0.5)).abs() < 1e-5);
        // The bounds hold, about the hands' midpoint.
        let capped = stage.pinched(from, to, (0.001, 0.06));
        assert!((capped.scale - 0.06).abs() < 1e-7);
        let middle = stage.to_world(mid);
        assert!((capped.to_reference(middle) - (mid + Vec3::new(0.1, 0.0, -0.05))).length() < 1e-4);
        // Hands on one spot change nothing.
        assert_eq!(stage.pinched([grabbed; 2], to, (0.001, 10.0)), stage);
    }
}
