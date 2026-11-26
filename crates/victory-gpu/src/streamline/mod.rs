//! NVIDIA DLSS through the user's copy of Streamline (`VICTORY_DLSS_DIR`:
//! the folder with `sl.interposer.dll`, its `sl.*.dll` plugins and the
//! `nvngx_*.dll` models). Off unless that folder is named and the host
//! adapter is NVIDIA's.
//!
//! Nothing of NVIDIA's is built in or fetched: the DLLs are loaded at run
//! time from that folder only, each after its signature was checked
//! ([`crate::trust`]: valid, and NVIDIA Corporation's), and the runtime is told
//! not to update itself over the network (no `eAllowOTA`, no
//! `eLoadDownloadedPlugins`). The interface is written out by hand from the
//! SDK's public headers ([`ffi`]).
//!
//! The runtime is used the way its guide calls manual hooking: it gets our
//! host device (`slSetD3DDevice`) and no object of ours is replaced by one
//! of its proxies. What it records into a command list is recorded where
//! the title's upscaler would have been (`external::record`), on resources
//! whose states we name; the list's bindings are ours to put back after it.
//!
//! Frame generation ([`generate`], `VICTORY_FRAMEGEN=dlss`) is the one
//! exception to "no proxies": the presenter's swap chain is then made by a
//! proxy factory of the runtime's, as `sl.dlss_g` requires.
//!
//! Super resolution ([`Dlss`], one of the two backends of
//! [`crate::upscale`]) is what `victory-hle`'s FidelityFX module runs in
//! place of the title's AMD upscaler when DLSS serves.

mod ffi;
pub(crate) mod generate;

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory4};
use windows::core::Interface;

use crate::trust;
use crate::upscale::{Frame, Quality, Texture, Upscaler};
use ffi::{Base, Constants, DlssOptimalSettings, DlssOptions, Extent, Matrix, Resource, ResourceTag, ViewportHandle};

const INTERPOSER: &str = "sl.interposer.dll";
/// What super resolution needs beside the interposer.
const SUPER_RESOLUTION: [&str; 3] = ["sl.common.dll", "sl.dlss.dll", "nvngx_dlss.dll"];
const NVIDIA_VENDOR: u32 = 0x10de;

/// Identifies this project to the runtime (`sl::Preferences::projectId`): a
/// GUID of ours, made up once. Not an NVIDIA application id, which only
/// NVIDIA assigns; with none, the runtime wants an engine type and version
/// beside it.
const PROJECT_ID: &CStr = c"8f0d2c5e-6b1a-4f39-9a7e-3c4d1b2e5f60";
const ENGINE_VERSION: &CStr = c"1.0";

/// The one viewport everything of ours is evaluated in.
const VIEWPORT: ViewportHandle = ViewportHandle::new(0);

/// A quality mode as `sl::DLSSMode`.
fn dlss_mode(quality: Quality) -> u32 {
    match quality {
        Quality::Native => ffi::DLSS_DLAA,
        Quality::Quality => ffi::DLSS_MAX_QUALITY,
        Quality::Balanced => ffi::DLSS_BALANCED,
        Quality::Performance => ffi::DLSS_MAX_PERFORMANCE,
        Quality::UltraPerformance => ffi::DLSS_ULTRA_PERFORMANCE,
    }
}

/// The mode asked for; quality without one (the render scale the title's
/// Series S profile has anyway).
pub fn asked_mode() -> Quality {
    crate::upscale::asked_quality().unwrap_or(Quality::Quality)
}

struct Api {
    shutdown: ffi::Shutdown,
    is_feature_supported: ffi::IsFeatureSupported,
    get_feature_function: ffi::GetFeatureFunction,
    get_feature_version: ffi::GetFeatureVersion,
    get_new_frame_token: ffi::GetNewFrameToken,
    set_constants: ffi::SetConstants,
    evaluate_feature: ffi::EvaluateFeature,
}

struct DlssFunctions {
    get_optimal_settings: ffi::DlssGetOptimalSettings,
    set_options: ffi::DlssSetOptions,
}

