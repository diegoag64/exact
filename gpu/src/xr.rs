//! Immersive frames (WebXR): one frame of a canvas's surface drawn once per
//! eye into textures the session lends, instead of into the canvas.
//!
//! The host hands each eye's texture, the region of it the eye owns, and the
//! eye's matrices ([`XrEye`]); [`Module::render_xr`] gives the surface a
//! target per eye ([`XrView`]) and copies an eye that owns only part of its
//! texture (side by side) into place. A surface draws immersive frames by
//! implementing [`crate::Surface::render_xr`]; one that does not refuses the
//! session.

use crate::{shaders, Frame, Module};

/// One eye of an immersive frame, as a surface draws it.
pub struct XrView {
    /// The eye's colour target, `size` pixels.
    pub target: wgpu::TextureView,
    /// Its width and height in pixels.
    pub size: (u32, u32),
    /// Reference space → eye (`XRView.transform.inverse.matrix`), column-major.
    pub view: [f32; 16],
    /// Eye → clip (`XRView.projectionMatrix`), column-major, 0–1 depth.
    pub projection: [f32; 16],
}

/// Both eyes of one stereo frame in a two-layer array target: layer 0 the
/// left eye, layer 1 the right (OpenXR's view order). A host whose swapchain
/// is an array of two layers (a Quest's) lends it whole, so a surface can draw
/// both eyes in one multiview pass ([`crate::Surface::render_xr_stereo`]).
pub struct XrStereo {
    /// `D2` texture of two array layers, in the swapchain's format, with
    /// `RENDER_ATTACHMENT` usage.
    pub target: wgpu::Texture,
    /// Width and height in pixels of each layer.
    pub size: (u32, u32),
    /// Reference space → eye, column-major, per layer.
    pub views: [[f32; 16]; 2],
    /// Eye → clip, column-major, 0–1 depth, per layer.
    pub projections: [[f32; 16]; 2],
}

impl XrStereo {
    /// The whole target as one `D2Array` view of both layers: a multiview
    /// pass's colour attachment.
    pub fn array_view(&self, format: wgpu::TextureFormat) -> wgpu::TextureView {
        self.target.create_view(&wgpu::TextureViewDescriptor {
            label: Some("exact xr stereo"),
            format: Some(format),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(2),
            ..Default::default()
        })
    }
    /// Each eye on its own: a `D2` view of its layer and its matrices, as
    /// [`crate::Surface::render_xr`] draws them.
    pub fn layer_views(&self, format: wgpu::TextureFormat) -> Vec<XrView> {
        (0..2)
            .map(|layer| XrView {
                target: self.target.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("exact xr eye"),
                    format: Some(format),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer as u32,
                    array_layer_count: Some(1),
                    ..Default::default()
                }),
                size: (self.size.0.max(1), self.size.1.max(1)),
                view: self.views[layer],
                projection: self.projections[layer],
            })
            .collect()
    }
}

/// One eye as the host lends it: the session's texture, the array layer and
/// region of it this eye owns, and its matrices.
pub struct XrEye {
    /// The session's colour texture (shared by the eyes of a texture array).
    pub texture: wgpu::Texture,
    /// The array layer this eye draws into.
    pub layer: u32,
    /// `[x, y, width, height]` in pixels of `layer`.
    pub viewport: [u32; 4],
    /// Reference space → eye, column-major.
    pub view: [f32; 16],
    /// Eye → clip, column-major, 0–1 depth.
    pub projection: [f32; 16],
}

/// An eye's own texture when it owns only part of the session's, kept
/// across frames while its size and format hold.
#[derive(Default)]
pub struct XrScratch {
    textures: Vec<wgpu::Texture>,
}

impl XrEye {
    fn whole(&self) -> bool {
        let size = self.texture.size();
        self.viewport == [0, 0, size.width, size.height]
    }
}

impl Module {
    /// The immersive session ended: every surface prepares for its page again.
    pub fn end_xr(&mut self) {
        for inst in self.instances.values_mut() {
            inst.surface.immersive(None);
            inst.dirty = true;
        }
    }

    /// The format canvas `id` draws its page frames in: an immersive layer in
    /// the same format keeps the surface's renderer as it is.
    pub fn render_format(&self, id: u32) -> Option<wgpu::TextureFormat> {
        self.instances
            .get(&id)?
            .config
            .as_ref()
            .map(crate::view_format)
    }

