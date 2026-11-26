//! NVIDIA Streamline's C++ interface as its runtime DLLs take it, written
//! out from the SDK's public headers (MIT; github.com/NVIDIA-RTX/Streamline,
//! tag v2.12.0: `sl_struct.h`, `sl_consts.h`, `sl_core_types.h`,
//! `sl_core_api.h`, `sl_result.h`, `sl_version.h`, `sl_appidentity.h`,
//! `sl_device_wrappers.h`, `sl_dlss.h`). The runtime the user supplies is
//! 2.13; a host built against an older SDK is what its versioned structures
//! are for (members are only ever added at the end, with the version raised).
//!
//! Every structure starts with [`Base`]: the next one of a chain, the
//! structure's GUID and its version. References are pointers; `sl::Result`
//! is an `int`; `sl::Boolean` a `char`.
#![allow(dead_code)]

use std::ffi::{c_char, c_void};
use std::ptr::null_mut;

/// `sl::StructType`: a GUID.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

/// `sl::BaseStructure`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Base {
    pub next: *mut Base,
    pub kind: Guid,
    pub version: usize,
}

impl Base {
    pub(crate) const fn new(kind: Guid, version: usize) -> Self {
        Self { next: null_mut(), kind, version }
    }
}

/// `SL_VERSION_MAJOR.MINOR.PATCH` of the headers above, as `kSDKVersion`.
pub(crate) const SDK_VERSION: u64 = (2 << 48) | (12 << 32) | 0xfedc;

/// `sl::Result::eOk`.
pub(crate) const OK: i32 = 0;

/// The name of an `sl::Result` (sl_result.h, in order).
pub(crate) fn result_name(code: i32) -> &'static str {
    const NAMES: [&str; 40] = [
        "eOk",
        "eErrorIO",
        "eErrorDriverOutOfDate",
        "eErrorOSOutOfDate",
        "eErrorOSDisabledHWS",
        "eErrorDeviceNotCreated",
        "eErrorNoSupportedAdapterFound",
        "eErrorAdapterNotSupported",
        "eErrorNoPlugins",
        "eErrorVulkanAPI",
        "eErrorDXGIAPI",
        "eErrorD3DAPI",
        "eErrorNRDAPI",
        "eErrorNVAPI",
        "eErrorReflexAPI",
        "eErrorNGXFailed",
        "eErrorJSONParsing",
        "eErrorMissingProxy",
        "eErrorMissingResourceState",
        "eErrorInvalidIntegration",
        "eErrorMissingInputParameter",
        "eErrorNotInitialized",
        "eErrorComputeFailed",
        "eErrorInitNotCalled",
        "eErrorExceptionHandler",
        "eErrorInvalidParameter",
        "eErrorMissingConstants",
        "eErrorDuplicatedConstants",
        "eErrorMissingOrInvalidAPI",
        "eErrorCommonConstantsMissing",
        "eErrorUnsupportedInterface",
        "eErrorFeatureMissing",
        "eErrorFeatureNotSupported",
        "eErrorFeatureMissingHooks",
        "eErrorFeatureFailedToLoad",
        "eErrorFeatureWrongPriority",
        "eErrorFeatureMissingDependency",
        "eErrorFeatureManagerInvalidState",
        "eErrorInvalidState",
        "eWarnOutOfVRAM",
    ];
    usize::try_from(code).ok().and_then(|i| NAMES.get(i)).copied().unwrap_or("an unknown result")
}

// `sl::Feature`.
pub(crate) const FEATURE_DLSS: u32 = 0;
pub(crate) const FEATURE_REFLEX: u32 = 3;
pub(crate) const FEATURE_PCL: u32 = 4;
pub(crate) const FEATURE_DLSS_G: u32 = 1000;
/// Ray reconstruction (`sl.dlss_d`).
pub(crate) const FEATURE_DLSS_RR: u32 = 1001;
/// `sl.dlss_nr` (sl_core_types.h of v2.14.1; not in v2.12.0's).
pub(crate) const FEATURE_DLSS_NR: u32 = 1004;

