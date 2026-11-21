//! Stereo frames: both eyes of a headset drawn in one multiview pass
//! (`wgpu::Features::MULTIVIEW`, `@builtin(view_index)`), into a two-layer
//! array target (`exact_gpu::XrStereo`).
//!
//! What does not depend on the eye runs once for both: skinning, the GPU
//! cull, the sun's cascades, local light lists and shadows, levels of detail
//! and the translucent order all take one *shared* camera, whose frustum
//! holds both eyes' ([`shared_camera`]). Only the passes that draw the
//! picture (forward, translucent, the tone pass) are multiview: their
//! pipelines come from a second pipeline set compiled from the same shader
//! pieces with [`multiview_source`], which reads each eye's matrices from a
//! block after the frame uniform's words ([`EYE_WORDS`]).
//!
//! Left out of stereo frames, as they are of the per-eye path: render hooks,
//! bloom, ambient occlusion, the water's shafts and soft particles (drawn
//! hard). A frame that needs anything else multiview cannot draw (a look
//! frame's probes) is drawn per eye instead.
use exact_gpu::wgpu;
use glam::{Mat4, Vec3, Vec4};
use std::num::NonZeroU32;

/// Both views: layer 0 the left eye, layer 1 the right.
pub(crate) const MASK: NonZeroU32 = NonZeroU32::new(0b11).unwrap();
/// Where the eyes' block starts in the frame uniform, in floats: right after
/// the words `frame.wgsl`'s `Frame` declares.
pub(crate) const EYE_WORDS: usize = crate::frame::FLOATS;
/// The eyes' block: two view-projections, two inverses, two camera positions
/// and two view-depth rows.
const EYE_FLOATS: usize = 2 * 16 + 2 * 16 + 2 * 4 + 2 * 4;
/// The frame uniform's size in floats: `Frame` and the eyes' block, which
/// only multiview modules declare (the buffer is this size for every frame).
pub(crate) const UNIFORM_FLOATS: usize = EYE_WORDS + EYE_FLOATS;

/// The eyes' block as multiview modules declare it, appended to `Frame` on
/// the line of its last member so diagnostics keep their line numbers.
const LAST_MEMBER: &str = "water_drift: vec4<f32>,";
const EYE_MEMBERS: &str = " eye_view_proj: array<mat4x4<f32>, 2>, \
eye_inverse_view_proj: array<mat4x4<f32>, 2>, eye_camera: array<vec4<f32>, 2>, \
eye_view_depth: array<vec4<f32>, 2>,";

/// What stays on the shared camera in a multiview module: the cascade a
/// fragment samples (its splits are the shared frustum's), and the light
/// cluster it reads (the light lists are binned in the shared camera's
/// screen tiles and depth slices, so the tile comes from projecting the
/// fragment through it rather than from its own pixel).
const SHARED: [(&str, &str); 3] = [
    (
        "let depth = dot(frame.view_depth, vec4(world, 1.0));",
        "let depth = dot(SHARED_FRAME.view_depth, vec4(world, 1.0));",
    ),
    (
        "let tile = min(vec2<u32>(max(frag * vec2(16.0, 9.0) / frame.logical_size.xy, vec2(0.0))), vec2(15u, 8u));",
        "let shared_clip = SHARED_FRAME.view_proj * vec4(world, 1.0); \
         let shared_frag = (shared_clip.xy / max(shared_clip.w, 1e-6) * vec2(0.5, -0.5) + 0.5) * frame.logical_size.xy; \
         let tile = min(vec2<u32>(max(shared_frag * vec2(16.0, 9.0) / frame.logical_size.xy, vec2(0.0))), vec2(15u, 8u));",
    ),
    (
        "let depth = max(dot(frame.view_depth, vec4(world, 1.0)), 1e-6);",
        "let depth = max(dot(SHARED_FRAME.view_depth, vec4(world, 1.0)), 1e-6);",
    ),
];

/// What each eye reads from its own row of the eyes' block.
const PER_EYE: [(&str, &str); 4] = [
    ("frame.view_proj", "frame.eye_view_proj[mv_eye]"),
    ("frame.inverse_view_proj", "frame.eye_inverse_view_proj[mv_eye]"),
    ("frame.camera_alpha.xyz", "frame.eye_camera[mv_eye].xyz"),
    ("frame.view_depth", "frame.eye_view_depth[mv_eye]"),
];

/// A module assembled for the frame pass (packed pieces, `frame.wgsl` first)
/// as its multiview form: every vertex and fragment entry point takes
/// `@builtin(view_index)` into a private `mv_eye`, `Frame` declares the eyes'
/// block, and what is the eye's reads that eye's row ([`PER_EYE`]); what
/// stays shared reads the shared camera ([`SHARED`]).
pub(crate) fn multiview_source(source: &str) -> String {
    assert_eq!(
        source.matches(LAST_MEMBER).count(),
        1,
        "a multiview module declares the frame uniform once"
    );
    let mut text = source.replace(LAST_MEMBER, &format!("{LAST_MEMBER}{EYE_MEMBERS}"));
    for (shared, marked) in SHARED {
        assert!(
            text.matches(shared).count() <= 1,
            "a shared read occurs once: {shared}"
        );
        text = text.replace(shared, marked);
    }
    for (mono, eye) in PER_EYE {
        text = text.replace(mono, eye);
    }
    text = text.replace("SHARED_FRAME.", "frame.");
    let mut out = entry_points(&text);
    out.push_str("\nvar<private> mv_eye: u32;\n");
    out
}

