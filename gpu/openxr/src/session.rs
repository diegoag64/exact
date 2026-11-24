//! The headset's session and its frames (`docs/quest.md`). One colour
//! swapchain of two array layers (left, right) serves both eyes; each frame
//! the canvas the host renders first draws a stereo frame into it (the GPU
//! module's `gpu_render_stereo`, which falls back to one pass per eye)
//! instead of into its window, and the frame is handed to the compositor as
//! one projection layer.
//!
//! The frame loop rides the host's own: the host calls [`render`] (`xr_render`)
//! where it would render the canvas, and that call waits for the headset's
//! next frame (`xrWaitFrame`), so the host is paced by the headset. The world
//! ticks once per headset frame, on its predicted display time.

use crate::capture::{Capture, Perf};
use crate::java::{self, log};
use crate::module::{Module, StereoTarget, USAGE_ALL};
use crate::pose::{fov_projection, pose_view};
use openxr as xr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The near and far planes of each eye, metres (the stage rescales them to
/// world units).
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
/// The display rate asked for (`XR_FB_display_refresh_rate`).
const REFRESH_HZ: f32 = 72.0;

// ---------------------------------------------------------------- the headset

/// The session and what it draws into, made with the device.
pub(crate) struct Headset {
    instance: xr::Instance,
    system: xr::SystemId,
    session: xr::Session<xr::Vulkan>,
    waiter: xr::FrameWaiter,
    stream: xr::FrameStream<xr::Vulkan>,
    space: Option<xr::Space>,
    swapchain: Option<Swapchain>,
    /// `xrBeginSession` was called and `xrEndSession` not since.
    running: bool,
    focused: bool,
    exiting: bool,
    /// The last frame had no valid head orientation.
    untracked: bool,
    /// The canvas drawn into the headset: the first the host renders.
    canvas: Option<u32>,
    /// Host clock − headset clock, milliseconds, fixed at the first frame.
    clock_offset: Option<f64>,
    perf: Perf,
    frames: u64,
    refresh: Option<xr::raw::DisplayRefreshRateFB>,
    foveation: Option<Foveation>,
    capture: Capture,
    /// The controllers' actions; None where the runtime refused them.
    controls: Option<crate::controls::Controls>,
    /// The last frame's time on the host's clock: what the next frame's
    /// input is stamped with, so the world's next tick takes it.
    last_frame_ms: Option<f64>,
    /// The canvas refused its input once (logged once).
    refused_input: bool,
}

struct Swapchain {
    handle: xr::Swapchain<xr::Vulkan>,
    /// The `VkImage` of each index.
    images: Vec<u64>,
    /// A `VkFormat`.
    format: u32,
    size: (u32, u32),
    /// The images' usage besides colour attachment, as the module names it.
    usage: u32,
}

/// `XR_FB_foveation` and its swapchain update, where the runtime has them.
pub(crate) struct Foveation {
    pub(crate) fov: xr::raw::FoveationFB,
    pub(crate) config: xr::raw::FoveationConfigurationFB,
    pub(crate) update: xr::raw::SwapchainUpdateStateFB,
}

// SAFETY: OpenXR handles are usable from any thread with external
// synchronisation, which the mutex gives.
unsafe impl Send for Headset {}

pub(crate) static HEADSET: Mutex<Option<Headset>> = Mutex::new(None);

impl Headset {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        instance: xr::Instance,
        system: xr::SystemId,
        session: xr::Session<xr::Vulkan>,
        waiter: xr::FrameWaiter,
        stream: xr::FrameStream<xr::Vulkan>,
        refresh: Option<xr::raw::DisplayRefreshRateFB>,
        foveation: Option<Foveation>,
        controls: Option<crate::controls::Controls>,
    ) -> Self {
        Self {
            instance,
            system,
            session,
            waiter,
            stream,
            space: None,
            swapchain: None,
            running: false,
            focused: false,
            exiting: false,
            untracked: false,
            canvas: None,
            clock_offset: None,
            perf: Perf::default(),
            frames: 0,
            refresh,
            foveation,
            capture: Capture::default(),
            controls,
            last_frame_ms: None,
            refused_input: false,
        }
    }

    /// Handle the runtime's events; whether the session runs.
    fn poll(&mut self, module: &Module) -> xr::Result<()> {
        let mut buffer = xr::EventDataBuffer::new();
        while let Some(event) = self.instance.poll_event(&mut buffer)? {
            use xr::Event;
            match event {
                Event::SessionStateChanged(change) => {
                    let state = change.state();
                    log(false, &format!("session state {state:?}"));
                    match state {
                        xr::SessionState::READY => {
                            self.session
                                .begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                            self.running = true;
                            self.request_refresh_rate();
                            self.request_fixed_levels();
                        }
                        xr::SessionState::STOPPING => {
                            self.session.end()?;
                            self.running = false;
                        }
                        xr::SessionState::FOCUSED => {
                            self.focused = true;
                            if let Some(id) = self.canvas {
                                module.lifecycle(id, 3);
                            }
                        }
                        xr::SessionState::VISIBLE => {
                            if self.focused {
                                if let Some(id) = self.canvas {
                                    module.lifecycle(id, 2);
                                }
                            }
                            self.focused = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                            self.running = false;
                            self.exiting = true;
                            module.set_headset(false);
                        }
                        _ => {}
                    }
                }
                Event::InstanceLossPending(_) => {
                    self.running = false;
                    self.exiting = true;
                    module.set_headset(false);
                }
                Event::InteractionProfileChanged(_) => {
                    if let Some(controls) = &self.controls {
                        controls.profile_changed(&self.instance, &self.session);
                    }
                }
                // A recenter (the Meta button held): the runtime re-bases
                // LOCAL and LOCAL_FLOOR about the person.
                Event::ReferenceSpaceChangePending(change) => {
                    log(
                        false,
                        &format!(
                            "reference space {:?} changes (a recenter)",
                            change.reference_space_type()
                        ),
                    );
                    if let Some(controls) = &mut self.controls {
                        controls.recentered();
                    }
                }
                Event::DisplayRefreshRateChangedFB(change) => log(
                    false,
                    &format!(
                        "display {} Hz → {} Hz",
                        change.from_display_refresh_rate(),
                        change.to_display_refresh_rate()
                    ),
                ),
                _ => {}
            }
        }
        Ok(())
    }

    /// The `fixed-levels` debug switch (`docs/quest.md`, "Debug switches"):
    /// CPU and GPU held at XR_EXT_performance_settings' sustained-high level,
    /// so that A/B measurements are not skewed by the runtime lowering the
    /// clocks of a lighter frame. Never on otherwise: the runtime's dynamic
    /// levels save power.
    fn request_fixed_levels(&self) {
        if !java::debug_switch("fixed-levels") {
            return;
        }
        let Some(perf) = self.instance.exts().ext_performance_settings else {
            log(
                true,
                "fixed-levels: the runtime has no XR_EXT_performance_settings",
            );
            return;
        };
        for domain in [
            xr::sys::PerfSettingsDomainEXT::CPU,
            xr::sys::PerfSettingsDomainEXT::GPU,
        ] {
            // SAFETY: the extension's function on this live session.
            let result = unsafe {
                (perf.perf_settings_set_performance_level)(
                    self.session.as_raw(),
                    domain,
                    xr::sys::PerfSettingsLevelEXT::SUSTAINED_HIGH,
                )
            };
            log(
                true,
                &format!("fixed-levels: {domain:?} sustained high ({result:?})"),
            );
        }
    }

    fn request_refresh_rate(&self) {
        let Some(refresh) = self.refresh else { return };
        let mut count = 0u32;
        // SAFETY: the extension's functions on this session.
        unsafe {
            let session = self.session.as_raw();
            if (refresh.enumerate_display_refresh_rates)(
                session,
                0,
                &mut count,
                std::ptr::null_mut(),
            )
            .into_raw()
                < 0
            {
                return;
            }
            let mut rates = vec![0f32; count as usize];
            (refresh.enumerate_display_refresh_rates)(
                session,
                count,
                &mut count,
                rates.as_mut_ptr(),
            );
            let rate = if rates.iter().any(|r| (r - REFRESH_HZ).abs() < 0.5) {
                REFRESH_HZ
            } else {
                rates.iter().copied().fold(0.0, f32::max)
            };
            let result = (refresh.request_display_refresh_rate)(session, rate);
            log(
                false,
                &format!("display rates {rates:?}: asked for {rate} Hz ({result:?})"),
            );
        }
    }

    fn space(&mut self) -> xr::Result<&xr::Space> {
        if self.space.is_none() {
            let floor = self
                .session
                .enumerate_reference_spaces()?
                .contains(&xr::ReferenceSpaceType::LOCAL_FLOOR);
            let space = if floor {
                log(false, "reference space LOCAL_FLOOR");
                self.session.create_reference_space(
                    xr::ReferenceSpaceType::LOCAL_FLOOR,
                    xr::Posef::IDENTITY,
                )?
            } else {
                // The floor 1.6 m under where the head started.
                log(
                    false,
                    "reference space LOCAL, the floor 1.6 m under the head",
                );
                self.session.create_reference_space(
                    xr::ReferenceSpaceType::LOCAL,
                    xr::Posef {
                        orientation: xr::Quaternionf::IDENTITY,
                        position: xr::Vector3f {
                            x: 0.0,
                            y: -1.6,
                            z: 0.0,
                        },
                    },
                )?
            };
            self.space = Some(space);
        }
        Ok(self.space.as_ref().unwrap())
    }

    fn swapchain(&mut self, module: &Module, id: u32) -> Result<(), String> {
        if self.swapchain.is_some() {
            return Ok(());
        }
        let views = self
            .instance
            .enumerate_view_configuration_views(
                self.system,
                xr::ViewConfigurationType::PRIMARY_STEREO,
            )
            .map_err(|e| format!("views: {e}"))?;
        let view = views.first().ok_or("no views")?;
        let mut size = (
            view.recommended_image_rect_width,
            view.recommended_image_rect_height,
        );
        // `eye-scale-<k>` (a measuring switch, docs/quest.md): eyes of k times
        // the recommended size, to price the resolution against the picture.
        static SCALE: OnceLock<Option<f32>> = OnceLock::new();
        let scale = SCALE.get_or_init(|| {
            java::debug_switches()
                .split_whitespace()
                .find_map(|w| w.strip_prefix("eye-scale-")?.parse::<f32>().ok())
                .filter(|k| (0.25..=1.5).contains(k))
        });
        if let Some(k) = *scale {
            size = (
                (size.0 as f32 * k).round() as u32,
                (size.1 as f32 * k).round() as u32,
            );
            log(
                false,
                &format!("applied: eye-scale {k}: {}x{}", size.0, size.1),
            );
        }
        let formats = self
            .session
            .enumerate_swapchain_formats()
            .map_err(|e| format!("formats: {e}"))?;
        // The canvas's own format when it is sRGB and offered (its renderer
        // is kept), else sRGB RGBA, else sRGB BGRA.
        let format = module
            .vulkan_format(id)
            .into_iter()
            .chain([R8G8B8A8_SRGB, B8G8R8A8_SRGB])
            .filter(|f| [R8G8B8A8_SRGB, B8G8R8A8_SRGB].contains(f))
            .find(|f| formats.contains(f))
            .ok_or_else(|| format!("no sRGB colour format among {formats:?}"))?;
        let usage_full = xr::SwapchainUsageFlags::COLOR_ATTACHMENT
            | xr::SwapchainUsageFlags::SAMPLED
            | xr::SwapchainUsageFlags::TRANSFER_SRC
            | xr::SwapchainUsageFlags::TRANSFER_DST;
        let info = |usage| xr::SwapchainCreateInfo::<xr::Vulkan> {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: usage,
            format,
            sample_count: 1,
            width: size.0,
            height: size.1,
            face_count: 1,
            array_size: 2,
            mip_count: 1,
        };
        // `swap-color-only` (`xr-debug`): a swapchain only drawn into, as a
        // measurement of what its other usages cost (the eye capture then
        // cannot read it back).
        let color_only = java::debug_switch("swap-color-only");
        let full = if color_only {
            None
        } else {
            self.session.create_swapchain(&info(usage_full)).ok()
        };
        let (handle, usage) = match full {
            Some(handle) => (handle, USAGE_ALL),
            None => (
                self.session
                    .create_swapchain(&info(xr::SwapchainUsageFlags::COLOR_ATTACHMENT))
                    .map_err(|e| format!("swapchain: {e}"))?,
                0,
            ),
        };
        let images = handle
            .enumerate_images()
            .map_err(|e| format!("images: {e}"))?;
        log(
            false,
            &format!(
                "swapchain {}×{}×2 {}, {} images, usage {}",
                size.0,
                size.1,
                if format == R8G8B8A8_SRGB {
                    "R8G8B8A8_SRGB"
                } else {
                    "B8G8R8A8_SRGB"
                },
                images.len(),
                if usage == USAGE_ALL {
                    "colour+sampled+copy"
                } else {
                    "colour"
                }
            ),
        );
        self.swapchain = Some(Swapchain {
            handle,
            images,
            format,
            size,
            usage,
        });
        self.foveate();
        Ok(())
    }

    /// Fixed foveation, HIGH and dynamic, on the colour swapchain
    /// (`XR_FB_foveation`). On Vulkan the runtime applies it through a
    /// fragment density map the render pass must use, which wgpu's passes
    /// do not yet: until then this is asked for and has no effect.
    fn foveate(&self) {
        let (Some(f), Some(swapchain)) = (self.foveation.as_ref(), self.swapchain.as_ref()) else {
            log(false, "foveation: not offered by the runtime");
            return;
        };
        // SAFETY: the extensions' functions with valid structures.
        unsafe {
            let level = xr::sys::FoveationLevelProfileCreateInfoFB {
                ty: xr::sys::FoveationLevelProfileCreateInfoFB::TYPE,
                next: std::ptr::null_mut(),
                level: xr::sys::FoveationLevelFB::HIGH,
                vertical_offset: 0.0,
                dynamic: xr::sys::FoveationDynamicFB::LEVEL_ENABLED,
            };
            let info = xr::sys::FoveationProfileCreateInfoFB {
                ty: xr::sys::FoveationProfileCreateInfoFB::TYPE,
                next: &level as *const _ as *mut _,
            };
            let mut profile = <xr::sys::FoveationProfileFB as xr::sys::Handle>::NULL;
            let made = (f.fov.create_foveation_profile)(self.session.as_raw(), &info, &mut profile);
            if made.into_raw() < 0 {
                log(true, &format!("foveation profile: {made:?}"));
                return;
            }
            let state = xr::sys::SwapchainStateFoveationFB {
                ty: xr::sys::SwapchainStateFoveationFB::TYPE,
                next: std::ptr::null_mut(),
                flags: xr::sys::SwapchainStateFoveationFlagsFB::EMPTY,
                profile,
            };
            let updated = (f.update.update_swapchain)(
                swapchain.handle.as_raw(),
                &state as *const _ as *const _,
            );
            log(false, &format!("foveation HIGH, dynamic: {updated:?} (no effect without a fragment density map)"));
            let _ = (f.fov.destroy_foveation_profile)(profile);
            let _ = &f.config;
        }
    }
}