// `sl::BufferType`.
pub(crate) const BUFFER_DEPTH: u32 = 0;
pub(crate) const BUFFER_MOTION_VECTORS: u32 = 1;
pub(crate) const BUFFER_HUDLESS_COLOR: u32 = 2;
pub(crate) const BUFFER_SCALING_INPUT_COLOR: u32 = 3;
pub(crate) const BUFFER_SCALING_OUTPUT_COLOR: u32 = 4;
pub(crate) const BUFFER_EXPOSURE: u32 = 13;
pub(crate) const BUFFER_UI_COLOR_AND_ALPHA: u32 = 23;
pub(crate) const BUFFER_BIAS_CURRENT_COLOR_HINT: u32 = 29;

// `sl::PreferenceFlags`.
pub(crate) const DISABLE_CL_STATE_TRACKING: u64 = 1 << 0;
pub(crate) const USE_MANUAL_HOOKING: u64 = 1 << 2;
/// Not set here: updates of plugins and models over the network.
pub(crate) const ALLOW_OTA: u64 = 1 << 3;
/// Not set here: loading what those updates downloaded.
pub(crate) const LOAD_DOWNLOADED_PLUGINS: u64 = 1 << 6;
/// The DXGI factory we upgrade becomes a proxy object; without it the
/// runtime rewrites the factory class's own table.
pub(crate) const USE_DXGI_FACTORY_PROXY: u64 = 1 << 5;
pub(crate) const USE_FRAME_BASED_RESOURCE_TAGGING: u64 = 1 << 7;

// `sl::LogLevel`.
pub(crate) const LOG_OFF: u32 = 0;
pub(crate) const LOG_DEFAULT: u32 = 1;
pub(crate) const LOG_VERBOSE: u32 = 2;

/// `sl::EngineType::eCustom`.
pub(crate) const ENGINE_CUSTOM: u32 = 0;
/// `sl::RenderAPI::eD3D12`.
pub(crate) const RENDER_D3D12: u32 = 1;

// `sl::Boolean`.
pub(crate) const FALSE: i8 = 0;
pub(crate) const TRUE: i8 = 1;
pub(crate) const fn boolean(v: bool) -> i8 {
    v as i8
}

/// `sl::INVALID_FLOAT`: a value not given.
pub(crate) const INVALID_FLOAT: f32 = f32::MAX;

/// `sl::ResourceType::eTex2d`.
pub(crate) const RESOURCE_TEX2D: i8 = 0;
// `sl::ResourceLifecycle`.
pub(crate) const ONLY_VALID_NOW: u32 = 0;
pub(crate) const VALID_UNTIL_PRESENT: u32 = 1;
pub(crate) const VALID_UNTIL_EVALUATE: u32 = 2;

/// `void(sl::LogType, const char*)`: info 0, warning 1, error 2.
pub(crate) type LogCallback = unsafe extern "C" fn(u32, *const c_char);

/// `sl::Preferences`, version 1.
#[repr(C)]
pub(crate) struct Preferences {
    pub base: Base,
    pub show_console: bool,
    pub log_level: u32,
    pub paths_to_plugins: *const *const u16,
    pub num_paths_to_plugins: u32,
    pub path_to_logs_and_data: *const u16,
    pub allocate_callback: *const c_void,
    pub release_callback: *const c_void,
    pub log_message_callback: Option<LogCallback>,
    pub flags: u64,
    pub features_to_load: *const u32,
    pub num_features_to_load: u32,
    pub application_id: u32,
    pub engine: u32,
    pub engine_version: *const c_char,
    pub project_id: *const c_char,
    pub render_api: u32,
}

pub(crate) const PREFERENCES: Guid = Guid(0x1ca1_0965, 0xbf8e, 0x432b, [0x8d, 0xa1, 0x67, 0x16, 0xd8, 0x79, 0xfb, 0x14]);

/// `sl::AdapterInfo`, version 1.
#[repr(C)]
pub(crate) struct AdapterInfo {
    pub base: Base,
    pub device_luid: *const u8,
    pub device_luid_size: u32,
    pub vk_physical_device: *mut c_void,
}

pub(crate) const ADAPTER_INFO: Guid = Guid(0x0677_315f, 0xa746, 0x4492, [0x9f, 0x42, 0xcb, 0x61, 0x42, 0xc9, 0xc3, 0xd4]);

