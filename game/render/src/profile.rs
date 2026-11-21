//! The immersive profile: what a headset frame spends, on a tiling mobile GPU
//! (a Quest 3's Adreno 740) drawing two eyes of about 1680×1760 at 72 Hz.
//!
//! Every immersive frame takes it (`Surface::render_xr`, and a native XR
//! host's frames), whatever the quality rung; `Quality::Xr` asks for it on a
//! flat screen, to measure or look at the headset's picture on a desktop.
//! Like the rest of the quality ladder it is presentation only: nothing here
//! reaches the world, its saves or its hash.
//!
//! What it changes, and why:
//!
//! - **Direct frames** (README "Direct frames"): the scene is shaded and
//!   tonemapped straight into the eye's texture in one forward pass, with no
//!   HDR image, no resolve copy and no full-screen tone pass. Its MSAA
//!   attachment is transient and resolves into the target at the end of the
//!   pass, so on a tiler the samples live and resolve in tile memory and
//!   never reach DRAM. Translucent draws blend after the tone curve
//!   (`display_blends`), a small change at low alpha, rather than forcing the
//!   HDR path.
//! - **Four samples**, because the resolve is in tile: the cost is tile
//!   memory (more bins), not bandwidth, and aliasing is what a headset shows
//!   most. Meta's guidance for Quest is the same 4x. The game's
//!   `SceneSamples::One` stays the ceiling.
//! - **No bloom and no ambient occlusion** (each a chain of full-screen
//!   passes that read the HDR image back from memory).
//! - **One sun shadow layer of [`SHADOW_RESOLUTION`]²**, fixed to the box
//!   about [`XrStage::extent`](exact_game::XrStage) when the stage gives one:
//!   the same light matrix for both eyes and every frame, so its static
//!   casters are drawn once into the cache (`Shadows::cache_static`) and only
//!   what moves is drawn again. A game's own `Shadows` keeps its filter.
//! - **Water without caustics or shafts**, its surface drawn by a direct
//!   pipeline in the forward pass.
//! - **Models' coarse levels sooner**: a `ModelLod` level is chosen at
//!   [`LOD_BIAS`] times the eye's distance (a headset's pixels per degree are
//!   about a desktop monitor's, but its raster is drawn twice).
//! - **Fewer local lights** ([`MAX_LIGHTS`], the nearest) and local shadow
//!   layers of at most 512².
//! - **Full raster scale**: the eye's texture is the size the runtime asked
//!   for; a host that wants fewer pixels asks for a smaller swapchain.
//!
//! What a native host does for it: hand `Surface::render_xr` the eye
//! textures in an sRGB format (`Rgba8UnormSrgb`: the direct pipelines then
//! leave the encoding to the hardware) and single-sampled — the profile's MSAA
//! attachment is its own, transient, and resolves into them. Its
//! Vulkan device should grant `TRANSIENT_ATTACHMENT` usage (lazily allocated
//! memory) so the samples never take memory at all. That holds for the
//! per-eye path's single-layer attachments; a multiview stereo frame's are
//! two-layer, which wgpu refuses as transient, so they take real memory
//! (`buffers.rs`, `DirectTargets`) though their contents are discarded. Fixed foveation and the
//! swapchain's size (Meta's recommended 1680×1760 at 1.0, fewer pixels for
//! headroom) are the host's.
use exact_game::{ShadowBox, XrStage};

/// Width and height of the sun's single shadow layer.
pub const SHADOW_RESOLUTION: u32 = 1024;
/// A `ModelLod` level is chosen at this many times the eye's distance.
pub const LOD_BIAS: f32 = 3.0;
/// Local lights drawn at most, the nearest first.
pub const MAX_LIGHTS: usize = 8;
/// Largest local shadow layer.
pub const LOCAL_SHADOW_RESOLUTION: u32 = 512;

