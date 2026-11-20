//! A stereo frame's encode (`crate::stereo`): the frame's shared work once on
//! the shared camera, its frame pass (forward, translucent, tone) multiview
//! into a two-layer array target.
use super::*;
use crate::stereo::{self as st, Eye};

/// What a stereo frame draws with, kept between frames and swapped with the
/// renderer's own for the length of its encode: the multiview pipeline set
/// and its hull, water and quad pipelines, and two-layer attachments. The
/// effects a stereo frame leaves out (bloom, ambient occlusion, a hook's
/// targets) hold `None` here, so the renderer's own outlive its frames.
pub(crate) struct StereoSet {
    pipelines: Pipelines,
    outlines: Option<crate::outline::Pipelines>,
    quads: crate::quads::QuadViews,
    targets: Targets,
    direct_targets: Option<crate::buffers::DirectTargets>,
    water: crate::water::Passes,
    bloom: Option<BloomTargets>,
    ssao: Option<crate::ssao::Ssao>,
    hook_targets: Option<crate::hooks::HookTargets>,
}

impl StereoSet {
    /// Join finished pipeline specializations (`Pipelines::reap_specializations`).
    pub(crate) fn reap(&self) {
        self.pipelines.reap_specializations();
    }
}

impl<const ASSETS: bool> RendererWithAssets<ASSETS> {
    /// Both views while a stereo frame encodes its frame pass, else none.
    pub(crate) fn view_mask(&self) -> Option<std::num::NonZeroU32> {
        self.stereo_eyes.map(|_| st::MASK)
    }

    /// Whether this renderer's device draws stereo frames in one multiview
    /// pass (`MULTIVIEW`, two views, and `MULTISAMPLE_ARRAY` at four samples).
    pub fn stereo_supported(&self) -> bool {
        st::supported(&self.device, self.pipelines.samples)
    }

    /// Encode one stereo frame of `eyes` into `target`, a `D2Array` view of
    /// two layers of `size` pixels each (layer 0 the left eye). The frame's
    /// shared work (skinning, cull, cascades, light lists, levels of detail,
    /// translucent order) runs once on the shared camera
    /// ([`st::shared_camera`]); the frame pass draws both layers. `frame`'s
    /// camera is replaced for the encode and restored after; bloom, ambient
    /// occlusion, water shafts and soft particles are left out. Ok(None)
    /// when this frame cannot draw in one pass (no multiview device, or a
    /// look frame's probes armed): the caller draws each eye instead.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_stereo(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        frame: &mut FrameInput<'_>,
        eyes: [Eye; 2],
        world: Option<&exact_game::World>,
        poses: &crate::hooks::Poses,
    ) -> Result<Option<Stats>, RenderError> {
        if !self.stereo_supported() || self.probe.is_some() {
            return Ok(None);
        }
        let (view, proj, centre) = st::shared_camera(&eyes);
        let saved = (
            frame.view,
            frame.proj,
            frame.camera_position,
            frame.environment.bloom,
            frame.ambient_occlusion,
            frame.viewport_css_height,
        );
        frame.view = view;
        frame.proj = proj;
        frame.camera_position = centre;
        frame.environment.bloom = None;
        frame.ambient_occlusion = None;
        // Outline pixel floors follow the eyes' own raster.
        frame.viewport_css_height = None;
        let effects = self.water_effects;
        self.water_effects.shafts = false;
        self.swap_stereo(true);
        self.stereo_eyes = Some(eyes);
        self.stereo_frames += 1;
        let result = self.encode_hooked(
            encoder,
            target,
            size,
            frame,
            &mut (),
            world,
            crate::HookTime::default(),
            poses,
            None,
        );
        self.stereo_eyes = None;
        self.swap_stereo(false);
        self.water_effects = effects;
        (
            frame.view,
            frame.proj,
            frame.camera_position,
            frame.environment.bloom,
            frame.ambient_occlusion,
            frame.viewport_css_height,
        ) = saved;
        result.map(Some)
    }

    /// Swap the stereo set in (`enter`, making it at the first stereo frame
    /// and catching it up with models and outlines prepared since) or back out.
    fn swap_stereo(&mut self, enter: bool) {
        if enter {
            let set = self.stereo.get_or_insert_with(|| {
                let pipelines = self.pipelines.stereo(&self.device);
                let targets = Targets::with_layers(
                    &self.device,
                    (64, 64),
                    &pipelines.tone_layout,
                    &self.uniform,
                    false,
                    pipelines.samples,
                    2,
                );
                Box::new(StereoSet {
                    pipelines,
                    outlines: None,
                    quads: crate::quads::QuadViews::stereo(),
                    targets,
                    direct_targets: None,
                    water: Default::default(),
                    bloom: None,
                    ssao: None,
                    hook_targets: None,
                })
            });
            set.pipelines.share_models(&self.device, &self.pipelines);
            if let (Some(mono), None) = (&self.outlines, &set.outlines) {
                set.outlines = Some(mono.stereo(&self.device, &set.pipelines));
            }
        }
        let set = self.stereo.as_mut().unwrap();
        std::mem::swap(&mut self.pipelines, &mut set.pipelines);
        std::mem::swap(&mut self.outlines, &mut set.outlines);
        std::mem::swap(&mut self.targets, &mut set.targets);
        std::mem::swap(&mut self.direct_targets, &mut set.direct_targets);
        std::mem::swap(&mut self.water, &mut set.water);
        std::mem::swap(&mut self.bloom, &mut set.bloom);
        std::mem::swap(&mut self.ssao, &mut set.ssao);
        std::mem::swap(&mut self.hook_targets, &mut set.hook_targets);
        self.quads.swap_views(&self.device, &mut set.quads);
    }
}