/// `VK_FORMAT_R8G8B8A8_SRGB` and `VK_FORMAT_B8G8R8A8_SRGB`.
const R8G8B8A8_SRGB: u32 = 43;
const B8G8R8A8_SRGB: u32 = 50;

/// Eye → clip from an OpenXR view's field of view (0–1 depth).
fn projection(fov: xr::Fovf, near: f32, far: f32) -> [f32; 16] {
    fov_projection(
        [
            fov.angle_left,
            fov.angle_right,
            fov.angle_up,
            fov.angle_down,
        ],
        near,
        far,
    )
}

/// Reference space → eye from an OpenXR view's pose.
fn view(pose: xr::Posef) -> [f32; 16] {
    let (q, p) = (pose.orientation, pose.position);
    pose_view([q.x, q.y, q.z, q.w], [p.x, p.y, p.z])
}

/// Canvas `id`'s frame while a headset session exists: Some(the render
/// result for the host, as `gpu_render`'s: 1 another frame, 0 none), or None
/// to render it as usual (no session, or one that ended).
pub(crate) fn render(module: &Module, id: u32, host_now_ms: f64) -> Option<u32> {
    let mut slot = HEADSET.lock().unwrap_or_else(|e| e.into_inner());
    let headset = slot.as_mut()?;
    if headset.exiting {
        return None;
    }
    if headset.canvas.is_some_and(|c| c != id) {
        // Not drawn while the headset shows another.
        return Some(0);
    }
    if let Err(e) = headset.poll(module) {
        log(true, &format!("events: {e}"));
    }
    if !headset.running {
        // Not shown until the session runs; rest a little so a host that
        // keeps asking does not spin.
        std::thread::sleep(Duration::from_millis(5));
        return Some(1);
    }
    match frame(headset, module, id, host_now_ms) {
        Ok(()) => Some(1),
        Err(e) => {
            log(true, &format!("frame: {e}"));
            Some(1)
        }
    }
}

