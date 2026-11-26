//! DLSS frame generation (`VICTORY_FRAMEGEN=dlss`, beside DLSS super
//! resolution): the runtime's `sl.dlss_g` makes a frame between each two
//! the title presents, from the upscale's depth and motion vectors and the
//! presented pictures, and paces both to the display itself.
//!
//! What it needs of us, and what it gets:
//! - The swap chain made through the runtime (`super::swap_chain`, as
//!   for super resolution alone): every present, back-buffer query and
//!   resize of ours goes through it. With frame generation on, its
//!   `Present` returns at once and the runtime presents both frames from a
//!   thread of its own.
//! - Depth and motion vectors of each presented frame: tagged at the
//!   upscale ([`after_upscale`]) as valid only then, so the runtime copies
//!   them in the title's list; the title reuses both textures before the
//!   present.
//! - The frame's constants, set once, at the upscale; the same frame token
//!   then marks the present ([`before_present`], [`after_present`]).
//! - Reflex, through the runtime (`sl.reflex`, `sl.pcl`): low-latency mode
//!   on, a sleep and the six frame markers each frame. The simulation and
//!   submission markers are all set at the upscale, where the frame first
//!   becomes known to us: they bracket nothing real.
//!
//! Not given: the picture without the title's interface (`HUD-less
//! colour`) and the interface's own colour and alpha, which the title
//! draws after the upscale into textures we do not identify; the generated
//! frame interpolates the interface with the scene. The camera matrices of
//! the constants are the ones super resolution gets (rebuilt projection,
//! no motion between clip spaces).

use std::collections::VecDeque;
use std::ffi::{CStr, c_void};
use std::ptr::null;
use std::sync::atomic::Ordering;

use windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList;
use windows::core::Interface;

use super::ffi::{self, Base, FrameToken, Resource};
use super::{Calls, PROXY, RUNTIME, Runtime, VIEWPORT, check, tag};

/// What frame generation needs in the folder beside super resolution.
pub(super) const FILES: [&str; 4] = ["sl.dlss_g.dll", "sl.reflex.dll", "sl.pcl.dll", "nvngx_dlssg.dll"];
pub(super) const FEATURES: [u32; 3] = [ffi::FEATURE_DLSS_G, ffi::FEATURE_REFLEX, ffi::FEATURE_PCL];

pub(super) struct Functions {
    set_options: ffi::DlssgSetOptions,
    get_state: ffi::DlssgGetState,
    reflex_sleep: ffi::ReflexSleep,
    set_marker: ffi::PclSetMarker,
    set_tag_for_frame: ffi::SetTagForFrame,
}

/// A frame the runtime has constants and inputs for, until it is presented.
struct Token(*mut FrameToken);

// SAFETY: the runtime's token, only ever handed back to it, under the
// calls' mutex.
unsafe impl Send for Token {}

#[derive(Default)]
pub(super) struct State {
    /// Upscaled frames not presented yet, oldest first.
    pending: VecDeque<Token>,
    /// The swap-chain size generation is on for.
    on: Option<[u32; 2]>,
    presents: u64,
    presented: u64,
    status: u32,
    failed: bool,
}

