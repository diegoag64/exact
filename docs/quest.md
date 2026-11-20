# A game on Meta Quest (OpenXR on Android)

A game can be built as a native Meta Quest 3 / 3S / Pro app: the Android APK
starts immersive in the headset and the world draws in stereo from the
headset's pose, through OpenXR and Vulkan. Like the WebXR spike
([webxr.md](webxr.md)) this is viewing only: no controllers yet, and the HUD
and menus (Contract) are not drawn in the headset. They keep running, unseen.

## Opting in

- `app.json`: `"host": { "android": { "xr": "quest" } }`. With it the game's
  `bun exact.mjs android` builds a Quest APK. Leave it out for a phone APK.
  The key is a build switch, not a variant (a game takes no variants): the
  Quest APK requires head tracking, so phones refuse to install it.
- Place the world with `Game::xr_stage` (as for WebXR, `game/engine/src/xr.rs`):
  `XrStage::tabletop` stands a board on a table in front of the player. The
  reference space is OpenXR's `LOCAL_FLOOR` (the floor where the app started),
  or `LOCAL` with the floor put 1.6 m under the head where a runtime has no
  `LOCAL_FLOOR`.
- `Game::xr_args(&mut args)` adjusts the canvas's arguments while the app
  runs in a headset (each bind, before the world sees it). Hexguard turns its
  menu's attract diorama into level 1, playing, since the menus are not drawn
  there. Without it the arguments are the app's.

## What the build does

- `game/app/shells.mjs` turns on exact-gpu's `openxr` feature for the game's
  GPU module on Android.
- `scripts/agent-android.mjs` (`stageOpenxrLoader`) packages the Khronos
  OpenXR loader (`libopenxr_loader.so`, Maven Central's
  `org.khronos.openxr:openxr_loader_for_android`, version and sha256 pinned in
  `OPENXR_LOADER`, cached under `~/.cache/exact/openxr-loader`) beside the
  module.
- `host/android/app/build.gradle.kts` adds to the manifest: the launcher
  activity's `com.oculus.intent.category.VR` and
  `org.khronos.openxr.intent.category.IMMERSIVE_HMD` categories,
  `com.oculus.supportedDevices` (`quest3|quest3s|questpro`), the required
  `android.hardware.vr.headtracking` feature, the loader's
  `org.khronos.openxr.permission.OPENXR` / `OPENXR_SYSTEM` permissions and its
  runtime-broker `<queries>`, and a landscape activity.

## What runs

`gpu/src/openxr.rs`, in the GPU module (only with the feature, only on
Android):

1. `gpu_android_init` keeps the Java VM and the application `Context`.
   `ExactActivity` registers itself as `dev.exact.canvas.Immersive.activity`
   before the host boots; the module reads it over JNI and gives it to the
   loader and `xrCreateInstance` (`XR_KHR_android_create_instance`).
2. Making the device (`gpu_prepare`, at boot): OpenXR instance and system,
   then `XR_KHR_vulkan_enable2`: the runtime makes the `VkInstance` (with the
   extensions wgpu-hal wants) and the `VkDevice` (wgpu-hal's extensions and
   features for the granted wgpu features, Vulkan 1.1, queue family 0 /
   index 0 as wgpu-hal opens). Where the GPU has them, the device also gets
   `MULTIVIEW` with `max_multiview_view_count = 2` (wgpu's default limit is
   0, which refuses a two-view pass) and `MULTISAMPLE_ARRAY`, for single-pass
   stereo at 4x; the log line `device … multiview … multisample array …`
   says what was granted. wgpu wraps
   both (`Instance::from_raw`, `expose_adapter`, `device_from_raw`,
   `create_adapter_from_hal`, `create_device_from_hal`), and the session is
   made on that device. With no loader or runtime (a phone) the device is made
   as always and the app draws to its window; the log says so.
