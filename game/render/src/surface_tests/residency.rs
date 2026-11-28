use super::*;
struct Fox;
impl Game for Fox {
    type Args = ();
    const ID: &'static str = "residency-fox";
    const ASSETS: &'static [&'static str] = &["fox.model"];
    fn setup(w: &mut World, _: &()) {
        w.spawn((
            exact_game::Transform::default(),
            exact_game::Mesh::asset("fox.model"),
        ));
    }
    fn tick(_: &mut World, _: &exact_game::Input, _: &()) {}
}

struct Cosmetic;
impl Game for Cosmetic {
    type Args = ();
    const ID: &'static str = "residency-cosmetic";
    fn setup(w: &mut World, _: &()) {
        w.spawn_named(
            "hero",
            (
                exact_game::Transform::default(),
                exact_game::Mesh::asset("hero.model"),
            ),
        );
        w.spawn((
            exact_game::Transform::at(2.8, 2., 3.8)
                .looking_at(exact_game::Vec3::ZERO, exact_game::Vec3::Y),
            exact_game::Camera::default(),
        ));
    }
    fn tick(_: &mut World, _: &exact_game::Input, _: &()) {}
}
fn frame() -> Frame {
    Frame {
        width: 32.,
        height: 32.,
        scale: 1.,
        now_ms: 0.,
        seekable: true,
        period_ms: 0.,
        children_generation: 0,
        shader_generation: 0,
        headroom: 1.0,
    }
}
fn model() -> exact_game::asset::Model {
    exact_game::bin::from_slice(include_bytes!("../../../bake/tests/fixtures/crate.model")).unwrap()
}
#[test]
#[ignore = "requires two real GPU devices; run explicitly on a GPU host"]
fn surface_lifecycle_rebuilds_textured_draws_after_prepared_retry_loss() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let mut s = WorldSurface::<Cosmetic, crate::ModelExecutor, true>::default();
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.bind(&[], None).unwrap();
    let model = model();
    let bytes = exact_game::asset::encode_model(&model);
    let tex = include_bytes!("../../../bake/tests/fixtures/crate/0-srgb-straight.tex");
    s.assets();
    s.asset("hero.model", Ok(&bytes));
    s.asset(&model.textures[0], Ok(tex));
    let (before, _) = exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    let work = s.render.as_ref().unwrap().0.residency_work().json();
    assert!(!s.render.as_ref().unwrap().0.models.records.is_empty());
    s.device_lost();
    // Failed adapter retries can finish redelivery before device_ready.
    s.device_lost();
    s.assets();
    s.asset(&model.textures[0], Ok(tex));
    s.device_lost(); // another failed retry must preserve delivered bytes too
    let Some(replacement) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.prepare_assets(
        &replacement.device,
        &replacement.queue,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    s.device_lost(); // A failed retry consumed CPU texture bytes into its renderer.
    let changes = s.assets();
    assert!(changes.retired.contains(&model.textures[0]));
    assert!(changes.requests.contains(&model.textures[0]));
    s.asset(&model.textures[0], Ok(tex));
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.prepare_assets(
        &replacement.device,
        &replacement.queue,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let (after, _) = exact_gpu::fixture::render(&replacement, &mut s, &frame()).unwrap();
    assert!(
        before == after,
        "replacement device must draw the same model pixels"
    );
    assert_eq!(work, s.render.as_ref().unwrap().0.residency_work().json());
}

#[test]
#[ignore = "requires a real GPU device; run cargo test -p exact-game-render retired_model_stays_hidden -- --ignored"]
fn retired_model_stays_hidden_until_changed_dependency_closure_is_prepared() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let mut s = WorldSurface::<Cosmetic, crate::ModelExecutor, true>::default();
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.bind(&[], None).unwrap();
    let mut model = model();
    let texture = include_bytes!("../../../bake/tests/fixtures/crate/0-srgb-straight.tex");
    s.assets();
    s.asset("hero.model", Ok(&exact_game::asset::encode_model(&model)));
    s.asset(&model.textures[0], Ok(texture));
    let (before, _) = exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    assert!(!s.render.as_ref().unwrap().0.models.records.is_empty());
    // A model the renderer draws is held (`Sim::hold_assets`): it retires
    // once a frame draws something else in its place.
    *s.sim
        .as_ref()
        .unwrap()
        .world()
        .get_mut::<exact_game::Mesh>("hero")
        .unwrap() = exact_game::Mesh::cube(1.);
    s.dirty = true; // the write bypassed a tick, which would feed it
    exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    s.assets();
    *s.sim
        .as_ref()
        .unwrap()
        .world()
        .get_mut::<exact_game::Mesh>("hero")
        .unwrap() = exact_game::Mesh::asset("hero.model");
    s.assets();
    exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    assert!(
        s.render.as_ref().unwrap().0.models.records.is_empty(),
        "retired geometry leaked before redelivery"
    );
    model.materials[0].base_color = [1., 0., 0., 1.];
    s.asset("hero.model", Ok(&exact_game::asset::encode_model(&model)));
    exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    assert!(
        s.render.as_ref().unwrap().0.models.records.is_empty(),
        "model leaked before its texture arrived"
    );
    s.asset(&model.textures[0], Ok(texture));
    let (after, _) = exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    assert!(
        before != after,
        "first drawable frame must use changed material bytes"
    );
    assert_eq!(
        s.render.as_ref().unwrap().0.models.materials.len(),
        model.materials.len(),
        "replacement reclaims old materials"
    );
}

