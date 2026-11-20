//! Stereo frames (`XrStereo`): both eyes drawn in one multiview pass match
//! each eye drawn on its own, layer for layer.
#![cfg(not(target_arch = "wasm32"))]
use exact_game::*;
use exact_game_render::WorldSurface;
use exact_gpu::{fixture, wgpu, Frame, Surface, XrStereo};

#[derive(Default, Args)]
struct Settings {
    water: bool,
    one_sample: bool,
}
struct Diorama;
impl Game for Diorama {
    const ID: &'static str = "stereo-diorama";
    type Args = Settings;
    fn samples(a: &Settings) -> SceneSamples {
        if a.one_sample {
            SceneSamples::One
        } else {
            SceneSamples::Four
        }
    }
    fn setup(w: &mut World, a: &Settings) {
        w.insert_resource(Environment {
            zenith: [0.2, 0.35, 0.7],
            horizon: [0.7, 0.75, 0.8],
            ground: [0.3, 0.25, 0.2],
            bloom: None,
            fog: Some(Fog::linear(14., 60.)),
            exposure: 0.8,
            ..Default::default()
        });
        if a.water {
            w.insert_resource(Water::at(-0.2));
        }
        w.spawn((
            Transform::default().looking_at(Vec3::new(-0.4, -1.0, -0.3), Vec3::Y),
            DirectionalLight {
                illuminance: 4000.,
                ..Default::default()
            },
        ));
        w.spawn((
            Transform::default().looking_at(Vec3::new(0.5, -0.4, 0.6), Vec3::Y),
            DirectionalLight {
                illuminance: 2000.,
                shadows: false,
                color: [0.6, 0.7, 1.0],
            },
        ));
        w.spawn((
            Transform::at(0., 0., 0.),
            Mesh::plane(6., 6.),
            Material::rgb(0.55, 0.6, 0.45),
        ));
        for (i, (x, z)) in [(-1.5, -1.0), (0.0, -2.0), (1.4, -0.4), (-0.4, 0.9), (2.2, -3.5)]
            .into_iter()
            .enumerate()
        {
            w.spawn((
                Transform::at(x, 0.4 + 0.15 * i as f32, z),
                Mesh::cube(0.6 + 0.1 * i as f32),
                Material::rgb(0.8 - 0.1 * i as f32, 0.3 + 0.1 * i as f32, 0.4),
            ));
        }
        w.spawn((
            Transform::at(0.6, 0.5, 0.4),
            Mesh::sphere(0.45),
            Material::rgb(0.9, 0.85, 0.8),
        ));
        w.spawn((
            Transform::at(-0.7, 0.5, 0.1),
            PointLight {
                color: [1.0, 0.4, 0.1],
                intensity: 6000.,
                range: 3.,
                ..Default::default()
            },
        ));
        // A fountain of dots: quads in the translucent pass.
        w.spawn((
            Transform::at(0.9, 0.2, 1.6),
            Emitter {
                rate: 60.,
                lifetime: 2.,
                speed: 1.2,
                size: [0.3, 0.1],
                color: [[1.0, 0.85, 0.3, 0.9], [1.0, 0.3, 0.1, 0.0]],
                ..Default::default()
            },
        ));
        w.spawn((Transform::at(0., 2., 6.), Camera::default()));
    }
    fn tick(_: &mut World, _: &Input, _: &Settings) {}
}

/// An off-centre perspective onto 0–1 depth from tangents on each side.
fn frustum(left: f32, right: f32, down: f32, up: f32, near: f32, far: f32) -> Mat4 {
    let (w, h) = (right + left, up + down);
    Mat4::from_cols(
        Vec4::new(2.0 / w, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / h, 0.0, 0.0),
        Vec4::new(
            (right - left) / w,
            (up - down) / h,
            far / (near - far),
            -1.0,
        ),
        Vec4::new(0.0, 0.0, near * far / (near - far), 0.0),
    )
}