/// `sl::ViewportHandle`, version 1.
#[repr(C)]
pub(crate) struct ViewportHandle {
    pub base: Base,
    pub value: u32,
}

pub(crate) const VIEWPORT_HANDLE: Guid = Guid(0x171b_6435, 0x9b3c, 0x4fc8, [0x99, 0x94, 0xfb, 0xe5, 0x25, 0x69, 0xaa, 0xa4]);

impl ViewportHandle {
    pub(crate) const fn new(value: u32) -> Self {
        Self { base: Base::new(VIEWPORT_HANDLE, 1), value }
    }
}

/// `sl::FrameToken`: the runtime's, only ever passed back to it.
#[repr(C)]
pub(crate) struct FrameToken {
    _opaque: [u8; 0],
}

/// `sl::Resource`, version 1.
#[repr(C)]
pub(crate) struct Resource {
    pub base: Base,
    pub kind: i8,
    pub native: *mut c_void,
    pub memory: *mut c_void,
    pub view: *mut c_void,
    /// The `D3D12_RESOURCE_STATES` it is in when the runtime uses it.
    pub state: u32,
    pub width: u32,
    pub height: u32,
    pub native_format: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub gpu_virtual_address: u64,
    pub flags: u32,
    pub usage: u32,
    pub reserved: u32,
}

pub(crate) const RESOURCE: Guid = Guid(0x3a9d_70cf, 0x2418, 0x4b72, [0x83, 0x91, 0x13, 0xf8, 0x72, 0x1c, 0x72, 0x61]);

/// `sl::Extent`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Extent {
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

/// `sl::ResourceTag`, version 1.
#[repr(C)]
pub(crate) struct ResourceTag {
    pub base: Base,
    pub resource: *mut Resource,
    pub kind: u32,
    pub lifecycle: u32,
    pub extent: Extent,
}

pub(crate) const RESOURCE_TAG: Guid = Guid(0x4c6a_5aad, 0xb445, 0x496c, [0x87, 0xff, 0x1a, 0xf3, 0x84, 0x5b, 0xe6, 0x53]);

/// `sl::float4x4`: row major.
pub(crate) type Matrix = [[f32; 4]; 4];

/// `sl::Constants`, version 2.
#[repr(C)]
pub(crate) struct Constants {
    pub base: Base,
    pub camera_view_to_clip: Matrix,
    pub clip_to_camera_view: Matrix,
    pub clip_to_lens_clip: Matrix,
    pub clip_to_prev_clip: Matrix,
    pub prev_clip_to_clip: Matrix,
    pub jitter_offset: [f32; 2],
    pub mvec_scale: [f32; 2],
    pub camera_pinhole_offset: [f32; 2],
    pub camera_pos: [f32; 3],
    pub camera_up: [f32; 3],
    pub camera_right: [f32; 3],
    pub camera_fwd: [f32; 3],
    pub camera_near: f32,
    pub camera_far: f32,
    pub camera_fov: f32,
    pub camera_aspect_ratio: f32,
    pub motion_vectors_invalid_value: f32,
    pub depth_inverted: i8,
    pub camera_motion_included: i8,
    pub motion_vectors_3d: i8,
    pub reset: i8,
    pub orthographic_projection: i8,
    pub motion_vectors_dilated: i8,
    pub motion_vectors_jittered: i8,
    pub min_relative_linear_depth_object_separation: f32,
}

pub(crate) const CONSTANTS: Guid = Guid(0xdcd3_5ad7, 0x4e4a, 0x4bad, [0xa9, 0x0c, 0xe0, 0xc4, 0x9e, 0xb2, 0x3a, 0xfe]);

// `sl::DLSSMode`.
pub(crate) const DLSS_OFF: u32 = 0;
pub(crate) const DLSS_MAX_PERFORMANCE: u32 = 1;
pub(crate) const DLSS_BALANCED: u32 = 2;
pub(crate) const DLSS_MAX_QUALITY: u32 = 3;
pub(crate) const DLSS_ULTRA_PERFORMANCE: u32 = 4;
pub(crate) const DLSS_DLAA: u32 = 6;