struct Runtime {
    api: Api,
    dlss: DlssFunctions,
    /// Frame generation, if asked for and supported.
    generation: Option<generate::Functions>,
    /// Serializes every call into the runtime: its feature functions are
    /// not thread safe, and one frame's calls belong together.
    calls: Mutex<Calls>,
    /// The checked files, held open.
    _signed: Vec<trust::Signed>,
}

#[derive(Default)]
struct Calls {
    /// The sizes and mode the DLSS options were last set for.
    options: Option<([u32; 2], [u32; 2], Quality)>,
    upscales: u64,
    failures: u32,
    generation: generate::State,
}

// SAFETY: code addresses and plain data; the mutex serializes the calls.
unsafe impl Send for Runtime {}
// SAFETY: as above.
unsafe impl Sync for Runtime {}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Whether DLSS super resolution is ready on the host device.
pub fn serving() -> bool {
    RUNTIME.get().is_some()
}

/// The runtime's messages, into our log.
unsafe extern "C" fn log_message(kind: u32, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: a NUL-terminated message, alive for the call.
    let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let text = text.trim_end();
    match kind {
        0 => tracing::debug!(target: "victory_gpu::streamline::sl", "{text}"),
        1 => tracing::warn!(target: "victory_gpu::streamline::sl", "{text}"),
        _ => tracing::error!(target: "victory_gpu::streamline::sl", "{text}"),
    }
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn check(what: &str, code: i32) -> Result<(), String> {
    if code == ffi::OK { Ok(()) } else { Err(format!("{what}: {} ({code})", ffi::result_name(code))) }
}

/// Loads Streamline from `dir` and readies DLSS super resolution on the
/// host device. What was loaded, or why DLSS stays off (nothing of the
/// title's rendering changes then).
pub fn load(dir: &Path) -> Result<String, String> {
    if serving() {
        return Ok("already loaded".into());
    }
    let host = crate::host::host().ok_or("there is no host GPU")?;
    let device = &host.device;
    // SAFETY: plain queries of our device and its adapter.
    let (luid, vendor, name) = unsafe {
        let luid = device.GetAdapterLuid();
        let adapter: IDXGIAdapter1 = CreateDXGIFactory1::<IDXGIFactory4>()
            .and_then(|f| f.EnumAdapterByLuid(luid))
            .map_err(|e| format!("the host adapter cannot be asked what it is ({e})"))?;
        let desc = adapter.GetDesc1().map_err(|e| e.to_string())?;
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        (luid, desc.VendorId, String::from_utf16_lossy(&desc.Description[..len]))
    };
    if vendor != NVIDIA_VENDOR {
        return Err(format!("the host adapter ({name}, vendor {vendor:#06x}) is not NVIDIA's"));
    }
    let dir = std::path::absolute(dir).map_err(|e| e.to_string())?;
    let mut signed = Vec::new();
    let generate = crate::upscale::asked_frame_generation() == crate::upscale::FrameGeneration::Dlss;
    let files = std::iter::once(INTERPOSER).chain(SUPER_RESOLUTION).chain(generate::FILES.into_iter().filter(|_| generate));
    for file in files {
        let path = dir.join(file);
        if !path.is_file() {
            return Err(format!("no {file} in {}", dir.display()));
        }
        signed.push(trust::signed_by(&path, trust::NVIDIA)?);
    }
    let interposer = dir.join(INTERPOSER);
    let module = {
        use windows_sys::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        };
        // Its own folder and System32 only, never the current folder.
        let search = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
        // SAFETY: a NUL-terminated full path to the file just checked.
        unsafe { LoadLibraryExW(wide(&interposer).as_ptr(), null_mut(), search) }
    };
    if module.is_null() {
        return Err(format!("{INTERPOSER} did not load ({})", std::io::Error::last_os_error()));
    }
    let find = |name: &CStr| {
        // SAFETY: a loaded module and a NUL-terminated name.
        unsafe { windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, name.as_ptr().cast()) }
            .map(|f| f as *const c_void)
            .ok_or_else(|| format!("{INTERPOSER} has no {}", name.to_string_lossy()))
    };
    // SAFETY: the interposer's exports, with the signatures of sl_core_api.h.
    let (init, set_device, api) = unsafe {
        use std::mem::transmute;
        (
            transmute::<*const c_void, ffi::Init>(find(c"slInit")?),
            transmute::<*const c_void, ffi::SetD3dDevice>(find(c"slSetD3DDevice")?),
            Api {
                shutdown: transmute::<*const c_void, ffi::Shutdown>(find(c"slShutdown")?),
                is_feature_supported: transmute::<*const c_void, ffi::IsFeatureSupported>(find(c"slIsFeatureSupported")?),
                get_feature_function: transmute::<*const c_void, ffi::GetFeatureFunction>(find(c"slGetFeatureFunction")?),
                get_feature_version: transmute::<*const c_void, ffi::GetFeatureVersion>(find(c"slGetFeatureVersion")?),
                get_new_frame_token: transmute::<*const c_void, ffi::GetNewFrameToken>(find(c"slGetNewFrameToken")?),
                set_constants: transmute::<*const c_void, ffi::SetConstants>(find(c"slSetConstants")?),
                evaluate_feature: transmute::<*const c_void, ffi::EvaluateFeature>(find(c"slEvaluateFeature")?),
            },
        )
    };

    let plugin_dir = wide(&dir);
    let plugin_dirs = [plugin_dir.as_ptr()];
    // Its own log file beside ours, if asked for (`VICTORY_DLSS_LOG_DIR`);
    // its warnings and errors reach our log either way.
    let log_dir = std::env::var_os("VICTORY_DLSS_LOG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    let log_dir_wide = log_dir.as_deref().map(wide);
    let verbose = std::env::var_os("VICTORY_DLSS_LOG").is_some_and(|v| v == "verbose");
    let mut features = vec![ffi::FEATURE_DLSS];
    if generate {
        features.extend(generate::FEATURES);
    }
    let preferences = ffi::Preferences {
        base: Base::new(ffi::PREFERENCES, 1),
        show_console: false,
        log_level: if verbose { ffi::LOG_VERBOSE } else { ffi::LOG_DEFAULT },
        paths_to_plugins: plugin_dirs.as_ptr(),
        num_paths_to_plugins: plugin_dirs.len() as u32,
        path_to_logs_and_data: log_dir_wide.as_ref().map_or(null(), |w| w.as_ptr()),
        allocate_callback: null(),
        release_callback: null(),
        log_message_callback: Some(log_message),
        // We put the list's state back ourselves; no proxies; tags belong
        // to a frame. No updates over the network.
        flags: ffi::DISABLE_CL_STATE_TRACKING
            | ffi::USE_MANUAL_HOOKING
            | ffi::USE_FRAME_BASED_RESOURCE_TAGGING
            | if generate { ffi::USE_DXGI_FACTORY_PROXY } else { 0 },
        features_to_load: features.as_ptr(),
        num_features_to_load: features.len() as u32,
        application_id: 0,
        engine: ffi::ENGINE_CUSTOM,
        engine_version: ENGINE_VERSION.as_ptr(),
        project_id: PROJECT_ID.as_ptr(),
        render_api: ffi::RENDER_D3D12,
    };
    // SAFETY: the preferences and what they point at live through the call.
    check("slInit", unsafe { init(&preferences, ffi::SDK_VERSION) })?;
    let shut = |why: String| {
        // SAFETY: initialized above; nothing of it is in use.
        unsafe { (api.shutdown)() };
        why
    };
    // SAFETY: our live host device.
    check("slSetD3DDevice", unsafe { set_device(device.as_raw()) }).map_err(shut)?;
    let adapter = ffi::AdapterInfo {
        base: Base::new(ffi::ADAPTER_INFO, 1),
        device_luid: (&raw const luid).cast(),
        device_luid_size: size_of_val(&luid) as u32,
        vk_physical_device: null_mut(),
    };
    // SAFETY: the adapter description lives through the call.
    check("DLSS on this adapter (slIsFeatureSupported)", unsafe { (api.is_feature_supported)(ffi::FEATURE_DLSS, &adapter) })
        .map_err(shut)?;
    let function = |feature: u32, name: &CStr| {
        let mut f = null_mut();
        // SAFETY: a NUL-terminated name and a slot for the address.
        check(&name.to_string_lossy(), unsafe { (api.get_feature_function)(feature, name.as_ptr(), &mut f) })?;
        if f.is_null() { Err(format!("no {}", name.to_string_lossy())) } else { Ok(f.cast_const()) }
    };
    // SAFETY: the DLSS plugin's functions, with the signatures of sl_dlss.h.
    let dlss = unsafe {
        use std::mem::transmute;
        DlssFunctions {
            get_optimal_settings: transmute::<*const c_void, ffi::DlssGetOptimalSettings>(
                function(ffi::FEATURE_DLSS, c"slDLSSGetOptimalSettings").map_err(shut)?,
            ),
            set_options: transmute::<*const c_void, ffi::DlssSetOptions>(
                function(ffi::FEATURE_DLSS, c"slDLSSSetOptions").map_err(shut)?,
            ),
        }
    };
    // Frame generation is extra: without it super resolution still serves.
    let generation = if generate {
        // SAFETY: the adapter description lives through the calls.
        generate::load(|feature| unsafe { (api.is_feature_supported)(feature, &adapter) }, find, function)
            .inspect_err(|why| tracing::warn!("DLSS frame generation: {why}; off"))
            .ok()
    } else {
        None
    };
    let mut version = ffi::FeatureVersion { base: Base::new(ffi::FEATURE_VERSION, 1), sl: ffi::Version::default(), ngx: ffi::Version::default() };
    // SAFETY: a version structure of ours to fill.
    unsafe { (api.get_feature_version)(ffi::FEATURE_DLSS, &mut version) };
    let what = format!(
        "Streamline {}.{}.{} with DLSS {}.{}.{} from {} on {name}; mode {}; frame generation {}",
        version.sl.major,
        version.sl.minor,
        version.sl.build,
        version.ngx.major,
        version.ngx.minor,
        version.ngx.build,
        dir.display(),
        asked_mode().name(),
        if generation.is_some() { "on" } else { "off" },
    );
    let _ = RUNTIME.set(Runtime { api, dlss, generation, calls: Mutex::new(Calls::default()), _signed: signed });
    Ok(what)
}