/// A headset's two eyes, 64 mm apart, each with a Quest-like asymmetric field.
fn eyes() -> ([[f32; 16]; 2], [[f32; 16]; 2]) {
    let head = glam::camera::rh::view::look_at_mat4(
        Vec3::new(0.3, 2.2, 4.5),
        Vec3::new(0., 0.4, -0.5),
        Vec3::Y,
    );
    let view = |x: f32| (Mat4::from_translation(Vec3::new(-x, 0., 0.)) * head).to_cols_array();
    let (outer, inner, down, up) = (1.0, 0.84, 0.97, 0.9);
    (
        [view(-0.032), view(0.032)],
        [
            frustum(outer, inner, down, up, 0.05, 100.).to_cols_array(),
            frustum(inner, outer, down, up, 0.05, 100.).to_cols_array(),
        ],
    )
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn stereo_target(gpu: &exact_gpu::Gpu, size: (u32, u32)) -> XrStereo {
    let (views, projections) = eyes();
    XrStereo {
        target: gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stereo swapchain"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 2,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }),
        size,
        views,
        projections,
    }
}

/// The compared frames' time: the first frame starts the clock at zero, the
/// next runs the world (and its fountain) to here.
const NOW_MS: f64 = 600.;
fn frame(size: (u32, u32)) -> Frame {
    Frame {
        width: size.0 as f32,
        height: size.1 as f32,
        scale: 1.,
        now_ms: NOW_MS,
        seekable: true,
        period_ms: 0.,
        children_generation: 0,
        shader_generation: 0,
        headroom: 1.,
    }
}

struct Difference {
    mean: f64,
    max: u8,
    /// Share of pixels whose largest channel differs by more than 24.
    off: f64,
}

fn difference(a: &fixture::Pixels, b: &fixture::Pixels) -> Difference {
    let (mut sum, mut max, mut off) = (0u64, 0u8, 0usize);
    let pixels = a.data.as_chunks::<4>().0.iter().zip(b.data.as_chunks::<4>().0);
    for (p, q) in pixels {
        let d = (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap();
        sum += u64::from(d);
        max = max.max(d);
        off += usize::from(d > 24);
    }
    let n = (a.width * a.height) as f64;
    Difference {
        mean: sum as f64 / n,
        max,
        off: off as f64 / n,
    }
}

#[test]
fn one_multiview_pass_draws_what_each_eye_draws_on_its_own() {
    let extra = wgpu::Features::MULTIVIEW | wgpu::Features::MULTISAMPLE_ARRAY;
    let Some(gpu) = fixture::device_or_skip(fixture::device_with(extra)) else {
        return;
    };
    if !gpu.device.features().contains(extra) {
        eprintln!("skipped: {:?} has no multiview", gpu.adapter.get_info().name);
        return;
    }
    let size = (320, 288);
    for (water, one_sample) in [(false, false), (true, false), (false, true)] {
        let case = format!(
            "{}{}",
            if water { "water" } else { "land" },
            if one_sample { "-1x" } else { "-4x" }
        );
        let mut surface = WorldSurface::<Diorama>::default();
        surface
            .bind(&[Value::Bool(water), Value::Bool(one_sample)], None)
            .unwrap();
        let stereo = stereo_target(&gpu, size);
        let read = |s: &XrStereo| [0, 1].map(|l| fixture::read_layer(&gpu, &s.target, l).unwrap());
        // Each eye on its own (the per-eye path), then both in one pass,
        // then each on its own again: the same frame three times.
        let per_eye_at = |surface: &mut WorldSurface<Diorama>, now_ms: f64| {
            let views = stereo.layer_views(FORMAT);
            let frame = Frame {
                now_ms,
                ..frame(size)
            };
            surface
                .render_xr(&frame, &gpu.device, &gpu.queue, &views, FORMAT)
                .unwrap();
            surface.submitted();
            assert!(surface.error().is_none(), "{:?}", surface.error());
            read(&stereo)
        };
        per_eye_at(&mut surface, 0.);
        let per_eye = |surface: &mut WorldSurface<Diorama>| per_eye_at(surface, NOW_MS);
        let first = per_eye(&mut surface);
        assert!(
            surface.frame_stats().instances > 7,
            "{case}: the fountain's dots draw"
        );
        let before = surface.frame_stats().stereo_frames;
        surface
            .render_xr_stereo(&frame(size), &gpu.device, &gpu.queue, &stereo, FORMAT)
            .unwrap();
        surface.submitted();
        assert!(surface.error().is_none(), "{:?}", surface.error());
        assert_eq!(
            surface.frame_stats().stereo_frames,
            before + 1,
            "{case}: drawn in one multiview pass"
        );
        let both = read(&stereo);
        let again = per_eye(&mut surface);
        for eye in 0..2 {
            let name = ["left", "right"][eye];
            first[eye].save(&format!("stereo-{case}-{name}-per-eye"));
            both[eye].save(&format!("stereo-{case}-{name}-multiview"));
            let steady = difference(&first[eye], &again[eye]);
            assert_eq!(steady.max, 0, "{case} {name}: the per-eye frame repeats");
            let d = difference(&first[eye], &both[eye]);
            eprintln!(
                "{case} {name}: mean {:.3}, max {}, {:.3}% of pixels off by more than 24",
                d.mean,
                d.max,
                d.off * 100.
            );
            assert!(
                d.mean < 1.0 && d.off < 0.005,
                "{case} {name}: multiview differs from per-eye: mean {:.3}, off {:.4}",
                d.mean,
                d.off
            );
        }
        // The eyes see the scene from 64 mm apart: the layers differ.
        let parallax = difference(&both[0], &both[1]);
        assert!(parallax.mean > 1.0, "{case}: the layers are the same picture");
    }
}

/// Rough cost of a stereo frame: CPU time to encode and submit, and draws,
/// one multiview pass against two per-eye passes (`--ignored --nocapture`).
#[test]
#[ignore]
fn stereo_frame_cost() {
    let extra = wgpu::Features::MULTIVIEW | wgpu::Features::MULTISAMPLE_ARRAY;
    let Some(gpu) = fixture::device_or_skip(fixture::device_with(extra)) else {
        return;
    };
    let size = (1680, 1760);
    for (water, one_sample) in [(false, false), (true, false), (false, true)] {
        let mut surface = WorldSurface::<Diorama>::default();
        surface
            .bind(&[Value::Bool(water), Value::Bool(one_sample)], None)
            .unwrap();
        let stereo = stereo_target(&gpu, size);
        let views = stereo.layer_views(FORMAT);
        let mut run = |multiview: bool| {
            let mut times = Vec::new();
            let mut draws = 0;
            for i in 0..40 {
                let start = std::time::Instant::now();
                if multiview {
                    surface.render_xr_stereo(&frame(size), &gpu.device, &gpu.queue, &stereo, FORMAT);
                } else {
                    surface.render_xr(&frame(size), &gpu.device, &gpu.queue, &views, FORMAT);
                }
                surface.submitted();
                let cpu = start.elapsed().as_secs_f64() * 1e3;
                gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let total = start.elapsed().as_secs_f64() * 1e3;
                if i >= 10 {
                    times.push((cpu, total));
                }
                draws = surface.frame_stats().draws * if multiview { 1 } else { 2 };
            }
            times.sort_by(|a, b| a.0.total_cmp(&b.0));
            let median = times[times.len() / 2];
            (median, draws)
        };
        let ((eye_cpu, eye_total), eye_draws) = run(false);
        let ((mv_cpu, mv_total), mv_draws) = run(true);
        eprintln!(
            "water={water} 1x={one_sample} {}x{} per eye: per-eye {eye_draws} draws, {eye_cpu:.2} ms CPU \
             ({eye_total:.2} ms with GPU); multiview {mv_draws} draws, {mv_cpu:.2} ms CPU ({mv_total:.2} ms with GPU)",
            size.0, size.1
        );
    }
}