fn frame(headset: &mut Headset, module: &Module, id: u32, host_now_ms: f64) -> Result<(), String> {
    let waited = Instant::now();
    let state = headset.waiter.wait().map_err(|e| format!("wait: {e}"))?;
    let wait_ms = waited.elapsed().as_secs_f64() * 1e3;
    let started = Instant::now();
    headset.capture.poll();
    headset.stream.begin().map_err(|e| format!("begin: {e}"))?;
    let time = state.predicted_display_time;
    if !state.should_render {
        headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[])
            .map_err(|e| format!("end: {e}"))?;
        headset.perf.frame(wait_ms, 0.0, 0.0, false, None);
        return Ok(());
    }
    if let Err(e) = headset.swapchain(module, id) {
        let _ = headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[]);
        return Err(e);
    }
    headset.space().map_err(|e| format!("space: {e}"))?;
    let space = headset.space.as_ref().unwrap();
    let (located, mut views) = headset
        .session
        .locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, time, space)
        .map_err(|e| format!("views: {e}"))?;
    if views.len() < 2 {
        let _ = headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[]);
        return Err(format!("{} views", views.len()));
    }
    // A capture from a fixed pose: both eyes look at the stage from it, for
    // this frame only, whether or not anyone wears the headset.
    let capture = headset.capture.pending.take();
    let held = headset.capture.hold;
    let fixed = capture.and_then(|c| c.pose).or(held).map(|pose| {
        let focus = module.xr_focus(id).unwrap_or([0.0, 1.0, -0.8]);
        for (view, (q, p)) in views.iter_mut().zip(pose.eyes(focus)) {
            view.pose = xr::Posef {
                orientation: xr::Quaternionf {
                    x: q[0],
                    y: q[1],
                    z: q[2],
                    w: q[3],
                },
                position: xr::Vector3f {
                    x: p[0],
                    y: p[1],
                    z: p[2],
                },
            };
            let f = view.fov;
            if [f.angle_left, f.angle_right, f.angle_up, f.angle_down]
                .iter()
                .all(|a| *a == 0.0)
            {
                let a = 45f32.to_radians();
                view.fov = xr::Fovf {
                    angle_left: -a,
                    angle_right: a,
                    angle_up: a,
                    angle_down: -a,
                };
            }
        }
        let focus = pose.focus.unwrap_or(focus);
        format!(
            "{} pose {:.2} m, {:.0}° down, {:.0}° turned, looking at {focus:?}",
            if pose.focus.is_some() {
                "look"
            } else {
                "stage"
            },
            pose.distance,
            pose.pitch_deg,
            pose.yaw_deg
        )
    });
    // Tracking lost (a headset nobody wears, a dark room): the compositor
    // refuses a layer without a valid orientation, so the frame shows
    // nothing and the world waits.
    if fixed.is_none() && !located.contains(xr::ViewStateFlags::ORIENTATION_VALID) {
        // A capture of the tracked head waits for tracking.
        headset.capture.pending = capture;
        headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[])
            .map_err(|e| format!("end: {e}"))?;
        if !headset.untracked {
            log(
                true,
                "tracking lost: frames show nothing until the head is tracked again",
            );
        }
        headset.untracked = true;
        headset.perf.frame(wait_ms, 0.0, 0.0, false, None);
        return Ok(());
    }
    if std::mem::take(&mut headset.untracked) {
        log(false, "tracking again");
    }
    let swapchain = headset.swapchain.as_mut().unwrap();
    let image = swapchain
        .handle
        .acquire_image()
        .map_err(|e| format!("acquire: {e}"))?;
    swapchain
        .handle
        .wait_image(xr::Duration::INFINITE)
        .map_err(|e| format!("wait image: {e}"))?;
    let target = StereoTarget {
        image: swapchain.images[image as usize],
        format: swapchain.format,
        width: swapchain.size.0,
        height: swapchain.size.1,
        usage: swapchain.usage,
        views: [view(views[0].pose), view(views[1].pose)],
        projections: [
            projection(views[0].fov, NEAR, FAR),
            projection(views[1].fov, NEAR, FAR),
        ],
    };
    // The world's clock: the frame's predicted display time on the host's.
    let xr_ms = time.as_nanos() as f64 / 1e6;
    let offset = *headset.clock_offset.get_or_insert_with(|| {
        let offset = host_now_ms - xr_ms;
        log(false, &format!("clocks: host − headset = {offset:.1} ms"));
        if offset.abs() < 1000.0 {
            0.0
        } else {
            offset
        }
    });
    if headset.canvas.is_none() {
        log(false, &format!("canvas {id} draws in the headset"));
        headset.canvas = Some(id);
    }
    // The controllers, before the world advances to this frame: stamped at
    // the last frame's time, so the next tick takes them rather than the one
    // whose window holds this frame's display time.
    let now_ms = xr_ms + offset;
    if let Some(controls) = headset.controls.as_mut() {
        let at_ms = headset.last_frame_ms.unwrap_or(now_ms).min(now_ms);
        let (input, messages) = controls.frame(&headset.session, space, time, java::files_dir);
        let mut delivered =
            input.map_or(Ok(()), |input| module.input_xr(id, &input.floats(), at_ms));
        for text in messages {
            delivered = delivered.and(module.message(id, &text, at_ms));
        }
        if let Err(e) = delivered {
            if !std::mem::replace(&mut headset.refused_input, true) {
                log(
                    true,
                    &format!("controllers: canvas {id} refused its input: {e}"),
                );
            }
        }
    }
    headset.last_frame_ms = Some(now_ms);
    let encode = Instant::now();
    // The module hands the image back in COLOR_ATTACHMENT_OPTIMAL.
    let drawn = module.render_stereo(id, &target, now_ms);
    let encode_ms = encode.elapsed().as_secs_f64() * 1e3;
    if capture.is_some() && drawn.is_ok() {
        let what = fixed.as_deref().unwrap_or("the tracked head");
        headset.capture.take(module, swapchain.size, what);
    }
    swapchain
        .handle
        .release_image()
        .map_err(|e| format!("release: {e}"))?;
    if let Err(why) = drawn {
        let _ = headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[]);
        return Err(format!("canvas {id}: {why}"));
    }
    let (w, h) = swapchain.size;
    let rect = xr::Rect2Di {
        offset: xr::Offset2Di { x: 0, y: 0 },
        extent: xr::Extent2Di {
            width: w as i32,
            height: h as i32,
        },
    };
    let projection_views = [0, 1].map(|i| {
        xr::CompositionLayerProjectionView::new()
            .pose(views[i].pose)
            .fov(views[i].fov)
            .sub_image(
                xr::SwapchainSubImage::new()
                    .swapchain(&swapchain.handle)
                    .image_array_index(i as u32)
                    .image_rect(rect),
            )
    });
    let layer = xr::CompositionLayerProjection::new()
        .space(space)
        .views(&projection_views);
    headset
        .stream
        .end(time, xr::EnvironmentBlendMode::OPAQUE, &[&layer])
        .map_err(|e| format!("end: {e}"))?;
    headset.frames += 1;
    if headset.frames == 1 {
        log(false, "first frame submitted");
    }
    let cpu_ms = started.elapsed().as_secs_f64() * 1e3;
    let mode = module.xr_stereo_mode(id);
    headset
        .perf
        .frame(wait_ms, cpu_ms, encode_ms, true, Some(mode));
    Ok(())
}