/// `sl::DLSSOptions`, version 3. The presets stay `eDefault` (0).
#[repr(C)]
pub(crate) struct DlssOptions {
    pub base: Base,
    pub mode: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub sharpness: f32,
    pub pre_exposure: f32,
    pub exposure_scale: f32,
    pub color_buffers_hdr: i8,
    pub indicator_invert_axis_x: i8,
    pub indicator_invert_axis_y: i8,
    pub dlaa_preset: u32,
    pub quality_preset: u32,
    pub balanced_preset: u32,
    pub performance_preset: u32,
    pub ultra_performance_preset: u32,
    pub ultra_quality_preset: u32,
    pub use_auto_exposure: i8,
    pub alpha_upscaling_enabled: i8,
}

pub(crate) const DLSS_OPTIONS: Guid = Guid(0x6ac8_26e4, 0x4c61, 0x4101, [0xa9, 0x2d, 0x63, 0x8d, 0x42, 0x10, 0x57, 0xb8]);

/// `sl::DLSSOptimalSettings`, version 1.
#[repr(C)]
pub(crate) struct DlssOptimalSettings {
    pub base: Base,
    pub optimal_render_width: u32,
    pub optimal_render_height: u32,
    pub optimal_sharpness: f32,
    pub render_width_min: u32,
    pub render_height_min: u32,
    pub render_width_max: u32,
    pub render_height_max: u32,
}

pub(crate) const DLSS_OPTIMAL_SETTINGS: Guid = Guid(0xef1d_0957, 0xfd58, 0x4df7, [0xb5, 0x04, 0x8b, 0x69, 0xd8, 0xaa, 0x6b, 0x76]);

/// `sl::Version`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Version {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

/// `sl::FeatureVersion`, version 1.
#[repr(C)]
pub(crate) struct FeatureVersion {
    pub base: Base,
    pub sl: Version,
    pub ngx: Version,
}

pub(crate) const FEATURE_VERSION: Guid = Guid(0x6d5b_51f0, 0x076b, 0x486d, [0x99, 0x95, 0x5a, 0x56, 0x10, 0x43, 0xf5, 0xc1]);

/// `sl::FeatureRequirements`, version 2.
#[repr(C)]
pub(crate) struct FeatureRequirements {
    pub base: Base,
    pub flags: u32,
    pub max_num_cpu_threads: u32,
    pub max_num_viewports: u32,
    pub num_required_tags: u32,
    pub required_tags: *const u32,
    pub os_version_detected: Version,
    pub os_version_required: Version,
    pub driver_version_detected: Version,
    pub driver_version_required: Version,
    pub vk_num_compute_queues_required: u32,
    pub vk_num_graphics_queues_required: u32,
    pub vk_num_device_extensions: u32,
    pub vk_device_extensions: *const *const c_char,
    pub vk_num_instance_extensions: u32,
    pub vk_instance_extensions: *const *const c_char,
    pub vk_num_features12: u32,
    pub vk_features12: *const *const c_char,
    pub vk_num_features13: u32,
    pub vk_features13: *const *const c_char,
    pub vk_num_optical_flow_queues_required: u32,
}

pub(crate) const FEATURE_REQUIREMENTS: Guid = Guid(0x6671_4097, 0xac6d, 0x4bc6, [0x89, 0x15, 0x1e, 0x0f, 0x55, 0xa6, 0xb6, 0x1f]);

// The layouts the 64-bit Microsoft compiler gives the headers' structures.
const _: () = {
    use std::mem::offset_of;

    assert!(size_of::<Base>() == 32);
    assert!(size_of::<Preferences>() == 144);
    assert!(offset_of!(Preferences, log_level) == 36);
    assert!(offset_of!(Preferences, flags) == 88);
    assert!(offset_of!(Preferences, application_id) == 108);
    assert!(offset_of!(Preferences, render_api) == 136);
    assert!(size_of::<AdapterInfo>() == 56);
    assert!(size_of::<ViewportHandle>() == 40);
    assert!(size_of::<Resource>() == 112);
    assert!(offset_of!(Resource, native) == 40);
    assert!(offset_of!(Resource, state) == 64);
    assert!(offset_of!(Resource, gpu_virtual_address) == 88);
    assert!(size_of::<ResourceTag>() == 64);
    assert!(offset_of!(ResourceTag, extent) == 48);
    assert!(size_of::<Constants>() == 456);
    assert!(offset_of!(Constants, jitter_offset) == 352);
    assert!(offset_of!(Constants, camera_near) == 424);
    assert!(offset_of!(Constants, depth_inverted) == 444);
    assert!(offset_of!(Constants, min_relative_linear_depth_object_separation) == 452);
    assert!(size_of::<DlssOptions>() == 88);
    assert!(offset_of!(DlssOptions, dlaa_preset) == 60);
    assert!(offset_of!(DlssOptions, use_auto_exposure) == 84);
    assert!(size_of::<DlssOptimalSettings>() == 64);
    assert!(size_of::<FeatureVersion>() == 56);
    assert!(size_of::<FeatureRequirements>() == 184);
    assert!(offset_of!(FeatureRequirements, driver_version_required) == 92);
};

