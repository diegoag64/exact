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

/// Both eyes of one stereo frame in a two-layer array target (OpenXR's
/// `arraySize = 2` swapchain image): layer 0 the left eye, layer 1 the right.
/// A surface may draw both layers in one pass (multiview) through
/// [`crate::Surface::render_xr_stereo`]; the default draws each layer through
/// [`crate::Surface::render_xr`].
pub struct XrStereo {
    /// The frame's colour target: `D2` with two array layers, `size` pixels,
    /// `RENDER_ATTACHMENT` usage.
    pub target: wgpu::Texture,
    /// Each layer's width and height in pixels.
    pub size: (u32, u32),
    /// Reference space → eye, column-major, per layer.
    pub views: [[f32; 16]; 2],
    /// Eye → clip, column-major, 0–1 depth (not reversed), per layer.
    pub projections: [[f32; 16]; 2],
}

impl XrStereo {
    /// One [`XrView`] per layer, each a `D2` view of its array layer in
    /// `format`: what the per-eye path draws into.
    pub fn eye_views(&self, format: wgpu::TextureFormat) -> [XrView; 2] {
        [0u32, 1].map(|layer| XrView {
            target: self.target.create_view(&wgpu::TextureViewDescriptor {
                label: Some("exact xr eye"),
                format: Some(format),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            }),
            size: (self.size.0.max(1), self.size.1.max(1)),
            view: self.views[layer as usize],
            projection: self.projections[layer as usize],
        })
    }

    /// The whole target as one `D2Array` view of both layers in `format`,
    /// what a multiview pass renders into.
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

    /// [`XrStereo::eye_views`] as a `Vec`, the slice
    /// [`crate::Surface::render_xr`] takes.
    pub fn layer_views(&self, format: wgpu::TextureFormat) -> Vec<XrView> {
        self.eye_views(format).into()
    }
}

/// Eye → clip (column-major, 0–1 depth, right-handed, looking down −Z)
/// from a field of view's angles in radians, `[left, right, up, down]`
/// (left and down negative, as OpenXR's `XrFovf`), and the near and far
/// planes.
pub fn fov_projection(angles: [f32; 4], near: f32, far: f32) -> [f32; 16] {
    let [l, r, u, d] = angles.map(f32::tan);
    let (w, h) = (r - l, u - d);
    [
        2.0 / w,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / h,
        0.0,
        0.0,
        (r + l) / w,
        (u + d) / h,
        far / (near - far),
        -1.0,
        0.0,
        0.0,
        near * far / (near - far),
        0.0,
    ]
}

/// Reference space → eye (column-major): the inverse of an eye's pose, its
/// orientation a unit quaternion `[x, y, z, w]` and its position in metres.
pub fn pose_view(orientation: [f32; 4], position: [f32; 3]) -> [f32; 16] {
    let [x, y, z, w] = orientation;
    // Rows of the pose's rotation.
    let r = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    let p = position;
    // The inverse is Rᵀ with −Rᵀp; column j of Rᵀ is row j of R.
    let t = [0, 1, 2].map(|i| -(r[0][i] * p[0] + r[1][i] * p[1] + r[2][i] * p[2]));
    [
        r[0][0], r[0][1], r[0][2], 0.0, //
        r[1][0], r[1][1], r[1][2], 0.0, //
        r[2][0], r[2][1], r[2][2], 0.0, //
        t[0], t[1], t[2], 1.0,
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `m` (column-major) applied to a point, divided through.
    fn project(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
        let v = [p[0], p[1], p[2], 1.0];
        let out: Vec<f32> = (0..4)
            .map(|row| (0..4).map(|col| m[col * 4 + row] * v[col]).sum())
            .collect();
        [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
    }

    #[test]
    fn a_fov_projection_maps_the_frustum_edges_and_depth() {
        let (l, r, u, d) = (-0.9f32, 0.7f32, 0.8f32, -0.85f32);
        let m = fov_projection([l, r, u, d], 0.1, 50.0);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(project(&m, [r.tan() * 2.0, 0.0, -2.0])[0], 1.0));
        assert!(close(project(&m, [l.tan() * 2.0, 0.0, -2.0])[0], -1.0));
        assert!(close(project(&m, [0.0, u.tan() * 3.0, -3.0])[1], 1.0));
        assert!(close(project(&m, [0.0, d.tan() * 3.0, -3.0])[1], -1.0));
        assert!(close(project(&m, [0.0, 0.0, -0.1])[2], 0.0));
        assert!(close(project(&m, [0.0, 0.0, -50.0])[2], 1.0));
    }

    #[test]
    fn a_pose_view_takes_the_eye_to_the_origin_looking_down_minus_z() {
        // Turned 90° left about +Y (looking down −X), standing at (1, 1.6, 2).
        let half = std::f32::consts::FRAC_PI_4;
        let m = pose_view([0.0, half.sin(), 0.0, half.cos()], [1.0, 1.6, 2.0]);
        let eye = project(&m, [1.0, 1.6, 2.0]);
        assert!(eye.iter().all(|c| c.abs() < 1e-5), "{eye:?}");
        // A point a metre down −X from the eye is a metre ahead (−Z).
        let ahead = project(&m, [0.0, 1.6, 2.0]);
        assert!((ahead[2] + 1.0).abs() < 1e-5 && ahead[0].abs() < 1e-5, "{ahead:?}");
    }
}
