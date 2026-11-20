# Immersive web (WebXR on WebGPU): the stereo spike

A game can be looked at in a headset from the web build: the world draws once
per eye into a WebXR session's projection layer, from the headset's pose. This
is phase 1 of the work: viewing only. There are no controllers and no menus or
HUD in the headset; the page's own UI keeps working outside the session.

## Opting in

- `app.json`: `"game": { "xr": true }`. The web build then marks the page
  (`<html data-exact-xr="immersive-vr">`) and `gpu-glue.js` loads
  `host/web/xr-glue.js`. Without the key nothing changes: no adapter option,
  no button, no extra script.
- Place the world with `Game::xr_stage(world, args) -> XrStage` (in
  `game/engine/src/xr.rs`). An `XrStage` says which world point (`center`)
  lands where (`position`, metres in the session's `local-floor` space, +Y up,
  the player facing −Z), how big (`scale`, metres per world unit) and turned
  how far (`yaw`). `XrStage::tabletop(center, size, metres, height, distance)`
  puts a board `size` units across `metres` wide at table height in front of
  the player. It is read on every immersive frame and never saved or hashed.
  The default is one world unit per metre with the origin on the floor.

In a session the world's own `Camera` is ignored: each eye's view is the
headset's, placed by the stage. The view stays rigid in world units (the scale
moves into the projection's near and far planes), so shadows, fog and LOD read
world distances.

## What happens

1. Before the module asks for its adapter, `xr-glue.js` adds `xrCompatible:
   true` to `navigator.gpu.requestAdapter` (wgpu has no such option);
   `XRGPUBinding` needs it.
2. Where `navigator.xr.isSessionSupported("immersive-vr")` and `XRGPUBinding`
   exist, an "Enter VR" button appears at the bottom right.
3. The button requests `immersive-vr` with `requiredFeatures: ["webgpu"]`
   (`local-floor` optional; without it a floor is put 1.6 m under the head),
   makes an `XRGPUBinding` on the module's device (`gpu_xr_device`) and a
   projection layer, in the canvas's own format when the browser takes it
   (`gpu_xr_format`), else its preferred one.
4. While the session runs, the page's frame loop rests and each XR frame calls
   `gpu_xr_render(id, time, format, textures, eyes)`: the session's
   `GPUTexture`s are wrapped with `Device::create_texture_from_webgpu_handle`
   (cached by object), the world ticks once on the frame's time and
   `Surface::render_xr` draws each eye into its array layer
   (`gpu/src/xr.rs`, `game/render/src/surface/xr.rs`). An eye that owns only
   part of its texture (side by side) is drawn into its own and copied in,
   which needs the layer's `COPY_DST` usage (asked for, dropped if refused).
5. Each eye is encoded and submitted on its own: the renderer writes its
   uniforms and culling lists through the queue, and a queue write lands
   before every command of the next submission.
6. When the session ends the page's frames draw again.

Left out in a session: render hooks (a game's post passes assume one flat
camera), placed children (`Placed`, DOM over the canvas), the perf timing,
and the page's Contract UI.

If a session must draw in a format other than the canvas's, the renderer is
made again for it and the world's textures are delivered again
(`Surface::immersive`); the page's renderer is remade the same way after the
session. Expect a moment of untextured models then.

## Trying it

Desktop: no macOS browser has an XR runtime, and the Immersive Web Emulator
supplies WebGL sessions only. The path was proven headless with a stand-in
`navigator.xr` and `XRGPUBinding` whose layer is a real two-layer texture
array, whose layers were then drawn side by side and captured.

Meta Quest 3 (Quest Browser with WebGPU-in-WebXR, behind a flag):

1. On the headset: `chrome://flags`, enable **WebXR experimental features**
   (and WebGPU, if `navigator.gpu` is missing), relaunch the browser.
2. Developer mode on, the headset on USB, `adb devices` lists it.
3. On the computer: `bun exact.mjs web --port 8765` in the game, then
   `adb reverse tcp:8765 tcp:8765`.
4. On the headset: open `http://localhost:8765/` (localhost is a secure
   context, which WebXR requires; a LAN address over plain http is not).
   Press **Enter VR**.

The dev server answers `Host: localhost` as it does `127.0.0.1`; nothing else
is needed. Errors from a session go to the page's console
(`chrome://inspect` on the computer, with the headset on USB) and to
`exact.gpu.immersive` (`error`, `format`, `frames`, `glDepth`).

`exact.gpu.immersive.glDepth` is true when the browser handed projections in
WebGL's −1..1 depth (the binding says 0..1); `xr-glue.js` remaps them.
