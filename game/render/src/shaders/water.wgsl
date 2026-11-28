// The water's surface (water.rs), seen from either side: one full-screen
// triangle that meets each pixel's ray with the plane at the water's height,
// writes that point's depth and shades it. Appended to sky.wgsl, whose
// `sky_radiance` it shows through the surface, and caustics.wgsl.
const TAU: f32 = 6.283185307179586;
// The surface's upward normal at `xz`: the slopes of two summed waves, the
// second at 0.61 of the wavelength crossing the first at 110°. Waves a pixel
// cannot resolve (`footprint` metres across) fade out rather than shimmer.
fn wave_normal(xz: vec2<f32>, footprint: f32) -> vec3<f32> {
    let wavelength = max(frame.water_waves.x, 0.01);
    let k1 = TAU / wavelength;
    let k2 = k1 / 0.61;
    let d1 = vec2(1.0, 0.0);
    let d2 = vec2(-0.342, 0.940);
    let amplitude = frame.water_color.w * (1.0 - smoothstep(0.15, 0.6, footprint / (0.61 * wavelength)));
    let slope = d1 * (amplitude * k1 * cos(k1 * dot(d1, xz) - frame.water_waves.y))
        + d2 * (amplitude * k2 * cos(k2 * dot(d2, xz) - frame.water_waves.z));
    return normalize(vec3(-slope.x, 1.0, -slope.y));
}
// Schlick's reflectance of water at normal incidence (0.02), for the cosine
// on the air side, the larger angle.
fn water_reflectance(cosine: f32) -> f32 {
    let c = 1.0 - clamp(cosine, 0.0, 1.0);
    let c2 = c * c;
    return 0.02 + 0.98 * c2 * c2 * c;
}
struct WaterFragment {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}
@fragment fn water_fs(input: SkyVarying) -> WaterFragment {
    return water_surface(input.ndc);
}
// The surface as geometry (water.rs, `Passes::raster`): a square of the plane
// about the camera, reaching far past any far plane, drawn with unclipped
// depth so that beyond the far plane it lies at the far depth, as
// `water_surface`'s clamp puts it. The rasterizer then gives each sample the
// plane's depth, and early depth tests leave hidden water unshaded: the
// full-screen form writes its depth from the shader and shades every pixel.
const WATER_REACH: f32 = 1.0e6;
struct WaterPlane {
    @builtin(position) clip: vec4<f32>,
    @location(0) at: vec4<f32>,
}
@vertex fn water_plane_vs(@builtin(vertex_index) i: u32) -> WaterPlane {
    // Two triangles, corners 0 1 2 and 0 2 3 of the square.
    let c = select(i, select(0u, i - 2u, i > 3u), i >= 3u);
    let corner = vec2(select(-1.0, 1.0, c == 1u || c == 2u), select(-1.0, 1.0, c >= 2u));
    let centre = frame.camera_alpha.xyz;
    let world = vec3(centre.x + corner.x * WATER_REACH, frame.water.x, centre.z + corner.y * WATER_REACH);
    let clip = frame.view_proj * vec4(world, 1.0);
    return WaterPlane(clip, clip);
}
@fragment fn water_plane_fs(input: WaterPlane) -> @location(0) vec4<f32> {
    return water_surface(input.at.xy / input.at.w).color;
}
fn water_surface(ndc: vec2<f32>) -> WaterFragment {
    let input = SkyVarying(vec4(0.0), ndc);
    // From the near plane, so an orthographic camera's parallel rays start apart.
    let near = frame.inverse_view_proj * vec4(input.ndc, 0.0, 1.0);
    let origin = near.xyz / near.w;
    let d = view_ray(input.ndc);
    let height = frame.water.x;
    let t = (height - origin.y) / d.y;
    let hit = origin + d * t;
    let footprint = length(fwidth(hit.xz));
    if !(t > 0.0 && t < 1.0e9) { discard; }
    let below = origin.y < height;
    let n = wave_normal(hit.xz, footprint);
    var color: vec3<f32>;
    var alpha = 1.0;
    if below {
        // Inside Snell's window the sky, outside it the depths, totally reflected.
        let refracted = refract(d, -n, WATER_IOR);
        var transmitted = 0.0;
        if dot(refracted, refracted) > 0.0 {
            transmitted = 1.0 - water_reflectance(dot(refracted, n));
        }
        let mirrored = reflect(d, -n);
        color = transmitted * sky_radiance(refracted) + (1.0 - transmitted) * environment(mirrored.y);
        color = height_fog(color, hit);
    } else {
        // Premultiplied: the sky's reflection, and the water's colour where the
        // floor does not show through.
        let reflected = water_reflectance(dot(-d, n));
        let body = (1.0 - reflected) * (1.0 - clamp(frame.water.z, 0.0, 1.0));
        color = reflected * sky_radiance(reflect(d, n)) + body * frame.water_color.xyz;
        alpha = reflected + body;
    }
    // Beyond the far plane the surface stays in front of the sky.
    let clip = frame.view_proj * vec4(hit, 1.0);
    let z = clamp(clip.z / clip.w, 0.0, 0.9999999);
    let split = frame.water.w;
    let depth = split + z * (1.0 - split);
    if DIRECT {
        // A direct frame (the immersive profile): tonemap the unpremultiplied
        // colour and premultiply again, so the blend mixes display values.
        let shown = tonemap(finite_hdr(color) / max(alpha, 1.0e-4)).rgb * alpha;
        return WaterFragment(vec4(shown, alpha), depth);
    }
    return WaterFragment(vec4(finite_hdr(color), alpha), depth);
}