// sl_core_api.h.
pub(crate) type Init = unsafe extern "C" fn(*const Preferences, u64) -> i32;
pub(crate) type Shutdown = unsafe extern "C" fn() -> i32;
pub(crate) type IsFeatureSupported = unsafe extern "C" fn(u32, *const AdapterInfo) -> i32;
pub(crate) type IsFeatureLoaded = unsafe extern "C" fn(u32, *mut bool) -> i32;
pub(crate) type SetD3dDevice = unsafe extern "C" fn(*mut c_void) -> i32;
pub(crate) type GetFeatureFunction = unsafe extern "C" fn(u32, *const c_char, *mut *mut c_void) -> i32;
pub(crate) type GetFeatureVersion = unsafe extern "C" fn(u32, *mut FeatureVersion) -> i32;
pub(crate) type GetFeatureRequirements = unsafe extern "C" fn(u32, *mut FeatureRequirements) -> i32;
pub(crate) type GetNewFrameToken = unsafe extern "C" fn(*mut *mut FrameToken, *const u32) -> i32;
pub(crate) type SetConstants = unsafe extern "C" fn(*const Constants, *const FrameToken, *const ViewportHandle) -> i32;
pub(crate) type SetTagForFrame =
    unsafe extern "C" fn(*const FrameToken, *const ViewportHandle, *const ResourceTag, u32, *mut c_void) -> i32;
pub(crate) type EvaluateFeature = unsafe extern "C" fn(u32, *const FrameToken, *const *const Base, u32, *mut c_void) -> i32;
pub(crate) type FreeResources = unsafe extern "C" fn(u32, *const ViewportHandle) -> i32;
pub(crate) type UpgradeInterface = unsafe extern "C" fn(*mut *mut c_void) -> i32;
pub(crate) type GetNativeInterface = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;

// sl_dlss.h, through `slGetFeatureFunction`.
pub(crate) type DlssGetOptimalSettings = unsafe extern "C" fn(*const DlssOptions, *mut DlssOptimalSettings) -> i32;
pub(crate) type DlssSetOptions = unsafe extern "C" fn(*const ViewportHandle, *const DlssOptions) -> i32;

// Frame generation: sl_dlss_g.h, sl_reflex.h and sl_pcl.h of the same tag.

// `sl::DLSSGMode`.
pub(crate) const DLSSG_OFF: u32 = 0;
pub(crate) const DLSSG_ON: u32 = 1;

// `sl::DLSSGStatus` (bits; 0 is fine).
pub(crate) const DLSSG_STATUS: [(u32, &str); 5] = [
    (1, "the resolution is too low"),
    (2, "Reflex was not detected at run time"),
    (4, "the HDR format is not supported"),
    (8, "the common constants are invalid"),
    (16, "GetCurrentBackBufferIndex was not called"),
];

/// `sl::DLSSGOptions`, version 5.
#[repr(C)]
pub(crate) struct DlssgOptions {
    pub base: Base,
    pub mode: u32,
    pub num_frames_to_generate: u32,
    pub flags: u32,
    pub dynamic_res_width: u32,
    pub dynamic_res_height: u32,
    pub num_back_buffers: u32,
    pub mvec_depth_width: u32,
    pub mvec_depth_height: u32,
    pub color_width: u32,
    pub color_height: u32,
    pub color_buffer_format: u32,
    pub mvec_buffer_format: u32,
    pub depth_buffer_format: u32,
    pub hud_less_buffer_format: u32,
    pub ui_buffer_format: u32,
    pub on_error_callback: *const c_void,
    pub reserved15: i8,
    pub queue_parallelism_mode: u32,
    pub enable_user_interface_recomposition: i8,
    pub dynamic_target_frame_rate: f32,
}