/// The sun's shadow box for a stage whose extent is known: the whole scene
/// about its centre, with a margin for what stands on its edge, and depth
/// enough for its tallest pieces and what casts onto it from above.
pub fn shadow_box(stage: &XrStage) -> Option<ShadowBox> {
    let extent = stage.extent;
    (extent.is_finite() && extent > 0.0 && stage.center.is_finite()).then(|| ShadowBox {
        center: stage.center,
        half_width: 0.55 * extent,
        half_depth: 0.75 * extent,
    })
}

/// Clamp one frame's input to the profile. `authored_shadows`: the game
/// inserted a `Shadows` resource, whose filter, strength and softness stand;
/// its layer count, size and box are the profile's. `stage`: where the game
/// places its world in a headset.
pub(crate) fn clamp(input: &mut crate::FrameInput<'_>, authored_shadows: bool, stage: Option<&XrStage>) {
    let _ = authored_shadows;
    if let Some(sun) = &mut input.sun {
        // A measurement's switch (`xr-debug`): the frame without the sun's shadow.
        if exact_gpu::debug_switch("no-sun-shadow") {
            sun.shadows = None;
        }
        if let Some(shadows) = &mut sun.shadows {
            shadows.cascades = 1;
            shadows.resolution = shadows.resolution.min(SHADOW_RESOLUTION);
            if let Some(fixed) = stage.and_then(shadow_box) {
                shadows.fixed_box = Some(fixed);
                // One matrix for both eyes and every frame: what stands
                // still is drawn once.
                shadows.cache_static = true;
            }
        }
    }
    input.environment.bloom = None;
    input.ambient_occlusion = None;
    if input.lights.len() > MAX_LIGHTS {
        input.lights_dropped += input.lights.len() - MAX_LIGHTS;
        input.lights = &input.lights[..MAX_LIGHTS];
    }
    let local = &mut input.local_shadows.resolution;
    *local = (*local).min(LOCAL_SHADOW_RESOLUTION);
}

/// Which of a `Water` resource's effects the renderer draws: the surface
/// alone.
pub fn water_effects() -> crate::WaterEffects {
    crate::WaterEffects {
        caustics: false,
        shafts: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exact_game::{Bloom, Shadows, Vec3};

    #[test]
    fn a_headset_frame_draws_one_fixed_shadow_layer_and_no_bloom() {
        let stage = XrStage::tabletop(Vec3::new(4.0, 0.0, -2.0), 24.0, 1.2, 0.85, 0.9);
        let mut input = crate::FrameInput {
            sun: Some(crate::Sun {
                shadows: Some(Shadows::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        input.environment.bloom = Some(Bloom::default());
        clamp(&mut input, false, Some(&stage));
        let shadows = input.sun.unwrap().shadows.unwrap();
        assert_eq!(shadows.cascades, 1);
        assert_eq!(shadows.resolution, SHADOW_RESOLUTION);
        assert!(shadows.cache_static);
        let fixed = shadows.fixed_box.unwrap();
        assert_eq!(fixed.center, stage.center);
        assert!(fixed.half_width >= 12.0, "the whole board fits");
        assert!(input.environment.bloom.is_none());
        assert_eq!(input.local_shadows.resolution, LOCAL_SHADOW_RESOLUTION);
    }

    #[test]
    fn without_a_stage_extent_the_layer_follows_the_eye() {
        let mut input = crate::FrameInput::default();
        clamp(&mut input, false, Some(&XrStage::default()));
        let shadows = input.sun.unwrap().shadows.unwrap();
        assert_eq!(shadows.cascades, 1);
        assert!(shadows.fixed_box.is_none());
        assert!(!shadows.cache_static);
    }

    #[test]
    fn the_nearest_lights_are_kept() {
        let lights = vec![crate::LightInput::default(); MAX_LIGHTS + 3];
        let mut input = crate::FrameInput {
            lights: &lights,
            lights_dropped: 1,
            ..Default::default()
        };
        clamp(&mut input, false, None);
        assert_eq!(input.lights.len(), MAX_LIGHTS);
        assert_eq!(input.lights_dropped, 4);
    }
}