/// Readies frame generation: supported on the adapter, its functions
/// found, Reflex on. `find` gives an interposer export, `function` a
/// feature's function.
pub(super) fn load(
    supported: impl Fn(u32) -> i32,
    find: impl Fn(&CStr) -> Result<*const c_void, String>,
    function: impl Fn(u32, &CStr) -> Result<*const c_void, String>,
) -> Result<Functions, String> {
    for (feature, name) in [(ffi::FEATURE_DLSS_G, "frame generation"), (ffi::FEATURE_REFLEX, "Reflex"), (ffi::FEATURE_PCL, "PC latency markers")] {
        check(&format!("{name} on this adapter (slIsFeatureSupported)"), supported(feature))?;
    }
    // SAFETY: the plugins' functions and the interposer's exports, with the
    // signatures of sl_dlss_g.h, sl_reflex.h, sl_pcl.h and sl_core_api.h.
    let (functions, reflex_set_options) = unsafe {
        use std::mem::transmute;
        (
            Functions {
                set_options: transmute::<*const c_void, ffi::DlssgSetOptions>(function(ffi::FEATURE_DLSS_G, c"slDLSSGSetOptions")?),
                get_state: transmute::<*const c_void, ffi::DlssgGetState>(function(ffi::FEATURE_DLSS_G, c"slDLSSGGetState")?),
                reflex_sleep: transmute::<*const c_void, ffi::ReflexSleep>(function(ffi::FEATURE_REFLEX, c"slReflexSleep")?),
                set_marker: transmute::<*const c_void, ffi::PclSetMarker>(function(ffi::FEATURE_PCL, c"slPCLSetMarker")?),
                set_tag_for_frame: transmute::<*const c_void, ffi::SetTagForFrame>(find(c"slSetTagForFrame")?),
            },
            transmute::<*const c_void, ffi::ReflexSetOptions>(function(ffi::FEATURE_REFLEX, c"slReflexSetOptions")?),
        )
    };
    let reflex = ffi::ReflexOptions {
        base: Base::new(ffi::REFLEX_OPTIONS, 1),
        mode: ffi::REFLEX_LOW_LATENCY,
        frame_limit_us: 0,
        use_markers_to_optimize: false,
        virtual_key: 0,
        id_thread: 0,
    };
    // SAFETY: our options, alive for the call.
    check("slReflexSetOptions", unsafe { reflex_set_options(&reflex) })?;
    Ok(functions)
}

/// Whether frame generation is loaded.
pub(crate) fn enabled() -> bool {
    RUNTIME.get().is_some_and(|rt| rt.generation.is_some())
}

fn marker(g: &Functions, marker: u32, token: *mut FrameToken) {
    // SAFETY: the runtime's token for a frame of ours.
    unsafe { (g.set_marker)(marker, token) };
}

/// After an upscale was recorded into `list` for the frame `token`: the
/// frame's depth and motion vectors are the runtime's to copy there, and
/// the frame waits for its present.
pub(super) fn after_upscale(
    rt: &Runtime,
    calls: &mut Calls,
    list: &ID3D12GraphicsCommandList,
    token: *mut FrameToken,
    depth: &mut Resource,
    motion: &mut Resource,
    sizes: [[u32; 2]; 2],
) {
    let Some(g) = &rt.generation else { return };
    if !PROXY.load(Ordering::Relaxed) || calls.generation.failed {
        return;
    }
    let mut tags = [tag(depth, ffi::BUFFER_DEPTH, sizes[0]), tag(motion, ffi::BUFFER_MOTION_VECTORS, sizes[1])];
    for t in &mut tags {
        t.lifecycle = ffi::ONLY_VALID_NOW;
    }
    // SAFETY: our tags, alive for the call; the list is open and the
    // textures are in the states named.
    let code = unsafe { (g.set_tag_for_frame)(token, &VIEWPORT, tags.as_ptr(), tags.len() as u32, list.as_raw()) };
    if let Err(why) = check("slSetTagForFrame", code) {
        tracing::warn!("DLSS frame generation: the frame's depth and motion were refused ({why}); off");
        calls.generation.failed = true;
        return;
    }
    for m in [ffi::MARKER_SIMULATION_START, ffi::MARKER_SIMULATION_END, ffi::MARKER_RENDER_SUBMIT_START] {
        marker(g, m, token);
    }
    // A title that upscales without presenting does not pile frames up.
    while calls.generation.pending.len() >= 3 {
        calls.generation.pending.pop_front();
    }
    calls.generation.pending.push_back(Token(token));
}

fn options(mode: u32, size: [u32; 2]) -> ffi::DlssgOptions {
    ffi::DlssgOptions {
        base: Base::new(ffi::DLSSG_OPTIONS, 5),
        mode,
        num_frames_to_generate: 1,
        flags: 0,
        dynamic_res_width: 0,
        dynamic_res_height: 0,
        num_back_buffers: 0,
        mvec_depth_width: 0,
        mvec_depth_height: 0,
        color_width: size[0],
        color_height: size[1],
        color_buffer_format: 0,
        mvec_buffer_format: 0,
        depth_buffer_format: 0,
        hud_less_buffer_format: 0,
        ui_buffer_format: 0,
        on_error_callback: null(),
        reserved15: ffi::INVALID,
        queue_parallelism_mode: 0,
        enable_user_interface_recomposition: ffi::FALSE,
        dynamic_target_frame_rate: 0.0,
    }
}

