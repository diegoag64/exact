//! Immersive (WebXR) frames: the world ticked once, drawn once per eye.
//!
//! Each eye is encoded and submitted on its own: the renderer writes its
//! frame uniforms, culling and light lists through the queue, and a queue
//! write lands before every command of the next submission, so two eyes in
//! one submission would both draw from the second eye. The render hooks are
//! left out (a game's post passes assume one flat camera), and so is the
//! perf timing.
use exact_game::{Mat4, World, XrStage};
use exact_gpu::{wgpu, XrView};

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