pub(crate) const DLSSG_OPTIONS: Guid = Guid(0xfac5_f1cb, 0x2dfd, 0x4f36, [0xa1, 0xe6, 0x3a, 0x9e, 0x86, 0x52, 0x56, 0xc5]);
/// `sl::Boolean::eInvalid`, the default of the options' reserved member.
pub(crate) const INVALID: i8 = 2;

/// `sl::DLSSGState`, version 4.
#[repr(C)]
pub(crate) struct DlssgState {
    pub base: Base,
    pub estimated_vram_usage_in_bytes: u64,
    pub status: u32,
    pub min_width_or_height: u32,
    pub num_frames_actually_presented: u32,
    pub num_frames_to_generate_max: u32,
    pub reserved4: i8,
    pub is_vsync_support_available: i8,
    pub inputs_processing_completion_fence: *mut c_void,
    pub last_present_inputs_processing_completion_fence_value: u64,
    pub is_dynamic_mfg_supported: i8,
}

pub(crate) const DLSSG_STATE: Guid = Guid(0xcc8a_c8e1, 0xa179, 0x44f5, [0x97, 0xfa, 0xe7, 0x41, 0x12, 0xf9, 0xbc, 0x61]);

/// `sl::ReflexMode::eLowLatency`.
pub(crate) const REFLEX_LOW_LATENCY: u32 = 1;

/// `sl::ReflexOptions`, version 1.
#[repr(C)]
pub(crate) struct ReflexOptions {
    pub base: Base,
    pub mode: u32,
    pub frame_limit_us: u32,
    pub use_markers_to_optimize: bool,
    pub virtual_key: u16,
    pub id_thread: u32,
}

pub(crate) const REFLEX_OPTIONS: Guid = Guid(0xf03a_f81a, 0x6d0b, 0x4902, [0xa6, 0x51, 0xc4, 0x96, 0x5e, 0x21, 0x54, 0x34]);

// `sl::PCLMarker`.
pub(crate) const MARKER_SIMULATION_START: u32 = 0;
pub(crate) const MARKER_SIMULATION_END: u32 = 1;
pub(crate) const MARKER_RENDER_SUBMIT_START: u32 = 2;
pub(crate) const MARKER_RENDER_SUBMIT_END: u32 = 3;
pub(crate) const MARKER_PRESENT_START: u32 = 4;
pub(crate) const MARKER_PRESENT_END: u32 = 5;

const _: () = {
    use std::mem::offset_of;

    assert!(size_of::<DlssgOptions>() == 120);
    assert!(offset_of!(DlssgOptions, on_error_callback) == 96);
    assert!(offset_of!(DlssgOptions, queue_parallelism_mode) == 108);
    assert!(offset_of!(DlssgOptions, dynamic_target_frame_rate) == 116);
    assert!(size_of::<DlssgState>() == 88);
    assert!(offset_of!(DlssgState, inputs_processing_completion_fence) == 64);
    assert!(offset_of!(DlssgState, is_dynamic_mfg_supported) == 80);
    assert!(size_of::<ReflexOptions>() == 48);
    assert!(offset_of!(ReflexOptions, virtual_key) == 42);
};

pub(crate) type DlssgSetOptions = unsafe extern "C" fn(*const ViewportHandle, *const DlssgOptions) -> i32;
pub(crate) type DlssgGetState = unsafe extern "C" fn(*const ViewportHandle, *mut DlssgState, *const DlssgOptions) -> i32;
pub(crate) type ReflexSetOptions = unsafe extern "C" fn(*const ReflexOptions) -> i32;
pub(crate) type ReflexSleep = unsafe extern "C" fn(*const FrameToken) -> i32;
pub(crate) type PclSetMarker = unsafe extern "C" fn(u32, *const FrameToken) -> i32;