/// The tone pass's module in multiview form: it reads its eye's layer of the
/// resolved HDR image (`tone_layout`'s `D2Array`).
pub(crate) fn tone_source(source: &str) -> String {
    let text = multiview_source(source);
    assert!(text.contains("var hdr: texture_2d<f32>;"));
    text.replace("var hdr: texture_2d<f32>;", "var hdr: texture_2d_array<f32>;")
        .replace(
            "textureLoad(hdr, vec2<i32>(p.xy), 0)",
            "textureLoad(hdr, vec2<i32>(p.xy), mv_eye, 0)",
        )
}

/// Each `@vertex` and `@fragment` function takes the view index first and
/// stores it before anything else runs.
fn entry_points(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 1024);
    let mut rest = text;
    loop {
        let next = ["@vertex", "@fragment"]
            .iter()
            .filter_map(|stage| rest.find(stage))
            .min();
        let Some(at) = next else {
            out.push_str(rest);
            return out;
        };
        let name = at + rest[at..].find("fn ").expect("an entry point is a function");
        let open = name + rest[name..].find('(').expect("an entry point has parameters");
        let mut depth = 0;
        let mut close = open;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = close + rest[close..].find('{').expect("an entry point has a body");
        out.push_str(&rest[..=open]);
        out.push_str("@builtin(view_index) mv_index: u32, ");
        out.push_str(&rest[open + 1..=body]);
        out.push_str(" mv_eye = mv_index;");
        rest = &rest[body + 1..];
    }
}

/// Whether `device` draws stereo frames in one pass at `samples` per pixel:
/// two views, and multisampled array attachments above one sample.
pub(crate) fn supported(device: &wgpu::Device, samples: u32) -> bool {
    let features = device.features();
    features.contains(wgpu::Features::MULTIVIEW)
        && device.limits().max_multiview_view_count >= 2
        && (samples == 1 || features.contains(wgpu::Features::MULTISAMPLE_ARRAY))
}

/// One eye of a stereo frame, in world units: world → eye, eye → clip
/// (0–1 depth) and the eye's position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eye {
    /// World → eye.
    pub view: Mat4,
    /// Eye → clip, 0–1 depth.
    pub proj: Mat4,
    /// The eye's position in the world.
    pub camera: Vec3,
}

/// The eyes' block for the frame uniform ([`EYE_WORDS`]).
pub(crate) fn eye_words(eyes: &[Eye; 2]) -> [f32; EYE_FLOATS] {
    let mut data = [0.0; EYE_FLOATS];
    for (i, eye) in eyes.iter().enumerate() {
        let view_proj = eye.proj * eye.view;
        data[i * 16..i * 16 + 16].copy_from_slice(&view_proj.to_cols_array());
        data[32 + i * 16..48 + i * 16].copy_from_slice(&view_proj.inverse().to_cols_array());
        data[64 + i * 4..67 + i * 4].copy_from_slice(&eye.camera.to_array());
        data[72 + i * 4..76 + i * 4].copy_from_slice(&(-eye.view.row(2)).to_array());
    }
    data
}