fn resource(texture: Texture<'_>) -> Resource {
    // SAFETY: a plain query of a live resource.
    let desc = unsafe { texture.0.GetDesc() };
    Resource {
        base: Base::new(ffi::RESOURCE, 1),
        kind: ffi::RESOURCE_TEX2D,
        native: texture.0.as_raw(),
        memory: null_mut(),
        view: null_mut(),
        state: texture.1,
        width: desc.Width as u32,
        height: desc.Height,
        native_format: desc.Format.0 as u32,
        mip_levels: u32::from(desc.MipLevels),
        array_layers: u32::from(desc.DepthOrArraySize),
        gpu_virtual_address: 0,
        flags: 0,
        usage: 0,
        reserved: 0,
    }
}

fn tag(resource: &mut Resource, kind: u32, size: [u32; 2]) -> ResourceTag {
    ResourceTag {
        base: Base::new(ffi::RESOURCE_TAG, 1),
        resource,
        kind,
        // Ours until the evaluate has recorded its work: the title reuses
        // them later in the frame.
        lifecycle: ffi::VALID_UNTIL_EVALUATE,
        extent: Extent { top: 0, left: 0, width: size[0], height: size[1] },
    }
}

const IDENTITY: Matrix = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// A left-handed perspective projection (row vectors, as the runtime's
/// matrices are) and its inverse, without jitter. `far` infinite or not
/// given leaves an infinite far plane.
fn projection(fov_y: f32, aspect: f32, near: f32, far: f32, inverted: bool) -> (Matrix, Matrix) {
    let ys = 1.0 / (fov_y * 0.5).tan();
    let xs = ys / aspect;
    let infinite = !far.is_finite() || far > 1e30;
    // Depth = a + b / z.
    let (a, b) = match (inverted, infinite) {
        (false, false) => (far / (far - near), -near * far / (far - near)),
        (false, true) => (1.0, -near),
        (true, false) => (near / (near - far), -far * near / (near - far)),
        (true, true) => (0.0, near),
    };
    let to_clip = [[xs, 0.0, 0.0, 0.0], [0.0, ys, 0.0, 0.0], [0.0, 0.0, a, 1.0], [0.0, 0.0, b, 0.0]];
    let to_view = [[1.0 / xs, 0.0, 0.0, 0.0], [0.0, 1.0 / ys, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0 / b], [0.0, 0.0, 1.0, -a / b]];
    (to_clip, to_view)
}

