# A game on Meta Quest (OpenXR on Android)

A game can be built as a native Meta Quest 3 / 3S / Pro app: the Android APK
starts immersive in the headset and the world draws in stereo from the
headset's pose, through OpenXR and Vulkan. The controllers reach the world as
input ("Controllers" below). The HUD and menus (Contract) are not drawn in
the headset: they keep running, unseen, so a game draws what it needs there
in its world.

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
  menu's attract diorama into the level, played with the hands, since the
  menus are not drawn there. Without it the arguments are the app's.

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
   made on that device. The wgpu instance is made with the module's instance
   flags and no backend of its own, then given the runtime's adapter: not
   with `wgpu::Instance::from_hal`, whose instance takes wgpu-core's default
   flags (see "What the headset drew white" below). With no loader or runtime (a phone) the device is made
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

## Controllers

The headset's controllers are input like any other device: the host reads
them, the canvas gets an event, the world's next tick reads it from `Input`.
Nothing in a game calls OpenXR, so a run with hands replays and saves as any
run does.

**The host** (`gpu/src/openxr_input.rs`). With the session it makes one
action set and attaches it: per hand an aim pose and a grip pose, the trigger
and the grip button (each as its travel and as pressed), the thumbstick and
its click, the two face buttons and the menu button. Bindings are suggested
for `/interaction_profiles/oculus/touch_controller` (Quest 2, 3, 3S and Pro
controllers answer to it) and for `/interaction_profiles/khr/simple_controller`
(select as the trigger, menu, the two poses) as the fallback. Each frame,
after `xrLocateViews`, it syncs the actions, locates both hands' poses and the
head (the `VIEW` space) in the frame's reference space at the predicted
display time, and delivers one `InputEvent::Xr` to the canvas before the
world advances (`Module::input`), stamped with the last frame's time so the
next tick takes it. A frame equal to the last sends nothing. Without focus
(the system menu) the runtime reports every action inactive, which arrives as
both hands inactive with nothing held. The log says which profile each hand
took and when a hand comes and goes (`adb logcat -s exact-xr | grep
controllers`).