/// A present the runtime was told about, to be finished after it.
pub(crate) struct Presenting(Token);

/// Before the presenter presents at `size`: generation on if the frame was
/// upscaled (its inputs are the runtime's), off for any other frame (a
/// menu, a video). The frame to finish with [`after_present`], if on.
pub(crate) fn before_present(size: [u32; 2]) -> Option<Presenting> {
    if !PROXY.load(Ordering::Relaxed) {
        return None;
    }
    let rt = RUNTIME.get()?;
    let g = rt.generation.as_ref()?;
    let mut calls = rt.calls.lock().unwrap_or_else(|e| e.into_inner());
    let failed = calls.generation.failed;
    let frame = calls.generation.pending.pop_front().filter(|_| !failed);
    let wanted = frame.as_ref().map(|_| size);
    if calls.generation.on != wanted {
        let o = options(if wanted.is_some() { ffi::DLSSG_ON } else { ffi::DLSSG_OFF }, size);
        // SAFETY: our options, alive for the call.
        let code = unsafe { (g.set_options)(&VIEWPORT, &o) };
        tracing::info!(on = wanted.is_some(), ?size, result = ffi::result_name(code), "DLSS frame generation: switched (slDLSSGSetOptions)");
        if code != ffi::OK {
            calls.generation.failed = wanted.is_some();
            return None;
        }
        calls.generation.on = wanted;
    }
    let frame = frame?;
    marker(g, ffi::MARKER_RENDER_SUBMIT_END, frame.0);
    marker(g, ffi::MARKER_PRESENT_START, frame.0);
    Some(Presenting(frame))
}

/// After the present [`before_present`] prepared.
pub(crate) fn after_present(presenting: Presenting) {
    let Some(rt) = RUNTIME.get() else { return };
    let Some(g) = &rt.generation else { return };
    let token = presenting.0.0;
    marker(g, ffi::MARKER_PRESENT_END, token);
    let mut state = ffi::DlssgState {
        base: Base::new(ffi::DLSSG_STATE, 4),
        estimated_vram_usage_in_bytes: 0,
        status: 0,
        min_width_or_height: 0,
        num_frames_actually_presented: 0,
        num_frames_to_generate_max: 0,
        reserved4: 0,
        is_vsync_support_available: 0,
        inputs_processing_completion_fence: std::ptr::null_mut(),
        last_present_inputs_processing_completion_fence_value: 0,
        is_dynamic_mfg_supported: 0,
    };
    {
        let mut calls = rt.calls.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: our state structure to fill.
        let code = unsafe { (g.get_state)(&VIEWPORT, &mut state, null()) };
        let c = &mut calls.generation;
        c.presents += 1;
        c.presented += u64::from(state.num_frames_actually_presented);
        if code != ffi::OK || state.status != c.status || c.presents == 1 || c.presents % 300 == 0 {
            let problems: Vec<&str> = ffi::DLSSG_STATUS.iter().filter(|(bit, _)| state.status & bit != 0).map(|&(_, what)| what).collect();
            tracing::info!(
                result = ffi::result_name(code),
                status = state.status,
                ?problems,
                presents = c.presents,
                frames_shown = c.presented,
                most_generated_per_frame = state.num_frames_to_generate_max,
                smallest_side = state.min_width_or_height,
                "DLSS frame generation: state after a present (frames shown counts ours and the generated ones)"
            );
            c.status = state.status;
        }
    }
    // Reflex paces the next frame; outside the lock, it may wait.
    // SAFETY: the runtime's token.
    unsafe { (g.reflex_sleep)(token) };
}

/// Generation off before the swap chain's buffers are made again.
pub(crate) fn before_resize(size: [u32; 2]) {
    let Some(rt) = RUNTIME.get() else { return };
    let Some(g) = &rt.generation else { return };
    let mut calls = rt.calls.lock().unwrap_or_else(|e| e.into_inner());
    if calls.generation.on.take().is_some() {
        // SAFETY: our options, alive for the call.
        unsafe { (g.set_options)(&VIEWPORT, &options(ffi::DLSSG_OFF, size)) };
    }
    calls.generation.pending.clear();
}