#[test]
fn identical_redelivery_survives_entry_and_post_acceptance_budget_compaction() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let mut s = WorldSurface::<Cosmetic, crate::ModelExecutor, true>::default();
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.bind(&[], None).unwrap();
    let mut model = model();
    // Character-sized resident, already retired when the oversized cache enters preparation.
    model.meshes[0].positions.resize(80_000 * 3, 0.);
    model.meshes[0].normals.resize(80_000 * 3, 0.);
    model.meshes[0].uvs.resize(80_000 * 2, 0.);
    let bytes = exact_game::asset::encode_model(&model);
    let tex = include_bytes!("../../../bake/tests/fixtures/crate/0-srgb-straight.tex");
    s.assets();
    s.asset("hero.model", Ok(&bytes));
    s.asset(&model.textures[0], Ok(tex));
    let (before, _) = exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    let handles = s.render.as_ref().unwrap().0.models.loaded["hero.model"]
        .nodes
        .clone();
    // A model the renderer draws is held (`Sim::hold_assets`): it retires
    // once a frame draws something else in its place.
    for (mesh, draw) in [
        (exact_game::Mesh::cube(1.), true),
        (exact_game::Mesh::asset("hero.model"), false),
    ] {
        *s.sim
            .as_ref()
            .unwrap()
            .world()
            .get_mut::<exact_game::Mesh>("hero")
            .unwrap() = mesh;
        if draw {
            s.dirty = true; // the write bypassed a tick, which would feed it
            exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
        }
        s.assets();
    }
    let retired = exact_game::asset::TextureData {
        width: 1024,
        height: 1024,
        mips: (0..11)
            .map(|level| vec![255; (1024usize >> level).pow(2) * 4])
            .collect(),
        ..Default::default()
    };
    let r = &mut s.render.as_mut().unwrap().0;
    for i in 0..13 {
        let name = format!("retired-{i}.tex");
        r.add_texture(&name, &retired).unwrap();
        r.retire_texture(&name);
    }
    let work = r.residency_work();
    // The new peer grows the mesh arena during this preparation pass. Its
    // unused capacity plus retired textures crosses the real 64 MiB budget.
    let mut peer = model.clone();
    let m = &mut peer.meshes[0];
    m.positions.resize(600_000 * 3, 0.);
    m.normals.resize(600_000 * 3, 0.);
    m.uvs.resize(600_000 * 2, 0.);
    s.sim.as_mut().unwrap().world_mut().spawn((
        exact_game::Transform::at(100., 0., 0.),
        exact_game::Mesh::asset("peer.model"),
    ));
    s.assets();
    s.asset("hero.model", Ok(&bytes));
    s.asset("peer.model", Ok(&exact_game::asset::encode_model(&peer)));
    let live = s
        .sim
        .as_ref()
        .unwrap()
        .presentation_assets()
        .map(str::to_owned)
        .collect();
    assert!(s.render.as_ref().unwrap().0.retired_bytes(&live) > RETIRED_BUDGET);
    s.prepare_assets(&gpu.device, &gpu.queue, wgpu::TextureFormat::Rgba8Unorm);
    assert!(!s.sim.as_ref().unwrap().model_prepared("hero.model"));
    let r = &s.render.as_ref().unwrap().0;
    assert!(
        r.retired_bytes(&live) <= RETIRED_BUDGET,
        "post-preparation compaction ran"
    );
    assert!(
        r.models.loaded.contains_key("hero.model"),
        "accepted identical model was discarded"
    );
    assert_eq!(r.models.loaded["hero.model"].nodes, handles);
    assert_eq!(r.residency_work().since(work).texture_uploads, 0);
    assert_eq!(
        r.residency_work().since(work).mesh_uploads,
        peer.meshes.len() as u64
    );
    s.asset(&model.textures[0], Ok(tex));
    let (after, _) = exact_gpu::fixture::render(&gpu, &mut s, &frame()).unwrap();
    assert_eq!(
        before, after,
        "same hero is drawable without duplicate upload"
    );
    assert_eq!(
        s.render
            .as_ref()
            .unwrap()
            .0
            .residency_work()
            .since(work)
            .mesh_uploads,
        peer.meshes.len() as u64,
        "closing the pending texture dependency must not upload the hero again"
    );
}