**The event** (`gpu/src/xr_input.rs`, `exact_gpu::XrInput`): the head's pose
and whether it is tracked, and for each hand `active` (a controller is
there), `tracked`, `aim` and `grip` poses (position in metres and a unit
quaternion, in the session's reference space; a pose points along its −Z),
`trigger` and `squeeze` in 0–1, `stick` (+x right, +y forward) and `buttons`,
bits of `xr_input::button` (`TRIGGER`, `SQUEEZE`, `PRIMARY` A or X,
`SECONDARY` B or Y, `MENU`, `STICK`); and `recenters`, the runtime's
`XrEventDataReferenceSpaceChangePending` events counted since the session
began.

**The world** (`game/engine/src/input/xr.rs`):

```rust
fn tick(world: &mut World, input: &Input, args: &Args) {
    let right = input.xr().hand(XrHand::Right);          // XrController
    if right.active && right.tracked {
        let stage = Self::xr_stage(world, args);
        let (origin, direction) = right.aim.ray();       // reference space
        let (o, d) = stage.ray_to_world(origin, direction);
        // ... cast o + d·t into the world; place a hand at
        // stage.to_world(right.grip.position)
    }
    if input.xr_pressed(XrHand::Right, XrButton::Trigger) { /* this tick only */ }
    let turn = right.stick.x;                            // and right.trigger, right.squeeze
}
```

`Input::xr()` is the latest state (`XrInput`: `head`, `head_tracked`,
`hands`, and `recenters`, which grows each time the runtime moves the
reference space under the person: the Meta button held. A game that placed
its world about the person's head places it again when the count changes); `xr_pressed` and `xr_released` are the edges since the last tick;
`XrController::down` is held. The state is also the world resource `XrInput`
(saved and hashed, absent in a world no headset reported to), and
`Sim::xr(input)` queues one in a test. Within a tick a later frame that
pressed and released nothing replaces an earlier one, as a pad's sticks do.

`XrStage` takes the reference space into the world and back (`to_world`,
`to_reference`, `ray_to_world`, `rotation_to_world`), and moves under the
hands: `dragged(from, to)` keeps the world point that was in one hand in it,
`pinched([a0, b0], [a1, b1], bounds)` turns the world about +Y, sizes it and
moves it so both held points stay in their hands. A game keeps the placement
it arrives at in its own world and answers `Game::xr_stage` from it
(Hexguard: `logic/src/xr.rs`).

Drawing the hands is the game's: an entity whose transform is the stage's
inverse (`position: stage.to_world(Vec3::ZERO)`, `rotation:
stage.rotation_to_world(Quat::IDENTITY)`, `scale: 1 / stage.scale`) is the
reference space in the world, and a child placed at a grip pose is in the
hand. Poses reach the picture a tick late and interpolated, as every
transform does; late latching (drawing a hand at the frame's own pose) is not
done.

### Driving the controllers without hands

A debug stand-in, in any build, for hands nobody has (a headset on a desk, a
scripted check with the eye capture below): the module looks about six times
a second for a file named `xr-input` in the app's external files directory,
takes its lines as a script and removes it. A hand the script posed or
pressed stays the script's, held where it was left, until the script
releases it; hands it never named stay the tracked ones. Each new file's
steps queue after those still pending.

    D=/sdcard/Android/data/<package>/files
    adb shell "echo 'right point 0.25 1.25 -0.25  0 0.85 -0.9' > $D/xr-input"
    adb shell "echo 'right click trigger' > $D/xr-input"
    adb logcat -s exact-xr | grep xr-input      # `xr-input: 1 lines taken`, or what it refused

One step a line (or between `;`); `#` starts a comment. Positions are metres
in the reference space (the floor where the app started, +Y up, −Z ahead).

| step | effect |
|---|---|
| `<hand> aim <ox oy oz> <dx dy dz>` | the hand (`left`, `right`) at the origin, pointing along the direction; its grip there too |
| `<hand> point <ox oy oz> <tx ty tz>` | the same, pointing at a target point |
| `<hand> grip <px py pz> <fx fy fz> [<ux uy uz>]` | the grip pose alone: its −Z along `f`, its +Y toward `u` |
| `<hand> <button> down` / `up` | `trigger`, `squeeze`, `a` `x` `primary`, `b` `y` `secondary`, `menu`, `stick` |
| `<hand> click <button>` | down, 8 frames, up, 8 frames |
| `<hand> stick <x> <y>` | the thumbstick, each in −1..1 |
| `head <px py pz> <fx fy fz>` | the head's pose as the canvas hears it (the eyes still draw from the tracked head, or from a capture's fixed pose) |
| `<hand> release`, `head release`, `release` | back to tracking |
| `wait <frames>` | let that many frames pass |
| `message <text>` | post text to the canvas, as Contract's `postMessage` |
| `recenter` | count a recenter (`XrInput::recenters`), as the runtime does when the Meta button is held; the poses stay where they are, so script a `head` first to stand somewhere else |

A wrong line refuses the whole file, by line. `gpu/src/xr_input.rs`
(`Injector`) holds the grammar and its tests; nothing runs until the file
appears.

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

## Seeing what the headset shows (eye capture)

A debug capture of what the app itself draws, in any build (release
included), without Meta's capture service (which uploads to the account):
the module looks about twice a second for a file named `capture` in the app's
external files directory, and when it appears the next drawn stereo frame's
two swapchain layers are read back and written beside it as PNGs (8-bit RGB,
uncompressed, about 9 MB an eye at 1680×1760). Nothing happens, and nothing
is read back, until the file appears. The image is the layer the app hands
the compositor, before lens distortion and reprojection: what each eye sees,
undistorted.

    D=/sdcard/Android/data/<package>/files      # e.g. com.exact.hexguard
    adb shell "echo head > $D/capture"          # the next frame from the tracked head
    adb shell "echo stage > $D/capture"         # from a fixed pose looking at the stage
    adb shell "echo 'stage 0.8 30 -20' > $D/capture"   # distance m, pitch ° down, yaw °
    adb shell "echo 'look 0 1.4 0  -0.3 1.1 -0.6' > $D/capture"   # an eye point, looking at a target
    adb logcat -s exact-xr | grep capture       # `capture <n> (…): <left> <right>`
    adb pull $D/eye-1-left.png; adb pull $D/eye-1-right.png

- `head` (or an empty file) waits for a tracked head: a headset nobody wears
  shows nothing, so it waits until someone does.
- `stage [distance [pitch [yaw]]]` replaces the head for that one frame with
  a fixed pose: looking at the stage's centre (`XrStage::position`, which the
  canvas reports through `Surface::xr_focus`) from `distance` metres (0.6),
  `pitch` degrees down (40), turned `yaw` degrees about +Y (0: from the
  player's side), eyes 64 mm apart, each eye's field of view as the runtime
  reports it. It draws whether or not the head is tracked, so it works with
  nobody wearing the headset, as long as the session runs (the proximity
  sensor pauses it: `adb shell am broadcast -a
  com.oculus.vrpowermanager.prox_close` keeps it running for a while). The
  compositor shows that one frame too, so someone wearing the headset sees a
  one-frame jump.
- `look <ex ey ez> <tx ty tz>` is the same fixed head standing at a point of
  the reference space and looking at another: a close look at what is not
  the stage's centre (a panel over a hand, a controller, one tile).
- Captures are numbered from 1 in each process (`eye-<n>-left.png`,
  `eye-<n>-right.png`) and overwrite an earlier process's of the same number.
  The module removes the trigger after reading it; where it cannot, the same
  file is not taken twice (write it again to take another).
- Three seconds into a session (the 216th stereo frame) the canvas also logs
  its world once, under the `exact` tag: `xr state: args [...] tick=…
  entities=… assets loaded=… pending=… failed=… prepared=… models=…
  resident_models=…; last frame mode=… draws=… instances=… triangles=…;
  stage …; journal: …` (the last journal lines), so a state problem shows in
  the log without a picture.

`gpu/src/xr_capture.rs` holds what needs no headset (the trigger's grammar,
the fixed pose, the PNG writer, with their tests); `gpu/src/openxr.rs`
(`Capture`) polls, reads back and writes.

### Debug switches

A file `xr-debug` in the same directory holds whitespace-separated switches
the module reads once, when it makes the device (relaunch the app after
changing it; `adb shell rm $D/xr-debug` turns them all off). The log names
them (`debug switches: …`) and the `device …` line says what was granted.
They exist to bisect a picture that is wrong on the headset only:

| switch | effect |
|---|---|
| `no-multiview` | no `MULTIVIEW`: stereo frames draw each eye on its own |
| `no-first-instance` | no `INDIRECT_FIRST_INSTANCE`: culled draws select their list by a dynamic offset |
| `no-indirect` | the game renderer draws every list directly (`Cull::direct`): no GPU cull, no indirect draws, finest levels only |
| `no-merge` | each culled group is its own `draw_indexed_indirect` (no multi-draw runs) |
| `cull-pass-all` | the GPU cull keeps everything in every view (its frustum test passes all) |
| `vk13` | the runtime makes the Vulkan instance at 1.3 rather than 1.1 |

`exact_gpu::debug_switch(name)` reads them anywhere in the module.
With the state line the canvas also logs `xr cull: …`, the GPU cull's
indirect arguments for this frame read back (instances per view, the first
groups' `[index count, instances, first index, base vertex, first
instance]`) and whether each compacted camera slot is one of its group's.

## What the headset drew white (November 2026)

Hexguard on a Quest 3 showed only the sea, the sky and the white foam hexes
at the waterline: no tiles, keep, portal, props or towers, though the world
was level 1 with every asset loaded and the renderer counted 650 instances
a frame. What was missing was everything the game renderer draws through
its GPU cull (opaque models and primitives); what drew was what it draws
directly (the sky, the water's surface, translucent models such as the
foam).

The captures and switches above narrowed it: `no-indirect` drew the whole
island; `no-multiview`, `no-first-instance`, `no-merge` (and both
together), `cull-pass-all` and `vk13` did not; the cull's own output read
back correct (468 camera instances, sane `[index count, instances, first
index, base vertex, first instance]`, every compacted slot its group's);
the compacted lists drawn by direct draws showed the island; and indirect
draws from arguments the host wrote drew nothing either. So every indirect
draw was dropped after it left the renderer.

The cause: `wgpu::Instance::from_hal` (wgpu-core 30's
`Instance::from_hal_instance`) gives the instance `InstanceFlags::default()`,
which includes `VALIDATION_INDIRECT_CALL`, and `create_device_from_hal`
takes its flags from the instance. Every other host makes its instance with
`device::instance_flags()`, which leaves that validation out (the game
renderer writes its own arguments, valid by construction). On the headset
wgpu's validation pass ran before every render pass and rewrote each culled
draw as an empty one. The fix makes the headset's wgpu instance with the
module's flags (`gpu/src/openxr.rs`, `make`).

With the island drawn, Hexguard's level 1 (the headset demo, seven towers,
the first wave) is GPU-bound on a Quest 3 under the immersive profile's
multiview direct 4x frames: VrApi `App=` 13–17 ms at GPU level 4 and
`GPU%=0.97`, so 48–62 Hz depending on the view (`no-indirect`, which draws
every finest level unculled, measured 62 Hz with the headset where it then lay). The earlier
72 Hz at 4–6 ms was a frame that drew no opaque geometry; holding 72 Hz is
work still to do.
