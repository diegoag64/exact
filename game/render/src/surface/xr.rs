//! Immersive frames: the world ticked once, drawn into both eyes. A stereo
//! frame (`XrStereo`, a headset's two-layer swapchain) draws both in one
//! multiview pass where the device has it (`encode_stereo`, `crate::stereo`);
//! otherwise, and for WebXR's per-eye textures, each eye draws on its own
//! (`encode_eyes`).
//!
//! Each eye is encoded and submitted on its own: the renderer writes its
//! frame uniforms, culling and light lists through the queue, and a queue
//! write lands before every command of the next submission, so two eyes in
//! one submission would both draw from the second eye. The render hooks are
//! left out (a game's post passes assume one flat camera), and so is the
//! perf timing.
use super::*;
use exact_game::{Mat4, XrStage};
use exact_gpu::{wgpu, XrStereo, XrView};

/// What an immersive frame draws into.
#[derive(Clone, Copy)]
pub(super) enum Immersive<'a> {
    /// Each eye its own target.
    Eyes(&'a [XrView]),
    /// Both eyes in the layers of one target.
    Stereo(&'a XrStereo),
}

impl<G: Game, P: Executor, const ASSETS: bool, H: crate::Hooks> WorldSurface<G, P, ASSETS, H> {
    /// The renderer is about to be made for another format (an immersive
    /// session's, or the page's again after one): the textures the old one
    /// held are delivered again, as after a lost device.
    pub(super) fn redeliver_textures(&mut self) {
        if ASSETS {
            if let Some(sim) = &mut self.sim {
                sim.invalidate_device_assets();
            }
        }
        self.assets_dirty = true;
        self.dirty = true;
    }

    /// A session draws in `format` (Some) or ended (None). A renderer made
    /// for a format other than the page's needs the textures the page's had;
    /// so does the page's after the session.
    pub(super) fn immersive_format(&mut self, format: Option<wgpu::TextureFormat>) {
        // The immersive profile may draw at other scene samples than the
        // page's rung: the renderer is made again, as for another format.
        if let (Some(sim), Some(made)) = (&self.sim, self.samples) {
            let rung = super::render::frame_rung(self.rung, format.is_some(), G::quality(sim.args()));
            let wanted = crate::quality::samples(rung, G::samples(sim.args()));
            if self.render.is_some() && made != wanted {
                self.redeliver_textures();
                self.format = None;
            }
        }
        match format {
            Some(format) if self.xr_format != Some(format) => {
                self.xr_format = Some(format);
                if self.format.is_some_and(|f| f != format) {
                    self.redeliver_textures();
                }
            }
            Some(_) => {}
            None => {
                if let Some(format) = self.xr_format.take() {
                    self.xr_left = self.format == Some(format);
                    self.dirty = true;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn encode_eyes<const ASSETS: bool>(
    renderer: &mut crate::renderer::RendererWithAssets<ASSETS>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input: &mut crate::FrameInput<'_>,
    stage: XrStage,
    views: &[XrView],
    world: &World,
    poses: &crate::hooks::Poses,
) -> Result<crate::Stats, crate::RenderError> {
    let mut stats = crate::Stats::default();
    // Outline pixel floors follow the eye's own raster.
    input.viewport_css_height = None;
    for eye in views {
        let (view, proj, camera) = stage.eye(
            Mat4::from_cols_array(&eye.view),
            Mat4::from_cols_array(&eye.projection),
        );
        input.view = view;
        input.proj = proj;
        input.camera_position = camera;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("exact xr eye"),
        });
        let eye_stats = renderer.encode_hooked(
            &mut encoder,
            &eye.target,
            eye.size,
            input,
            &mut (),
            Some(world),
            crate::HookTime::default(),
            poses,
            None,
        )?;
        stats.add(&eye_stats);
        queue.submit([encoder.finish()]);
        renderer.submitted();
    }
    Ok(stats)
}

/// Both eyes of `stereo` in one multiview pass and one submission, the
/// frame's shared work done once; each eye on its own (`encode_eyes`, into
/// views of its layer) where the renderer cannot draw this frame in one pass.
#[allow(clippy::too_many_arguments)]
pub(super) fn encode_stereo<const ASSETS: bool>(
    renderer: &mut crate::renderer::RendererWithAssets<ASSETS>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input: &mut crate::FrameInput<'_>,
    stage: XrStage,
    stereo: &XrStereo,
    format: wgpu::TextureFormat,
    world: &World,
    poses: &crate::hooks::Poses,
) -> Result<crate::Stats, crate::RenderError> {
    let eyes = [0, 1].map(|i| {
        let (view, proj, camera) = stage.eye(
            Mat4::from_cols_array(&stereo.views[i]),
            Mat4::from_cols_array(&stereo.projections[i]),
        );
        crate::StereoEye { view, proj, camera }
    });
    let target = stereo.array_view(format);
    let size = (stereo.size.0.max(1), stereo.size.1.max(1));
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("exact xr stereo"),
    });
    match renderer.encode_stereo(&mut encoder, &target, size, input, eyes, Some(world), poses)? {
        Some(stats) => {
            queue.submit([encoder.finish()]);
            renderer.submitted();
            Ok(stats)
        }
        None => {
            drop(encoder);
            let views = stereo.layer_views(format);
            encode_eyes(renderer, device, queue, input, stage, &views, world, poses)
        }
    }
}

/// A stereo frame's mode for a host's log (`Surface::xr_stereo_mode`).
pub(super) fn stereo_mode(multiview: bool, direct: bool, samples: u32) -> &'static str {
    match (multiview, direct, samples > 1) {
        (true, true, true) => "multiview-direct-4x",
        (true, true, false) => "multiview-direct-1x",
        (true, false, true) => "multiview-hdr-4x",
        (true, false, false) => "multiview-hdr-1x",
        (false, true, true) => "per-eye-direct-4x",
        (false, true, false) => "per-eye-direct-1x",
        (false, false, true) => "per-eye-hdr-4x",
        (false, false, false) => "per-eye-hdr-1x",
    }
}