/// The runtime's motion-vector scale (a vector times it is a part of the
/// frame, 1 its whole width or height) from the title's (a vector times it
/// is render pixels, at whichever size the vectors are stored).
fn mvec_scale(to_render_pixels: [f32; 2], render: [u32; 2]) -> [f32; 2] {
    [to_render_pixels[0] / render[0].max(1) as f32, to_render_pixels[1] / render[1].max(1) as f32]
}

/// The runtime's per-frame constants for an upscale. What the title's
/// upscaler interface does not carry is made up, and unused by super
/// resolution: the projection is rebuilt from the field of view and planes,
/// the motion between frames' clip spaces is the identity, the camera sits
/// at the origin looking down +Z.
fn constants(u: &Frame<'_>, jitter_sign: [f32; 2]) -> Constants {
    let sane = |v: f32, or: f32| if v.is_finite() && v > 0.0 { v } else { or };
    // The interface names the planes in either order for inverted depth.
    let (near, far) = (u.near.min(u.far), u.near.max(u.far));
    let near = sane(near, 0.1);
    let fov = if u.fov_y.is_finite() && u.fov_y > 0.01 && u.fov_y < 3.1 { u.fov_y } else { 1.0 };
    let aspect = u.render[0] as f32 / u.render[1].max(1) as f32;
    let (to_clip, to_view) = projection(fov, aspect, near, far, u.depth_inverted);
    Constants {
        base: Base::new(ffi::CONSTANTS, 2),
        camera_view_to_clip: to_clip,
        clip_to_camera_view: to_view,
        clip_to_lens_clip: IDENTITY,
        clip_to_prev_clip: IDENTITY,
        prev_clip_to_clip: IDENTITY,
        jitter_offset: [u.jitter[0] * jitter_sign[0], u.jitter[1] * jitter_sign[1]],
        mvec_scale: mvec_scale(u.motion_scale, u.render),
        camera_pinhole_offset: [0.0, 0.0],
        camera_pos: [0.0, 0.0, 0.0],
        camera_up: [0.0, 1.0, 0.0],
        camera_right: [1.0, 0.0, 0.0],
        camera_fwd: [0.0, 0.0, 1.0],
        camera_near: near,
        camera_far: if far.is_finite() && far < 1e30 { far } else { ffi::INVALID_FLOAT },
        camera_fov: fov,
        camera_aspect_ratio: aspect,
        motion_vectors_invalid_value: ffi::INVALID_FLOAT,
        depth_inverted: ffi::boolean(u.depth_inverted),
        // The title's upscaler interface wants whole motion: camera and
        // objects.
        camera_motion_included: ffi::TRUE,
        motion_vectors_3d: ffi::FALSE,
        reset: ffi::boolean(u.reset),
        orthographic_projection: ffi::FALSE,
        motion_vectors_dilated: ffi::FALSE,
        motion_vectors_jittered: ffi::boolean(u.motion_jittered),
        min_relative_linear_depth_object_separation: 40.0,
    }
}

