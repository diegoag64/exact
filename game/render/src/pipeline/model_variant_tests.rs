use super::*;

#[test]
fn first_primitive_draw_uses_fallback_while_worker_specializes() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let p = Pipelines::with_samples(&gpu.device, wgpu::TextureFormat::Rgba8Unorm, 1);
    for key in [LEAN | SINGLE_CASCADE, LEAN | SINGLE_CASCADE | 4] {
        let selected = p.forward_variant(&gpu.device, key);
        let index = 2 * usize::from(key & 4 != 0);
        assert_eq!(selected, p.primitive_fallback[index].get().unwrap());
        let specialized = p.variants[key].wait();
        assert_eq!(specialized, p.forward_variant(&gpu.device, key));
    }
}

#[test]
fn model_preparation_compiles_only_requested_variants_and_reuses_them() {
    let Some(gpu) = crate::test_device::device_or_skip(exact_gpu::fixture::device()) else {
        return;
    };
    let mut p = Pipelines::with_samples(&gpu.device, wgpu::TextureFormat::Rgba8Unorm, 1);
    p.prepare_model(&gpu.device);
    let family = p.models.as_ref().unwrap();
    assert!(family.forward.iter().all(|v| v.get().is_none()));
    assert!(family.shadow.iter().all(|v| v.get().is_none()));
    for key in [32 + 64, 3 + 32 + 64 + 128 + 256, 3 + 8 + 16] {
        let selected = p.model_variant(&gpu.device, key, false);
        let class = key & (4 | 8 | 16 | 64 | PREPASS);
        assert_eq!(
            selected,
            family.fallback[class | (key & 32)].get().unwrap(),
            "even the first draw of a family uses its ready fallback"
        );
        let pipeline = family.forward[key].wait();
        assert_eq!(pipeline, p.model_variant(&gpu.device, key, false));
    }
    for key in [0, 4 + 16 + 32] {
        let pipeline = p.model_variant(&gpu.device, key, true).clone();
        assert_eq!(&pipeline, p.model_variant(&gpu.device, key, true));
    }
    assert_eq!(
        family.forward.iter().filter(|v| v.get().is_some()).count(),
        3
    );
    assert_eq!(
        family.shadow.iter().filter(|v| v.get().is_some()).count(),
        3
    );
}