3. Each frame the host renders (`gpu_render`), the first canvas it asks for
   draws into the headset instead of its window: events are polled
   (`READY` begins the session and asks for 72 Hz through
   `XR_FB_display_refresh_rate`; `STOPPING` ends it; losing focus tells the
   canvas `Interrupted`, regaining it `Resumed`), then `xrWaitFrame` (the
   host's thread is paced by the headset), `xrBeginFrame`, `xrLocateViews`,
   the swapchain image is acquired and waited for, the canvas draws a stereo
   frame (`Module::render_xr_stereo`, below), the image goes back in
   `COLOR_ATTACHMENT_OPTIMAL`, and `xrEndFrame` submits one projection layer.
   The world ticks once per headset frame on its predicted display time.
4. The swapchain: one colour swapchain at the runtime's recommended size with
   two array layers (left, right), sRGB (the canvas's own format when the
   runtime offers it, else `R8G8B8A8_SRGB`, else `B8G8R8A8_SRGB`), each image
   wrapped as a wgpu `D2` texture of two layers. No depth swapchain yet.
5. Foveation: `XR_FB_foveation` (HIGH, dynamic) is applied to the colour
   swapchain where offered. On Vulkan the runtime foveates through a fragment
   density map the app's render passes must use, which wgpu's do not, so it
   has no effect yet.
6. Each eye's matrices: the view is the inverse of the eye's pose
   (`exact_gpu::pose_view`), the projection the asymmetric frustum of its
   field of view with 0–1 depth, near 0.05 m and far 200 m
   (`exact_gpu::fov_projection`; not reversed). `XrStage::eye` then places the
   world.

### The stereo entry

```rust
// gpu/src/xr.rs
pub struct XrStereo {
    pub target: wgpu::Texture,          // D2, 2 array layers (left, right)
    pub size: (u32, u32),               // each layer's pixels
    pub views: [[f32; 16]; 2],          // reference space → eye, column-major
    pub projections: [[f32; 16]; 2],    // eye → clip, 0–1 depth
}
// gpu/src/lib.rs, trait Surface
fn render_xr_stereo(&mut self, frame: &Frame, device: &wgpu::Device, queue: &wgpu::Queue,
                    stereo: &XrStereo, format: wgpu::TextureFormat) -> Option<bool>;
```

The default draws each layer through `Surface::render_xr` (the WebXR per-eye
path: one encode and submit per eye) using `XrStereo::eye_views`. The game
renderer (`game/render/src/surface/xr.rs`, `encode_stereo`) overrides it and
draws `XrStereo::array_view` in one multiview pass (`game/render/src/stereo.rs`):
the frame's shared work (skinning, cull, the sun's shadow, light lists, levels
of detail, translucent order) once on a camera whose frustum holds both eyes',
the forward and translucent passes with `@builtin(view_index)`. Without
`MULTIVIEW` (or `MULTISAMPLE_ARRAY` at 4x) it falls back to per eye.
`Surface::xr_stereo_mode` says which ran (`multiview-direct-4x`, …).

Every immersive frame takes the immersive quality profile
(`game/render/src/profile.rs`): direct frames (shaded and tonemapped straight
into the eye's sRGB layer, no HDR image or tone pass), 4x MSAA resolved at
the end of the pass, one 1024² sun shadow layer fit to `XrStage::extent` and
cached for what stands still, no bloom, ambient occlusion, caustics or water
shafts, at most 8 local lights, and models' coarse levels at 3× the eye's
distance. Stereo frames also skip render hooks and soft particles.

The MSAA attachments: on the per-eye path they are single-layer and
`TRANSIENT_ATTACHMENT` (lazily allocated memory on Adreno, never in DRAM). A
multiview frame's are two-layer, and wgpu 30 refuses transient textures of
more than one layer (`InvalidTransientTextureLayerCount`), so they take real
memory: at 1680×1760 a layer, 4x colour (RGBA8) and depth (Depth32Float)
are 2 × 90 MiB = 180 MiB. Their contents are still discarded at the end of
the pass, so on a tiler they cost memory, not bandwidth.

## Build, install, run

Toolchain (once): the Android SDK with platform 37.2, build-tools 37.0.0 and
an NDK, a JDK 17+ (`JAVA_HOME`), `rustup target add aarch64-linux-android`
and the Android Hermes bundle; `bun scripts/exact.mjs setup --check
--platform android` names what is missing.

Headset: developer mode on (the Meta Horizon app on a phone), connected over
USB, the computer allowed (`adb devices` lists it).

    bun exact.mjs android            # build, install on the headset (--release: optimized)
    adb shell am start -n <package>/dev.exact.app.MainActivity

The package is `host.android.id`, else `app.id` (`com.exact.hexguard`). Put
the headset on (the proximity sensor pauses a session nobody wears; `adb
shell am broadcast -a com.oculus.vrpowermanager.prox_close` keeps it awake
for a while without wearing it). It is also in the headset's library under
Unknown Sources.

## Debugging

- `adb logcat -s exact-xr` (the module's lines): the runtime, the device, the
  swapchain, every session state, `first frame submitted`, and once a second
  `perf mode=… fps=… cpu_ms=… encode_ms=… worst_ms=… wait_ms=… skipped=…`
  (`mode` how the canvas drew its stereo frames, `multiview-direct-4x` or
  `per-eye-…`, and `stereo frames draw …` when it changes; `cpu_ms` from
  `xrBeginFrame` to `xrEndFrame`, `encode_ms` the canvas's stereo draw,
  `wait_ms` the time in `xrWaitFrame`).
- `adb logcat -s VrApi` adds Meta's own per-second line (`FPS=…`, `Prd`,
  GPU and CPU levels, `GPU%`, stale frames).
- `adb exec-out screencap -p > frame.png` captures the compositor's view (one
  eye's distorted image, both eyes side by side on some builds).
