//! DLSS ray reconstruction (`sl.dlss_d`) and DLSS NR (`sl.dlss_nr`), as far
//! as they can honestly go here (`VICTORY_DLSS_NR=1`): the plugins are
//! loaded and asked whether they run on this adapter, what they require and
//! in which version, and that is logged. Neither is evaluated.
//!
//! Ray reconstruction replaces super resolution and the title's denoisers
//! in one pass. Its public interface (sl_dlss_d.h, bound in `ffi`) wants,
//! beside what super resolution gets, the noisy ray-traced colour and the
//! title's G-buffer at render size: diffuse albedo, specular albedo,
//! shading normals, linear roughness, and specular motion vectors or
//! specular hit distance, with the world-to-view matrix. None of those is
//! at the title's upscaler interface, the title's G-buffer layout is not
//! mapped, and its ray-traced passes do not run on the host yet: there is
//! nothing correct to tag.
//!
//! DLSS NR's interface is not in the SDK's public headers (no
//! `sl_dlss_nr.h` at any published tag; only its feature id and its three
//! buffer types, `kBufferTypeUpliftInputColor`, `...OutputColor` and
//! `...ControlMask`, in sl_core_types.h of v2.14.1). Its options structure
//! is therefore not bound, and `slDLSSNRSetOptions` is found but never
//! called: a layout guessed from the DLL would be exactly that.
//!
//! Which model runs is a matter of which file is in the folder: the
//! runtime gives NGX the folder its plugins were found in and nothing else
//! (a second plugin path is searched for plugins, not for models: tried),
//! so a newer `nvngx_dlssnr.dll` is used by pointing `VICTORY_DLSS_DIR` at
//! a Streamline folder that holds it. The log line below names the model
//! version that was loaded.

use std::ffi::{CStr, c_void};

use super::ffi::{self, Base};