    /// Draw one stereo frame of canvas `id` into `stereo`'s two layers (in
    /// `format`) and submit it: the surface ticks once and draws both eyes,
    /// in one multiview pass where it and the device can
    /// ([`crate::Surface::render_xr_stereo`]). Whether the surface wants
    /// another frame; None on failure ([`Module::error`] says why).
    pub fn render_xr_stereo(
        &mut self,
        id: u32,
        now_ms: f64,
        format: wgpu::TextureFormat,
        stereo: &XrStereo,
    ) -> Option<bool> {
        if let Some(error) = self.refused_instance(id) {
            return self.fail(error);
        }
        if stereo.target.depth_or_array_layers() < 2 {
            return self.fail("a stereo frame's target has fewer than two layers");
        }
        self.check_device();
        self.settle(id);
        let timing = self.timing(id, now_ms);
        let (seekable, period_ms) = (self.seekable, self.period_ms);
        let Some(gpu) = self.gpu.as_ref() else {
            return self.fail("no device");
        };
        let Some(inst) = self.instances.get_mut(&id) else {
            return self.fail("no such canvas");
        };
        if !inst.bound {
            return Some(false);
        }
        inst.surface.immersive(Some(format));
        let frame = Frame {
            width: stereo.size.0.max(1) as f32,
            height: stereo.size.1.max(1) as f32,
            scale: 1.0,
            now_ms,
            seekable,
            period_ms,
            children_generation: inst.children_generation,
            shader_generation: shaders::shader_generation(),
            headroom: 1.0,
        };
        inst.surface.timing(&timing);
        let wants = inst
            .surface
            .render_xr_stereo(&frame, &gpu.device, &gpu.queue, stereo, format);
        inst.drain();
        if let Some(crate::SurfaceError(e)) = inst.surface.take_error() {
            return self.fail(e);
        }
        let Some(wants) = wants else {
            return self.fail("this canvas draws no immersive frames");
        };
        inst.surface.submitted();
        inst.dirty = true;
        Some(wants)
    }

    /// Draw one immersive frame of canvas `id` into `eyes` (in `format`) and
    /// submit it. The surface ticks once and draws each eye; its commands are
    /// submitted before this returns, while the session's textures are lent.
    /// Whether the surface wants another frame; None on failure ([`Module::error`]
    /// says why), including a surface that draws no immersive frames.
    pub fn render_xr(
        &mut self,
        id: u32,
        now_ms: f64,
        format: wgpu::TextureFormat,
        eyes: &[XrEye],
        scratch: &mut XrScratch,
    ) -> Option<bool> {
        if let Some(error) = self.refused_instance(id) {
            return self.fail(error);
        }
        if eyes.is_empty() {
            return self.fail("an immersive frame has no eyes");
        }
        self.check_device();
        // Anything this canvas recorded for its page goes first.
        self.settle(id);
        let timing = self.timing(id, now_ms);
        let (seekable, period_ms) = (self.seekable, self.period_ms);
        let Some(gpu) = self.gpu.as_ref() else {
            return self.fail("no device");
        };
        let Some(inst) = self.instances.get_mut(&id) else {
            return self.fail("no such canvas");
        };
        if !inst.bound {
            return Some(false);
        }
        // The surface prepares its assets in `render_xr`, in `format`.
        inst.surface.immersive(Some(format));
        let (width, height) = (eyes[0].viewport[2].max(1), eyes[0].viewport[3].max(1));
        // An eye that owns part of its texture draws into its own, copied in after.
        let parts = eyes.iter().filter(|e| !e.whole()).count();
        scratch
            .textures
            .retain(|t| t.format() == format && t.width() == width && t.height() == height);
        while scratch.textures.len() < parts {
            scratch
                .textures
                .push(gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("exact xr eye"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }));
        }
        let mut own = scratch.textures.iter();
        let mut copies = Vec::new();
        let views: Vec<XrView> = eyes
            .iter()
            .map(|eye| {
                let target = if eye.whole() {
                    eye.texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("exact xr eye"),
                        format: Some(format),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_array_layer: eye.layer,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                } else {
                    let texture = own.next().unwrap();
                    copies.push((texture, eye));
                    texture.create_view(&Default::default())
                };
                XrView {
                    target,
                    size: (eye.viewport[2].max(1), eye.viewport[3].max(1)),
                    view: eye.view,
                    projection: eye.projection,
                }
            })
            .collect();
        let frame = Frame {
            width: width as f32,
            height: height as f32,
            scale: 1.0,
            now_ms,
            seekable,
            period_ms,
            children_generation: inst.children_generation,
            shader_generation: shaders::shader_generation(),
            headroom: 1.0,
        };
        inst.surface.timing(&timing);
        let wants = inst
            .surface
            .render_xr(&frame, &gpu.device, &gpu.queue, &views, format);
        inst.drain();
        if let Some(crate::SurfaceError(e)) = inst.surface.take_error() {
            return self.fail(e);
        }
        let Some(wants) = wants else {
            return self.fail("this canvas draws no immersive frames");
        };
        if !copies.is_empty() {
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("exact xr copy"),
                });
            for (texture, eye) in copies {
                encoder.copy_texture_to_texture(
                    texture.as_image_copy(),
                    wgpu::TexelCopyTextureInfo {
                        texture: &eye.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: eye.viewport[0],
                            y: eye.viewport[1],
                            z: eye.layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: width.min(eye.viewport[2]),
                        height: height.min(eye.viewport[3]),
                        depth_or_array_layers: 1,
                    },
                );
            }
            gpu.queue.submit([encoder.finish()]);
        }
        inst.surface.submitted();
        // The page's canvas shows its last frame until the session ends.
        inst.dirty = true;
        Some(wants)
    }
}