/// The shared camera of a stereo frame: a frustum that holds both eyes',
/// for everything computed once for both (cull, cascades, light lists, the
/// translucent order). It looks along the left eye's axes from an apex just
/// behind both eyes (the eyes' frusta are each inside it: their origins are,
/// and their sides are no wider), with the widest of the eyes' tangents on
/// each side, the nearest near plane and the farthest far plane. Returns
/// its world → view and view → clip (0–1 depth; infinite far when an eye's
/// is), and the centre eye's position for distances (levels of detail).
pub fn shared_camera(eyes: &[Eye; 2]) -> (Mat4, Mat4, Vec3) {
    let reference = eyes[0].view;
    // Each eye's origin, and its frustum's corners at the near and far
    // planes, in the reference eye's space.
    let mut tangents = [0f32; 4]; // left, right, down, up
    let mut origins = [Vec3::ZERO; 2];
    let mut nears = [0f32; 2];
    let mut fars = [f32::INFINITY; 2];
    for (i, eye) in eyes.iter().enumerate() {
        let inverse = (eye.proj * eye.view).inverse();
        let origin = reference.transform_point3(eye.camera);
        origins[i] = origin;
        let mut near = f32::INFINITY;
        let mut far = 0f32;
        for (x, y) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
            let at = |z: f32| {
                let p = inverse * Vec4::new(x, y, z, 1.0);
                reference.transform_point3(p.truncate() / p.w)
            };
            let d = at(0.0) - origin;
            let forward = (-d.z).max(1e-9);
            tangents[0] = tangents[0].max(-d.x / forward);
            tangents[1] = tangents[1].max(d.x / forward);
            tangents[2] = tangents[2].max(-d.y / forward);
            tangents[3] = tangents[3].max(d.y / forward);
            near = near.min(forward);
            let f = at(1.0) - origin;
            far = far.max(if f.is_finite() { -f.z } else { f32::INFINITY });
        }
        nears[i] = near;
        fars[i] = if far.is_finite() && far < 1e7 {
            far
        } else {
            f32::INFINITY
        };
    }
    let [left, right, down, up] = tangents.map(|t| t.max(1e-4));
    // The apex: centred between the eyes, and behind both far enough that
    // each lies inside every side plane.
    let centre = (origins[0] + origins[1]) * 0.5;
    let mut apex_z = origins[0].z.max(origins[1].z);
    for o in origins {
        apex_z = apex_z
            .max(o.z + (centre.x - o.x) / left)
            .max(o.z + (o.x - centre.x) / right)
            .max(o.z + (centre.y - o.y) / down)
            .max(o.z + (o.y - centre.y) / up);
    }
    let apex = Vec3::new(centre.x, centre.y, apex_z);
    let near = (0..2)
        .map(|i| apex_z - origins[i].z + nears[i])
        .fold(f32::INFINITY, f32::min);
    let far = (0..2)
        .map(|i| apex_z - origins[i].z + fars[i])
        .fold(0f32, f32::max);
    let view = Mat4::from_translation(-apex) * reference;
    let proj = frustum(left * near, right * near, down * near, up * near, near, far);
    let position = (eyes[0].camera + eyes[1].camera) * 0.5;
    (view, proj, position)
}

/// An off-centre right-handed perspective projection, looking down −Z, onto
/// 0–1 depth: `left`, `right`, `bottom`, `top` are distances on the near
/// plane (left and bottom positive to the left and down); `far` may be
/// infinite.
fn frustum(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Mat4 {
    let (w, h) = (right + left, top + bottom);
    let (a, b) = if far.is_finite() {
        (far / (near - far), near * far / (near - far))
    } else {
        (-1.0, -near)
    };
    Mat4::from_cols(
        Vec4::new(2.0 * near / w, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 * near / h, 0.0, 0.0),
        Vec4::new((right - left) / w, (top - bottom) / h, a, -1.0),
        Vec4::new(0.0, 0.0, b, 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eyes() -> [Eye; 2] {
        let reference = glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 1.6, 0.0), Vec3::new(0.0, 0.9, -1.0), Vec3::Y);
        // A headset's asymmetric eyes, 64 mm apart.
        let proj = |l: f32, r: f32| frustum(l * 0.05, r * 0.05, 0.9 * 0.05, 0.8 * 0.05, 0.05, 100.0);
        let eye = |x: f32, p: Mat4| {
            let view = Mat4::from_translation(Vec3::new(-x, 0.0, 0.0)) * reference;
            Eye { view, proj: p, camera: view.inverse().w_axis.truncate() }
        };
        [eye(-0.032, proj(1.0, 0.8)), eye(0.032, proj(0.8, 1.0))]
    }

    #[test]
    fn the_shared_frustum_holds_both_eyes() {
        let eyes = eyes();
        let (view, proj, centre) = shared_camera(&eyes);
        let shared = proj * view;
        assert!((centre - (eyes[0].camera + eyes[1].camera) * 0.5).length() < 1e-6);
        for eye in &eyes {
            let inverse = (eye.proj * eye.view).inverse();
            for x in [-1.0, -0.3, 1.0] {
                for y in [-1.0, 0.2, 1.0] {
                    for z in [0.0, 0.5, 0.999] {
                        let p = inverse.project_point3(Vec3::new(x, y, z));
                        let c = shared * p.extend(1.0);
                        let n = c.truncate() / c.w;
                        assert!(c.w > 0.0, "{p} is in front of the shared apex");
                        assert!(n.x.abs() <= 1.0 + 1e-4 && n.y.abs() <= 1.0 + 1e-4, "{p} -> {n}");
                        assert!((-1e-4..=1.0 + 1e-4).contains(&n.z), "{p} -> depth {}", n.z);
                    }
                }
            }
        }
    }

    #[test]
    fn eye_words_follow_the_frame_uniform() {
        let eyes = eyes();
        let words = eye_words(&eyes);
        assert_eq!(&words[16..32], &(eyes[1].proj * eyes[1].view).to_cols_array());
        assert_eq!(&words[68..71], &eyes[1].camera.to_array());
        assert_eq!(UNIFORM_FLOATS, 296);
    }

    #[test]
    fn multiview_modules_validate() {
        let sources = crate::pipeline::multiview_sources();
        assert!(sources.len() >= 8);
        for (name, source) in sources {
            assert!(source.contains("mv_eye = mv_index;"), "{name}");
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
}
