//! Where a world stands in an immersive (WebXR) session. A game opts in with
//! app.json's `game.xr` and places its world with [`Game::xr_stage`]
//! (`crate::Game`); the renderer then draws each eye from the headset's pose
//! and ignores the world's own camera. Presentation only: nothing here is
//! saved, hashed or seen by a tick.

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
}

impl Default for XrStage {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            scale: 1.0,
            yaw: 0.0,
            position: Vec3::ZERO,
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
        Quat::from_rotation_y(if self.yaw.is_finite() { self.yaw } else { 0.0 })
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
}
