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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use exact_game::{Camera, Environment, Material, Mesh, Transform, World};
    use exact_gpu::fixture;
    use glam::{Mat4, Vec3};

    #[test]
    fn native_two_samples_resolve_both_direct_eye_layers() {
        use wgpu::{Features as F, TextureFormat as T};
        let required =
            F::MULTIVIEW | F::MULTISAMPLE_ARRAY | F::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let Some(gpu) = fixture::device_or_skip(fixture::device_with(required)) else {
            return;
        };
        let format = T::Rgba8UnormSrgb;
        if !gpu.device.features().contains(required)
            || ![format, T::Rgba16Float, T::Depth32Float]
                .into_iter()
                .all(|f| {
                    gpu.adapter
                        .get_texture_format_features(f)
                        .flags
                        .sample_count_supported(2)
                })
        {
            eprintln!(
                "skipped: native 2x stereo unavailable on {:?}",
                gpu.adapter.get_info().name
            );
            return;
        }
        let mut renderer =
            crate::Renderer::with_sample_count(&gpu.device, &gpu.queue, format, 2, false);
        renderer.set_immersive(true);
        let mut world = World::new(60, 1);
        world.insert_resource(Environment {
            background: Some([0.; 3].into()),
            fog: None,
            bloom: None,
            ..Default::default()
        });
        world.spawn((Transform::at(0., 0., 4.), Camera::default()));
        world.spawn((
            Transform::default(),
            Mesh::cube(1.2),
            Material::glow([3.; 3]),
        ));
        let mut feed = crate::Feed::default();
        feed.feed(&world, &mut renderer).unwrap();
        let mut frame = feed.frame(&world, 1., 1.);
        let eyes = [-0.18, 0.18].map(|x| {
            let camera = Vec3::new(x, 0., 4.);
            Eye {
                view: Mat4::from_translation(-camera),
                proj: frame.proj,
                camera,
            }
        });
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("single-sample eye array"),
            size: wgpu::Extent3d {
                width: 96,
                height: 96,
                depth_or_array_layers: 2,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let stats = renderer
            .encode_stereo(
                &mut encoder,
                &target.create_view(&Default::default()),
                (96, 96),
                &mut frame,
                eyes,
                Some(&world),
                &Default::default(),
            )
            .unwrap()
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        assert_eq!(stats.samples, 2);
        assert_eq!(stats.direct_frames, 1);
        let scratch = renderer
            .stereo
            .as_ref()
            .unwrap()
            .direct_targets
            .as_ref()
            .unwrap();
        assert_eq!(scratch.color.as_ref().unwrap().texture().sample_count(), 2);
        assert_eq!(scratch.depth.texture().sample_count(), 2);
        assert_eq!(scratch.size.depth_or_array_layers, 2);
        let centers = [0, 1].map(|layer| {
            let pixels = fixture::read_layer(&gpu, &target, layer).unwrap();
            let xs: Vec<usize> = pixels
                .data
                .chunks_exact(4)
                .enumerate()
                .filter(|(_, p)| p[0] > 150)
                .map(|(i, _)| i % 96)
                .collect();
            assert!(
                xs.len() > 100 && xs.len() < 96 * 96 / 2,
                "cube on dark background in eye {layer}"
            );
            xs.iter().sum::<usize>() as f32 / xs.len() as f32
        });
        assert!(
            centers[0] > centers[1] + 1.,
            "parallax across resolved layers: {centers:?}"
        );
    }
}