/// `VICTORY_DLSS_JITTER=<x>,<y>` multiplies the jitter the title names
/// (a testing lever for its sign convention); 1,1 without it.
fn jitter_sign() -> [f32; 2] {
    static SIGN: OnceLock<[f32; 2]> = OnceLock::new();
    *SIGN.get_or_init(|| {
        std::env::var("VICTORY_DLSS_JITTER")
            .ok()
            .and_then(|t| {
                let (x, y) = t.split_once(',')?;
                Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
            })
            .unwrap_or([1.0, 1.0])
    })
}

/// DLSS super resolution as an upscaler backend.
pub struct Dlss;

impl Upscaler for Dlss {
    fn name(&self) -> &'static str {
        "NVIDIA DLSS"
    }

    fn upscale(&self, list: &ID3D12GraphicsCommandList, u: &Frame<'_>) -> Result<(), String> {
        let rt = RUNTIME.get().ok_or("DLSS is not loaded")?;
        let mut calls = rt.calls.lock().unwrap_or_else(|e| e.into_inner());
        let result = upscale_locked(rt, &mut calls, list, u);
        match &result {
            Ok(()) => calls.upscales += 1,
            Err(_) => calls.failures += 1,
        }
        result
    }
}

fn upscale_locked(rt: &Runtime, calls: &mut Calls, list: &ID3D12GraphicsCommandList, u: &Frame<'_>) -> Result<(), String> {
    if u.render.contains(&0) || u.output_size.contains(&0) {
        return Err("an empty frame".into());
    }
    // The mode asked for, unless the title renders at another scale than
    // its ratio (another render-scale patch): then the mode nearest it.
    let scale = u.render[1] as f32 / u.output_size[1] as f32;
    let asked = asked_mode();
    let mode = if (asked.dlss_scale() - scale).abs() <= 0.02 { asked } else { Quality::nearest(scale) };
    let options = DlssOptions {
        base: Base::new(ffi::DLSS_OPTIONS, 3),
        mode: dlss_mode(mode),
        output_width: u.output_size[0],
        output_height: u.output_size[1],
        sharpness: 0.0,
        pre_exposure: if u.pre_exposure.is_finite() && u.pre_exposure > 0.0 { u.pre_exposure } else { 1.0 },
        exposure_scale: 1.0,
        color_buffers_hdr: ffi::boolean(u.hdr),
        indicator_invert_axis_x: ffi::FALSE,
        indicator_invert_axis_y: ffi::FALSE,
        dlaa_preset: 0,
        quality_preset: 0,
        balanced_preset: 0,
        performance_preset: 0,
        ultra_performance_preset: 0,
        ultra_quality_preset: 0,
        use_auto_exposure: ffi::boolean(u.auto_exposure || u.exposure.is_none()),
        alpha_upscaling_enabled: ffi::FALSE,
    };
    let key = (u.render, u.output_size, mode);
    if calls.options != Some(key) {
        let mut optimal = DlssOptimalSettings {
            base: Base::new(ffi::DLSS_OPTIMAL_SETTINGS, 1),
            optimal_render_width: 0,
            optimal_render_height: 0,
            optimal_sharpness: 0.0,
            render_width_min: 0,
            render_height_min: 0,
            render_width_max: 0,
            render_height_max: 0,
        };
        // SAFETY: our structures, alive for the call.
        let code = unsafe { (rt.dlss.get_optimal_settings)(&options, &mut optimal) };
        let range = [[optimal.render_width_min, optimal.render_height_min], [optimal.render_width_max, optimal.render_height_max]];
        let inside = code == ffi::OK
            && (range[0][0]..=range[1][0]).contains(&u.render[0])
            && (range[0][1]..=range[1][1]).contains(&u.render[1]);
        tracing::info!(
            mode = mode.name(),
            asked = asked.name(),
            render = ?u.render,
            output = ?u.output_size,
            optimal = ?[optimal.optimal_render_width, optimal.optimal_render_height],
            ?range,
            inside,
            result = ffi::result_name(code),
            "DLSS: options for these sizes"
        );
        if !inside && code == ffi::OK && range[1][0] != 0 {
            tracing::warn!(render = ?u.render, ?range, "DLSS: the title's render size is outside what this mode takes");
        }
        calls.options = Some(key);
    }
    // Every frame: the pre-exposure is the frame's.
    // SAFETY: as above.
    check("slDLSSSetOptions", unsafe { (rt.dlss.set_options)(&VIEWPORT, &options) })?;

    let mut token = null_mut();
    // SAFETY: a slot for the runtime's token; its own frame counter.
    check("slGetNewFrameToken", unsafe { (rt.api.get_new_frame_token)(&mut token, null()) })?;
    let constants = constants(u, jitter_sign());
    // SAFETY: our constants for the token just given.
    check("slSetConstants", unsafe { (rt.api.set_constants)(&constants, token, &VIEWPORT) })?;

    let motion_size = if u.motion_at_output { u.output_size } else { u.render };
    let mut color = resource(u.color);
    let mut depth = resource(u.depth);
    let mut motion = resource(u.motion);
    let mut output = resource(u.output);
    let mut exposure = u.exposure.filter(|_| !u.auto_exposure).map(resource);
    let tags = [
        Some(tag(&mut color, ffi::BUFFER_SCALING_INPUT_COLOR, u.render)),
        Some(tag(&mut depth, ffi::BUFFER_DEPTH, u.render)),
        Some(tag(&mut motion, ffi::BUFFER_MOTION_VECTORS, motion_size)),
        Some(tag(&mut output, ffi::BUFFER_SCALING_OUTPUT_COLOR, u.output_size)),
        exposure.as_mut().map(|e| tag(e, ffi::BUFFER_EXPOSURE, [1, 1])),
    ];
    // The viewport and this evaluate's own tags.
    let mut inputs: Vec<*const Base> = vec![&VIEWPORT.base];
    inputs.extend(tags.iter().flatten().map(|t| &raw const t.base));
    // SAFETY: the inputs above live through the call; the list is open and
    // the resources are in the states named.
    let code = unsafe { (rt.api.evaluate_feature)(ffi::FEATURE_DLSS, token, inputs.as_ptr(), inputs.len() as u32, list.as_raw()) };
    if calls.upscales == 0 || (code != ffi::OK && calls.failures < 4) {
        tracing::info!(
            result = ffi::result_name(code),
            mode = mode.name(),
            render = ?u.render,
            output = ?u.output_size,
            jitter = ?constants.jitter_offset,
            mvec_scale = ?constants.mvec_scale,
            motion_at_output = u.motion_at_output,
            motion_jittered = u.motion_jittered,
            depth_inverted = u.depth_inverted,
            hdr = u.hdr,
            auto_exposure = options.use_auto_exposure != 0,
            pre_exposure = options.pre_exposure,
            planes = ?[u.near, u.far],
            fov_y = u.fov_y,
            formats = ?[color.native_format, depth.native_format, motion.native_format, output.native_format],
            sizes = ?[[color.width, color.height], [depth.width, depth.height], [motion.width, motion.height], [output.width, output.height]],
            states = ?[color.state, depth.state, motion.state, output.state],
            "DLSS: an upscale recorded (slEvaluateFeature)"
        );
    }
    check("slEvaluateFeature", code)?;
    generate::after_upscale(rt, calls, list, token, &mut depth, &mut motion, [u.render, motion_size]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(a: &Matrix, b: &Matrix) -> Matrix {
        let mut out = [[0.0; 4]; 4];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = (0..4).map(|k| a[r][k] * b[k][c]).sum();
            }
        }
        out
    }

    #[test]
    fn a_projection_and_its_inverse_cancel() {
        for (inverted, far) in [(false, 1000.0), (true, 1000.0), (true, f32::INFINITY), (false, f32::MAX)] {
            let (to_clip, to_view) = projection(1.0, 16.0 / 9.0, 0.1, far, inverted);
            let one = times(&to_clip, &to_view);
            for (r, row) in one.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    assert!((v - IDENTITY[r][c]).abs() < 1e-4, "{inverted} {far}: {one:?}");
                }
            }
            // A point on the near plane has depth 1 when inverted, else 0.
            let depth = (to_clip[2][2] * 0.1 + to_clip[3][2]) / 0.1;
            assert!((depth - if inverted { 1.0 } else { 0.0 }).abs() < 1e-4);
        }
    }

    #[test]
    fn motion_in_render_pixels_becomes_parts_of_the_frame() {
        // Vectors in UV (the title's scale is the render size) are already
        // parts of the frame; vectors in pixels are divided by the size.
        assert_eq!(mvec_scale([1920.0, -1080.0], [1920, 1080]), [1.0, -1.0]);
        let pixels = mvec_scale([1.0, 1.0], [1920, 1080]);
        assert!((pixels[1] - 1.0 / 1080.0).abs() < 1e-9);
    }
}