#[test]
fn twenty_unique_models_bound_retirement_and_hash_only_at_delivery() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let mut s = WorldSurface::<Cosmetic, crate::ModelExecutor, true>::default();
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.bind(&[], None).unwrap();
    let mut model = model();
    let texture = exact_game::asset::TextureData {
        width: 1024,
        height: 1024,
        mips: (0..11)
            .map(|level| vec![255; (1024usize >> level).max(1).pow(2) * 4])
            .collect(),
        ..Default::default()
    };
    let bytes = exact_game::bin::to_vec(&texture);
    let hashes_before = crate::models::model_hash_count();
    for i in 0..20 {
        let name = format!("unique-{i}.model");
        model.textures[0] = format!("unique-{i}.tex");
        *s.sim
            .as_ref()
            .unwrap()
            .world()
            .get_mut::<exact_game::Mesh>("hero")
            .unwrap() = exact_game::Mesh::asset(&name);
        s.assets();
        s.asset(&name, Ok(&exact_game::asset::encode_model(&model)));
        s.asset(&model.textures[0], Ok(&bytes));
        s.prepare_assets(&gpu.device, &gpu.queue, wgpu::TextureFormat::Rgba8Unorm);
        let live = s
            .sim
            .as_ref()
            .unwrap()
            .presentation_assets()
            .map(str::to_owned)
            .collect();
        assert!(s.render.as_ref().unwrap().0.retired_bytes(&live) <= RETIRED_BUDGET);
        assert_eq!(crate::models::model_hash_count(), hashes_before + i + 1);
        // Dirty texture redelivery must never hash any resident model again.
        s.asset(&model.textures[0], Ok(&bytes));
        s.prepare_assets(&gpu.device, &gpu.queue, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(crate::models::model_hash_count(), hashes_before + i + 1);
    }
    assert!(s.render.as_ref().unwrap().0.models.loaded.len() < 20);
    let json = s.agent(r#"{"op":"state"}"#).unwrap();
    assert!(json.contains(r#""modelSkinBufferReallocations":"#));
    assert!(!json.contains(r#""bufferReallocations":"#));
}
#[test]
fn ready_reasons_name_declaration_and_render_failures() {
    let mut s = WorldSurface::<Fox, crate::ModelExecutor, true>::default();
    s.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    s.bind(&[], None).unwrap();
    s.asset("fox.model", Err(AssetError::Missing));
    let state = s.agent(r#"{"op":"state"}"#).unwrap();
    assert!(
        state
            .split("\"readyReasons\":")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .contains("fox.model"),
        "{state}"
    );
    s.error = Some(SurfaceError("named render failure".into()));
    let state = s.agent(r#"{"op":"state"}"#).unwrap();
    assert!(
        state
            .split("\"readyReasons\":")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .contains("named render failure"),
        "{state}"
    );
    assert!(state.contains("\"ready\":false"));
}
#[test]
fn fox_restore_and_paranoid_save_keep_assets_pipelines_and_palette_capacity() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let mut surface = WorldSurface::<Fox, crate::ModelExecutor, true>::default();
    surface.device_ready(wgpu::Features::empty(), wgpu::Limits::downlevel_defaults());
    surface.bind(&[], None).unwrap();
    surface.asset(
        "fox.model",
        Ok(&exact_game::asset::encode_model(
            &crate::test_model::skinned_model(),
        )),
    );
    surface.asset(
        "fox/0-srgb-straight.tex",
        Ok(include_bytes!(
            "../../../bake/tests/fixtures/crate/0-srgb-straight.tex"
        )),
    );
    let mut frame = Frame {
        width: 16.,
        height: 16.,
        scale: 1.,
        now_ms: 0.,
        seekable: true,
        period_ms: 0.,
        children_generation: 0,
        shader_generation: 0,
        headroom: 1.0,
    };
    exact_gpu::fixture::render(&gpu, &mut surface, &frame).unwrap();
    surface
        .render
        .as_ref()
        .unwrap()
        .0
        .pipelines
        .wait_specializations();
    let ready_work = surface.render.as_ref().unwrap().0.residency_work();
    let before = surface.render.as_ref().unwrap().0.residency_work().json();
    let saved = surface.carry().unwrap().unwrap();
    for mode in [Restore::Open, Restore::Carry] {
        surface.restore(&saved, mode).unwrap();
        exact_gpu::fixture::render(&gpu, &mut surface, &frame).unwrap();
        assert_eq!(
            before,
            surface.render.as_ref().unwrap().0.residency_work().json()
        );
        assert_eq!(surface.carry().unwrap().unwrap(), saved);
    }
    let mut texture: exact_game::asset::TextureData = exact_game::bin::from_slice(include_bytes!(
        "../../../bake/tests/fixtures/crate/0-srgb-straight.tex"
    ))
    .unwrap();
    texture.mips[0][0] ^= 127;
    let changed = exact_game::bin::to_vec(&texture);
    surface.asset("fox/0-srgb-straight.tex", Ok(&changed));
    surface.restore(&saved, Restore::Carry).unwrap();
    exact_gpu::fixture::render(&gpu, &mut surface, &frame).unwrap();
    let work = surface.render.as_ref().unwrap().0.residency_work();
    let delta = work.since(ready_work);
    assert_eq!(delta.texture_uploads, 1);
    assert_eq!(delta.mesh_uploads, 0);
    assert_eq!(delta.pipeline_creations, 0);
    assert_eq!(delta.buffer_reallocations, 0);
    surface.asset("fox/0-srgb-straight.tex", Ok(&changed));
    exact_gpu::fixture::render(&gpu, &mut surface, &frame).unwrap();
    assert_eq!(
        work.json(),
        surface.render.as_ref().unwrap().0.residency_work().json()
    );
    let before = work.json();
    surface.sim = surface
        .sim
        .take()
        .map(|s| s.paranoid(exact_game::Paranoid::Save));
    for tick in 1..=4 {
        frame.now_ms = tick as f64 * 1000. / 60.;
        exact_gpu::fixture::render(&gpu, &mut surface, &frame).unwrap();
        assert_eq!(
            before,
            surface.render.as_ref().unwrap().0.residency_work().json()
        );
    }
    assert!(surface.render.as_ref().unwrap().0.models.skinning.is_some());
    let mut model = surface
        .sim
        .as_ref()
        .unwrap()
        .presentation_models()
        .next()
        .unwrap()
        .1
        .clone();
    let old = &surface.render.as_ref().unwrap().0;
    let sizes = (old.meshes.len(), old.models.materials.len());
    let skins: Vec<_> = old.models.loaded["fox.model"]
        .nodes
        .iter()
        .map(|n| n.3)
        .collect();
    for _ in 0..3 {
        model.meshes[0].positions[0] += 0.01;
        surface.asset("fox.model", Ok(&exact_game::asset::encode_model(&model)));
        exact_gpu::fixture::render(&gpu, &mut surface, &frame).unwrap();
        let renderer = &surface.render.as_ref().unwrap().0;
        assert_eq!(
            (renderer.meshes.len(), renderer.models.materials.len()),
            sizes
        );
        assert_eq!(
            renderer.models.loaded["fox.model"]
                .nodes
                .iter()
                .map(|n| n.3)
                .collect::<Vec<_>>(),
            skins,
            "same-name replacement reclaims skin templates too"
        );
    }
}