pub(super) const RAY_RECONSTRUCTION: (u32, [&str; 2]) = (ffi::FEATURE_DLSS_RR, ["sl.dlss_d.dll", "nvngx_dlssd.dll"]);
pub(super) const NEURAL: (u32, [&str; 2]) = (ffi::FEATURE_DLSS_NR, ["sl.dlss_nr.dll", "nvngx_dlssnr.dll"]);
/// Whether `VICTORY_DLSS_NR` asks for the two features to be looked at.
pub(super) fn asked() -> bool {
    std::env::var_os("VICTORY_DLSS_NR").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The name of a buffer type a feature requires (sl_core_types.h).
fn buffer_name(kind: u32) -> String {
    let name = match kind {
        0 => "depth",
        1 => "motion vectors",
        2 => "HUD-less colour",
        3 => "scaling input colour",
        4 => "scaling output colour",
        5 => "normals",
        6 => "roughness",
        7 => "diffuse albedo",
        8 => "specular albedo",
        10 => "specular motion vectors",
        13 => "exposure",
        14 => "normals with roughness",
        23 => "interface colour and alpha",
        42 => "specular hit distance",
        49 => "linear depth",
        53 => "back buffer",
        69 => "interface alpha",
        70 => "uplift input colour",
        71 => "uplift output colour",
        72 => "uplift control mask",
        other => return format!("buffer type {other}"),
    };
    name.to_owned()
}

/// Asks the runtime about each of `features` that was loaded, and logs it.
pub(super) fn report(
    features: &[u32],
    supported: impl Fn(u32) -> i32,
    version: impl Fn(u32) -> ffi::FeatureVersion,
    find: impl Fn(&CStr) -> Result<*const c_void, String>,
    function: impl Fn(u32, &CStr) -> Result<*const c_void, String>,
) {
    // SAFETY: the interposer's export, with the signature of sl_core_api.h.
    let requirements = find(c"slGetFeatureRequirements")
        .ok()
        .map(|f| unsafe { std::mem::transmute::<*const c_void, ffi::GetFeatureRequirements>(f) });
    for &feature in features {
        let name = if feature == ffi::FEATURE_DLSS_RR { "DLSS ray reconstruction (sl.dlss_d)" } else { "DLSS NR (sl.dlss_nr)" };
        let code = supported(feature);
        let v = version(feature);
        // SAFETY: zeroes are this structure's empty values; the header is ours.
        let mut r: ffi::FeatureRequirements = unsafe { std::mem::zeroed() };
        r.base = Base::new(ffi::FEATURE_REQUIREMENTS, 2);
        // SAFETY: our structure to fill.
        let asked = requirements.map(|f| unsafe { f(feature, &mut r) });
        let tags: Vec<String> = if r.required_tags.is_null() {
            Vec::new()
        } else {
            // SAFETY: the runtime's list of that many buffer types.
            unsafe { std::slice::from_raw_parts(r.required_tags, r.num_required_tags as usize) }.iter().map(|&t| buffer_name(t)).collect()
        };
        let functions: Vec<String> = if feature == ffi::FEATURE_DLSS_RR {
            vec![c"slDLSSDSetOptions", c"slDLSSDGetOptimalSettings", c"slDLSSDGetState"]
        } else {
            vec![c"slDLSSNRSetOptions"]
        }
        .into_iter()
        .map(|f| format!("{}: {}", f.to_string_lossy(), if function(feature, f).is_ok() { "found" } else { "not found" }))
        .collect();
        tracing::info!(
            supported = ffi::result_name(code),
            streamline = format_args!("{}.{}.{}", v.sl.major, v.sl.minor, v.sl.build),
            model = format_args!("{}.{}.{}", v.ngx.major, v.ngx.minor, v.ngx.build),
            requirements = asked.map_or("not asked", ffi::result_name),
            required_tags = ?tags,
            driver = format_args!(
                "{}.{} (needs {}.{})",
                r.driver_version_detected.major, r.driver_version_detected.minor, r.driver_version_required.major, r.driver_version_required.minor
            ),
            viewports = r.max_num_viewports,
            ?functions,
            "{name}: looked at, not evaluated"
        );
        if feature == ffi::FEATURE_DLSS_RR && code == ffi::OK {
            optimal(&function);
        }
    }
    tracing::warn!(
        "VICTORY_DLSS_NR: neither feature runs. Ray reconstruction needs the title's noisy ray-traced colour, diffuse and specular albedo, normals, roughness and specular motion vectors or hit distance, none of which the host has; DLSS NR's options are not in the SDK's public headers"
    );
}

/// Ray reconstruction's render size for a 4K output in quality mode: a
/// query through its bound options, which changes nothing.
fn optimal(function: &impl Fn(u32, &CStr) -> Result<*const c_void, String>) {
    let Ok(f) = function(ffi::FEATURE_DLSS_RR, c"slDLSSDGetOptimalSettings") else { return };
    // SAFETY: the plugin's function, with the signature of sl_dlss_d.h.
    let f = unsafe { std::mem::transmute::<*const c_void, ffi::DlssdGetOptimalSettings>(f) };
    let options = ffi::DlssdOptions::new(ffi::DLSS_MAX_QUALITY, [3840, 2160]);
    let mut settings = ffi::DlssOptimalSettings {
        base: Base::new(ffi::DLSSD_OPTIMAL_SETTINGS, 1),
        optimal_render_width: 0,
        optimal_render_height: 0,
        optimal_sharpness: 0.0,
        render_width_min: 0,
        render_height_min: 0,
        render_width_max: 0,
        render_height_max: 0,
    };
    // SAFETY: our structures, alive for the call.
    let code = unsafe { f(&options, &mut settings) };
    tracing::info!(
        result = ffi::result_name(code),
        render = ?[settings.optimal_render_width, settings.optimal_render_height],
        "DLSS ray reconstruction: its render size for 3840x2160 in quality mode (slDLSSDGetOptimalSettings)"
    );
}
