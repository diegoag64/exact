//! Immersive (WebXR) frames: the world ticked once, drawn once per eye.
//!
//! Each eye is encoded and submitted on its own: the renderer writes its
//! frame uniforms, culling and light lists through the queue, and a queue
//! write lands before every command of the next submission, so two eyes in
//! one submission would both draw from the second eye. The render hooks are
//! left out (a game's post passes assume one flat camera), and so is the
//! perf timing.
use super::*;
use exact_game::{Mat4, XrStage};
use exact_gpu::{wgpu, XrView};

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
        stats = renderer.encode_hooked(
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
        queue.submit([encoder.finish()]);
        renderer.submitted();
    }
    Ok(stats)
}
