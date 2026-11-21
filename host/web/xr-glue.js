// Immersive frames (WebXR on WebGPU): imported by gpu-glue.js only when the
// page says the game opted in (app.json `game.xr` → `<html data-exact-xr>`).
//
// `shimAdapter` runs before the module asks for its adapter: wgpu's
// requestAdapter has no `xrCompatible`, which XRGPUBinding requires of the
// device's adapter. `offer` shows an "Enter VR" button when the browser can
// run an `immersive-vr` session with the `webgpu` feature (Quest Browser,
// behind its WebXR experimental flag; Chromium with WebXRGPUBinding). In a
// session the world's canvas stops drawing on the page's frames: each XR
// frame draws it once per eye into the projection layer's textures
// (`gpu_xr_render`, gpu/src/xr.rs), at the session's frame time, so the
// world keeps ticking on the frame clock it always uses.
//
// Each eye's floats: the texture's width, height, layers and usage, the eye's
// array layer and viewport, then its view and projection matrices.
const EYE_FLOATS = 9 + 32;

export function shimAdapter() {
  const proto = globalThis.GPU?.prototype;
  if (!proto?.requestAdapter || proto.requestAdapter.exactXr) return;
  const request = proto.requestAdapter;
  const shimmed = function (options) { return request.call(this, { ...(options ?? {}), xrCompatible: true }); };
  shimmed.exactXr = true;
  proto.requestAdapter = shimmed;
}

/** Whether this browser can run a WebGPU immersive session. */
export async function supported() {
  if (!navigator.xr || typeof globalThis.XRGPUBinding !== "function") return false;
  try { return await navigator.xr.isSessionSupported("immersive-vr"); } catch { return false; }
}

const BUTTON_STYLE = "position:fixed;right:16px;bottom:16px;z-index:2147483647;padding:10px 18px;border-radius:999px;" +
  "border:1px solid rgba(255,255,255,.35);background:rgba(20,24,32,.82);color:#fff;font:600 15px/1.2 system-ui,sans-serif;" +
  "cursor:pointer;backdrop-filter:blur(6px)";

/**
 * Offer an immersive session for the page's world. `host` is gpu-glue's side:
 * `module()` the GPU module, `target()` the canvas entry to draw (with its
 * `id`), `clock(now)` the surfaces' clock, `started()`/`ended()` around the
 * session, `after(entry)` the page's work after a frame (its messages), and
 * `lost()` for a lost device.
 */
export async function offer(host) {
  if (!await supported()) return null;
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = "Enter VR";
  button.setAttribute("data-exact-xr-button", "");
  button.style.cssText = BUTTON_STYLE;
  const state = { session: null, active: false, error: null, frames: 0 };
  button.addEventListener("click", () => {
    if (state.session) { state.session.end().catch(() => {}); return; }
    start(host, state, button).catch(error => {
      state.error = String(error?.message ?? error);
      console.error("exact xr:", error);
      button.textContent = "VR unavailable";
      setTimeout(() => { if (!state.session) button.textContent = "Enter VR"; }, 2500);
    });
  });
  document.body.append(button);
  return state;
}

// A WebGPU session's projections are 0–1 depth (the WebXR-WebGPU binding);
// one that maps the near plane to −1 is WebGL's, remapped here: z' = (z + w) / 2.
function clipDepth(matrix, near, state) {
  const p = matrix, ndcNear = p[11] !== 0 ? (p[14] - near * p[10]) / near : 0;
  if (!(ndcNear < -0.5)) return p;
  if (!state.glDepth) { state.glDepth = true; console.warn("exact xr: the session's projections are -1..1 depth; remapping to 0..1"); }
  const q = Float32Array.from(p);
  for (let c = 0; c < 4; c++) q[c * 4 + 2] = 0.5 * p[c * 4 + 2] + 0.5 * p[c * 4 + 3];
  return q;
}

async function start(host, state, button) {
  const module = host.module();
  const device = module?.gpu_xr_device?.();
  if (!device) throw new Error("the GPU module has no device for an immersive session");
  const session = await navigator.xr.requestSession("immersive-vr", {
    requiredFeatures: ["webgpu"],
    optionalFeatures: ["local-floor"],
  });
  state.session = session;
  try {
    let space;
    try { space = await session.requestReferenceSpace("local-floor"); }
    catch {
      // No floor: put one 1.6 m under where the head started.
      space = (await session.requestReferenceSpace("local")).getOffsetReferenceSpace(new XRRigidTransform({ x: 0, y: -1.6, z: 0 }));
    }
    const binding = new XRGPUBinding(session, device);
    // The canvas's own format first: the world's renderer then stays as it is
    // (another format remakes it, and its textures are delivered again).
    // COPY_DST lets an eye that owns half a texture (side by side) be copied in.
    const own = module.gpu_xr_format?.(host.target()?.id ?? 0), preferred = binding.getPreferredColorFormat();
    const usage = GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_DST;
    let layer, format;
    for (const [colorFormat, textureUsage] of [[own, usage], [preferred, usage], [preferred, undefined]]) {
      if (!colorFormat) continue;
      try {
        layer = binding.createProjectionLayer(textureUsage ? { colorFormat, textureUsage } : { colorFormat });
        format = colorFormat;
        break;
      } catch (error) { state.error = String(error?.message ?? error); }
    }
    if (!layer) throw new Error(`no projection layer: ${state.error}`);
    state.format = format; state.error = null;
    session.updateRenderState({ layers: [layer], depthNear: 0.05, depthFar: 200 });
    state.active = true;
    button.textContent = "Exit VR";
    host.started(session);
    const frame = (time, xrFrame) => {
      if (state.session !== session) return;
      session.requestAnimationFrame(frame);
      const pose = xrFrame.getViewerPose(space);
      const entry = host.target();
      const gpu = host.module();
      if (!pose || !entry?.id || !gpu) return;
      const views = pose.views, textures = [], eyes = new Float32Array(views.length * EYE_FLOATS);
      for (let i = 0; i < views.length; i++) {
        const view = views[i], sub = binding.getViewSubImage(layer, view), texture = sub.colorTexture;
        const descriptor = sub.getViewDescriptor?.() ?? {};
        const vp = sub.viewport ?? { x: 0, y: 0, width: texture.width, height: texture.height };
        textures.push(texture);
        eyes.set([texture.width, texture.height, texture.depthOrArrayLayers, texture.usage,
          descriptor.baseArrayLayer ?? 0, vp.x, vp.y, vp.width, vp.height], i * EYE_FLOATS);
        eyes.set(view.transform.inverse.matrix, i * EYE_FLOATS + 9);
        eyes.set(clipDepth(view.projectionMatrix, session.renderState.depthNear ?? 0.1, state), i * EYE_FLOATS + 25);
      }
      const r = gpu.gpu_xr_render(entry.id, host.clock(time), format, textures, eyes);
      state.frames++; state.view = entry.view; state.last = r;
      if (r === 2) {
        const message = gpu.gpu_error();
        if (message !== state.error) console.error("exact xr:", message);
        state.error = message;
      } else if (r === 3) { session.end().catch(() => {}); host.lost(); return; }
      host.after(entry);
    };
    session.addEventListener("end", () => {
      state.session = null; state.active = false;
      button.textContent = "Enter VR";
      host.module()?.gpu_xr_end?.();
      host.ended();
    }, { once: true });
    session.requestAnimationFrame(frame);
  } catch (error) {
    state.session = null; state.active = false;
    session.end().catch(() => {});
    throw error;
  }
}
