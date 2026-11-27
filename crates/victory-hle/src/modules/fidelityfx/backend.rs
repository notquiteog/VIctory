//! Another vendor's upscaler in place of the title's AMD one: the title's
//! FidelityFX calls are answered here, with no FidelityFX DLL behind them,
//! and each upscaling it dispatches is handed to a backend
//! (`victory_gpu::upscale::Upscaler`) as a vendor-neutral frame, to record
//! into the same host list. NVIDIA DLSS is the backend there is
//! (`VICTORY_UPSCALER=dlss` with `VICTORY_DLSS_DIR`, see
//! `victory_gpu::streamline`); AMD's own library needs none of this, since
//! it takes the title's calls as they are (the parent module).
//!
//! What the title's upscaler interface hands over each frame is what any
//! temporal upscaler wants: the jittered colour at render size, depth,
//! motion vectors, an optional exposure texture, the output texture, the
//! jitter in render pixels, the scale that makes a motion vector render
//! pixels, both sizes, the pre-exposure, the frame time and a reset flag;
//! the context's creation flags say whether the colour is HDR, the depth
//! inverted, the vectors at output size or still jittered, the exposure
//! automatic. Not carried over: the reactive and transparency masks (DLSS
//! has no input of the same meaning) and the interface's sharpening pass
//! (DLSS's is deprecated).
//!
//! The queries are answered as AMD's upscaler answers them, so the title
//! jitters and sizes its frame the same way: the jitter is a Halton(2, 3)
//! sequence of `8 * (output / render)^2` phases either way.

use std::ffi::{c_char, c_void};
use std::path::Path;
use std::sync::OnceLock;

use victory_gpu::object::Object;
use victory_gpu::streamline;
use victory_gpu::upscale::{Frame, Quality, Upscaler};

use super::{
    CREATE_UPSCALE, CreateUpscale, DISPATCH_UPSCALE, DispatchUpscale, GET_PROVIDER_VERSION, GET_VERSIONS, GetVersions,
    Header, NO_PROVIDER, OK, PARAMETER, ProviderVersion, UNKNOWN_TYPE, host_state,
};

// ffx_upscale.h: the upscaler's queries and settings.
const GET_UPSCALE_RATIO: u64 = 0x1_0002;
const GET_RENDER_RESOLUTION: u64 = 0x1_0003;
pub(super) const GET_JITTER_PHASE_COUNT: u64 = 0x1_0004;
const GET_JITTER_OFFSET: u64 = 0x1_0005;
const MEMORY_USAGE: u64 = 0x1_0008;
const MEMORY_USAGE_WITH_DEVICE: u64 = 0x1_0009;
/// `ffxCreateContextDescUpscaleVersion`.
const UPSCALE_VERSION: u64 = 0x1_000b;

// The context's creation flags.
const HDR: u32 = 1 << 0;
const MOTION_AT_OUTPUT_SIZE: u32 = 1 << 1;
const MOTION_JITTERED: u32 = 1 << 2;
const DEPTH_INVERTED: u32 = 1 << 3;
const AUTO_EXPOSURE: u32 = 1 << 5;
const NON_LINEAR_COLOR: u32 = 1 << 8;
/// A dispatch's flags: sRGB or PQ colour.
const DISPATCH_NON_LINEAR: u32 = 0b110;

const NAME: &std::ffi::CStr = c"VIctory host upscaler";
/// The provider id the title is told; no AMD provider has it.
const PROVIDER: u64 = 0x444c_5353;

#[repr(C)]
struct GetUpscaleRatio {
    header: Header,
    quality: u32,
    ratio: *mut f32,
}

#[repr(C)]
struct GetRenderResolution {
    header: Header,
    display: [u32; 2],
    quality: u32,
    width: *mut u32,
    height: *mut u32,
}

#[repr(C)]
struct GetJitterPhaseCount {
    header: Header,
    render_width: u32,
    display_width: u32,
    count: *mut i32,
}

#[repr(C)]
struct GetJitterOffset {
    header: Header,
    index: i32,
    phase_count: i32,
    x: *mut f32,
    y: *mut f32,
}

/// Total and aliasable bytes.
#[repr(C)]
struct MemoryUsage {
    header: Header,
    usage: *mut [u64; 2],
}

#[repr(C)]
struct MemoryUsageWithDevice {
    header: Header,
    device: *mut c_void,
    max_render: [u32; 2],
    max_output: [u32; 2],
    flags: u32,
    usage: *mut [u64; 2],
}

// FidelityFX SDK, upscalers/include/ffx_upscale.h.
const _: () = {
    use std::mem::offset_of;

    assert!(offset_of!(GetUpscaleRatio, ratio) == 0x18);
    assert!(offset_of!(GetRenderResolution, width) == 0x20);
    assert!(offset_of!(GetJitterPhaseCount, count) == 0x18);
    assert!(offset_of!(GetJitterOffset, x) == 0x18);
    assert!(offset_of!(MemoryUsageWithDevice, usage) == 0x30);
};

static BACKEND: OnceLock<&'static dyn Upscaler> = OnceLock::new();

/// Whether a backend answers the title's FidelityFX calls.
pub(super) fn serving() -> bool {
    BACKEND.get().is_some()
}

/// The backend's render scale for the quality mode asked for.
pub(super) fn render_scale() -> Option<f32> {
    serving().then(|| streamline::asked_mode().dlss_scale())
}

/// Readies DLSS from `dir`; true if it serves.
pub(super) fn use_dlss(dir: &Path) -> bool {
    match streamline::load(dir) {
        Ok(what) => {
            tracing::info!("DLSS: {what}; it serves the title's FidelityFX upscaler");
            let _ = BACKEND.set(&streamline::Dlss);
            true
        }
        Err(why) => {
            tracing::warn!(folder = %dir.display(), "DLSS: {why}; off");
            false
        }
    }
}

/// What a context of ours is: the title's creation description.
struct Context {
    magic: u64,
    flags: u32,
}

const MAGIC: u64 = 0x5649_4354_444c_5353;

/// The context behind the title's slot, if it is one of ours.
///
/// # Safety
/// `slot` is null or the title's context slot.
unsafe fn ours<'a>(slot: *mut *mut c_void) -> Option<&'a Context> {
    // SAFETY: the title's slot holds null or what `create` put there.
    let context = unsafe { (*slot.as_ref()?).cast::<Context>().as_ref()? };
    (context.magic == MAGIC).then_some(context)
}

pub(super) fn create(context: *mut *mut c_void, desc: *mut Header) -> u32 {
    // SAFETY: the title passes a chain of descriptions.
    let kind = unsafe { (*desc).kind };
    if kind != CREATE_UPSCALE {
        once!(warn, kind = format_args!("{kind:#x}"), "host upscaler: only the upscaler is served; another FidelityFX effect was refused");
        return NO_PROVIDER;
    }
    // SAFETY: an upscaler description, by its type.
    let upscale = unsafe { desc.cast::<CreateUpscale>().read() };
    let mut chain = Vec::new();
    let mut next = upscale.header.next;
    // SAFETY: the title's chain.
    while let Some(h) = unsafe { next.as_ref() } {
        if h.kind == UPSCALE_VERSION {
            // SAFETY: a version description, by its type: the header, then
            // the upscaler interface version the title was built against.
            let v = unsafe { std::ptr::from_ref(h).add(1).cast::<u32>().read() };
            chain.push(format!("upscaler interface {}.{}.{}", v >> 22, (v >> 12) & 0x3ff, v & 0xfff));
        } else {
            chain.push(format!("{:#x}", h.kind));
        }
        next = h.next;
    }
    tracing::info!(
        render = ?upscale.max_render,
        output = ?upscale.max_output,
        flags = format_args!("{:#x}", upscale.flags),
        hdr = upscale.flags & HDR != 0,
        motion_at_output_size = upscale.flags & MOTION_AT_OUTPUT_SIZE != 0,
        motion_jittered = upscale.flags & MOTION_JITTERED != 0,
        depth_inverted = upscale.flags & DEPTH_INVERTED != 0,
        auto_exposure = upscale.flags & AUTO_EXPOSURE != 0,
        non_linear = upscale.flags & NON_LINEAR_COLOR != 0,
        ?chain,
        "host upscaler: the title's upscaler context"
    );
    let ours = Box::new(Context { magic: MAGIC, flags: upscale.flags });
    // SAFETY: the title's slot.
    unsafe { *context = Box::into_raw(ours).cast() };
    OK
}

pub(super) fn destroy(context: *mut *mut c_void) -> u32 {
    // SAFETY: the title passes its context slot.
    if unsafe { ours(context) }.is_none() {
        return PARAMETER;
    }
    // SAFETY: ours, made by `create`; the slot is cleared with it. The
    // runtime's own resources stay for the next context.
    unsafe {
        drop(Box::from_raw((*context).cast::<Context>()));
        *context = std::ptr::null_mut();
    }
    OK
}

/// AMD's upscaler's ratio for one of its quality modes: output over
/// render, per axis.
fn ratio(quality: u32) -> Option<f32> {
    Quality::ALL.get(quality as usize).map(|q| q.fsr_ratio())
}

/// The jitter sequence's length for a render and an output width.
fn jitter_phase_count(render_width: u32, display_width: u32) -> i32 {
    let ratio = display_width as f32 / render_width.max(1) as f32;
    ((8.0 * ratio * ratio) as i32).max(1)
}

/// After the title's own upscaler answered a jitter phase count query
/// (`code`): a count of 1 or more is left in the title's slot. The shipping
/// upscaler writes nothing for sizes it has no tables for, or without a
/// context; the title keeps the answer as its jitter ring's length and takes
/// the frame index modulo it, a division by zero (seen by Kemerd's fork on a
/// player's run). Its own answer is kept whenever it gave one.
pub(super) fn answer_phase_count(desc: *mut Header, code: u32) -> u32 {
    // SAFETY: a phase count query, by its kind; the title passed it.
    let q = unsafe { &*desc.cast::<GetJitterPhaseCount>() };
    // SAFETY: the title's answer slot for this call, if any.
    let Some(out) = (unsafe { q.count.as_mut() }) else { return code };
    if code == OK && *out > 0 {
        return code;
    }
    let ours = jitter_phase_count(q.render_width, q.display_width);
    once!(
        warn,
        code,
        given = *out,
        ours,
        "FidelityFX: the title's upscaler gave no jitter phase count; answered with FidelityFX's own formula"
    );
    *out = ours;
    OK
}

fn halton(mut index: i32, base: i32) -> f32 {
    let (mut f, mut result) = (1.0f32, 0.0f32);
    while index > 0 {
        f /= base as f32;
        result += f * (index % base) as f32;
        index /= base;
    }
    result
}

/// The sub-pixel offset of frame `index` in a sequence of `phase_count`.
fn jitter_offset(index: i32, phase_count: i32) -> [f32; 2] {
    let i = index.rem_euclid(phase_count.max(1)) + 1;
    [halton(i, 2) - 0.5, halton(i, 3) - 0.5]
}

pub(super) fn query(_context: *mut *mut c_void, desc: *mut Header) -> u32 {
    // SAFETY: the title passes a description; each is read by its type, and
    // its output pointers are the title's.
    unsafe {
        let kind = (*desc).kind;
        match kind {
            GET_PROVIDER_VERSION => {
                let q = &mut *desc.cast::<ProviderVersion>();
                (q.id, q.name) = (PROVIDER, NAME.as_ptr());
            }
            GET_VERSIONS => {
                let q = &mut *desc.cast::<GetVersions>();
                let Some(count) = q.count.as_mut() else { return PARAMETER };
                let room = *count;
                *count = 1;
                if room >= 1 {
                    if let Some(id) = q.ids.as_mut() {
                        *id = PROVIDER;
                    }
                    if let Some(name) = q.names.as_mut() {
                        *name = NAME.as_ptr().cast::<c_char>();
                    }
                }
            }
            GET_UPSCALE_RATIO => {
                let q = &*desc.cast::<GetUpscaleRatio>();
                let (Some(r), Some(out)) = (ratio(q.quality), q.ratio.as_mut()) else { return PARAMETER };
                *out = r;
            }
            GET_RENDER_RESOLUTION => {
                let q = &*desc.cast::<GetRenderResolution>();
                let (Some(r), Some(w), Some(h)) = (ratio(q.quality), q.width.as_mut(), q.height.as_mut()) else {
                    return PARAMETER;
                };
                (*w, *h) = ((q.display[0] as f32 / r) as u32, (q.display[1] as f32 / r) as u32);
            }
            GET_JITTER_PHASE_COUNT => {
                let q = &*desc.cast::<GetJitterPhaseCount>();
                let Some(out) = q.count.as_mut() else { return PARAMETER };
                *out = jitter_phase_count(q.render_width, q.display_width);
            }
            GET_JITTER_OFFSET => {
                let q = &*desc.cast::<GetJitterOffset>();
                let (Some(x), Some(y)) = (q.x.as_mut(), q.y.as_mut()) else { return PARAMETER };
                [*x, *y] = jitter_offset(q.index, q.phase_count);
            }
            // What DLSS holds is the runtime's to know; nothing of it is
            // in the title's heaps.
            MEMORY_USAGE => {
                if let Some(out) = (*desc.cast::<MemoryUsage>()).usage.as_mut() {
                    *out = [0, 0];
                }
            }
            MEMORY_USAGE_WITH_DEVICE => {
                if let Some(out) = (*desc.cast::<MemoryUsageWithDevice>()).usage.as_mut() {
                    *out = [0, 0];
                }
            }
            _ => {
                tracing::warn!(kind = format_args!("{kind:#x}"), "host upscaler: a FidelityFX query it has no answer to");
                return UNKNOWN_TYPE;
            }
        }
        static SEEN: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&kind) {
            seen.push(kind);
            tracing::info!(kind = format_args!("{kind:#x}"), "host upscaler: a FidelityFX query of the title answered (first of its kind)");
        }
    }
    OK
}

pub(super) fn configure(_context: *mut *mut c_void, desc: *mut Header) -> u32 {
    // SAFETY: the title passes a description.
    let kind = unsafe { desc.as_ref() }.map_or(0, |h| h.kind);
    once!(info, kind = format_args!("{kind:#x}"), "host upscaler: the title's FidelityFX settings have no DLSS counterpart; accepted and ignored");
    OK
}

pub(super) fn dispatch(context: *mut *mut c_void, desc: *mut Header) -> u32 {
    // SAFETY: the title passes its context slot.
    let Some(ours) = (unsafe { ours(context) }) else {
        return PARAMETER;
    };
    // SAFETY: the title passes a description.
    let kind = unsafe { (*desc).kind };
    if kind != DISPATCH_UPSCALE {
        once!(warn, kind = format_args!("{kind:#x}"), "host upscaler: only upscaling is served; a FidelityFX dispatch was refused");
        return UNKNOWN_TYPE;
    }
    let Some(&backend) = BACKEND.get() else {
        return NO_PROVIDER;
    };
    // SAFETY: an upscaling, by its type.
    let job = Job(unsafe { desc.cast::<DispatchUpscale>().read() });
    let flags = ours.flags;
    // At this place of the list: now, or when a list that is only written
    // down is translated. The title is told it went well either way.
    let placed = victory_gpu::external::in_order(job.0.list.cast::<Object>().cast_const(), move |list| {
        let job = job;
        upscale(backend, flags, &job.0, list);
    });
    if placed {
        OK
    } else {
        once!(warn, "host upscaler: an upscaling into a list that is not being recorded; it did not run");
        PARAMETER
    }
}

/// The title's upscaling description, for the thread that records it.
struct Job(DispatchUpscale);

// SAFETY: plain values and the title's objects, which live until the list
// has run and are used in the list's order.
unsafe impl Send for Job {}

/// Records the upscaling `d` (of a context made with `flags`) into `list`.
fn upscale(backend: &dyn Upscaler, flags: u32, d: &DispatchUpscale, list: *const Object) {
    // Colour, depth, motion vectors, exposure, (reactive and transparency
    // masks,) output: their host copies, in the states the title names as
    // far as the host copies can be in them (a depth target read as a
    // shader resource keeps what its flags allow); the backend is told the
    // state each is really in.
    let host = |index: usize| {
        let r = &d.resources[index];
        if r.resource.is_null() {
            return Ok(None);
        }
        host_state(r.state)
            .and_then(|w| victory_gpu::external::host_texture(list, r.resource.cast::<Object>(), w))
            .map(Some)
            .ok_or((index, r.state))
    };
    let textures = (|| Ok::<_, (usize, u32)>((host(0)?, host(1)?, host(2)?, host(3)?, host(6)?)))();
    let (color, depth, motion, exposure, output) = match textures {
        Ok((Some(color), Some(depth), Some(motion), exposure, Some(output))) => (color, depth, motion, exposure, output),
        Ok(_) => {
            once!(warn, "host upscaler: an upscaling without colour, depth, motion vectors or output; it did not run");
            return;
        }
        Err((index, state)) => {
            once!(
                warn,
                index,
                state,
                state_known = host_state(state).is_some(),
                list_is_ours = victory_gpu::external::record(list, |_| ()).is_some(),
                description = ?d.resources[index].description,
                "host upscaler: a resource has no host texture; the upscaling did not run"
            );
            return;
        }
    };
    let f = |i: usize| f32::from_bits(d.rest[i]);
    once!(
        info,
        masks = ?[!d.resources[4].resource.is_null(), !d.resources[5].resource.is_null()],
        exposure = exposure.is_some(),
        sharpening = d.rest[8] & 0xff != 0,
        sharpness = f(9),
        frame_time_ms = f(10),
        metres_per_unit = f(16),
        dispatch_flags = format_args!("{:#x}", d.rest[17]),
        descriptions = ?[d.resources[0].description, d.resources[1].description, d.resources[2].description, d.resources[6].description],
        "host upscaler: the title's first upscaling (reactive and transparency masks, sharpening: not carried over)"
    );
    let non_linear = flags & NON_LINEAR_COLOR != 0 || d.rest[17] & DISPATCH_NON_LINEAR != 0;
    let frame = Frame {
        color: (&color.0, color.1),
        depth: (&depth.0, depth.1),
        motion: (&motion.0, motion.1),
        exposure: exposure.as_ref().map(|e| (&e.0, e.1)),
        output: (&output.0, output.1),
        render: [d.rest[4], d.rest[5]],
        output_size: [d.rest[6], d.rest[7]],
        jitter: [f(0), f(1)],
        motion_scale: [f(2), f(3)],
        motion_at_output: flags & MOTION_AT_OUTPUT_SIZE != 0,
        motion_jittered: flags & MOTION_JITTERED != 0,
        depth_inverted: flags & DEPTH_INVERTED != 0,
        hdr: flags & HDR != 0 && !non_linear,
        auto_exposure: flags & AUTO_EXPOSURE != 0,
        pre_exposure: f(11),
        reset: d.rest[12] & 0xff != 0,
        near: f(13),
        far: f(14),
        fov_y: f(15),
        frame_time_ms: f(10),
    };
    if let Some(Err(why)) = victory_gpu::external::record(list, |host_list| backend.upscale(host_list, &frame)) {
        once!(error, "host upscaler: an upscaling failed ({why}); the title's output texture was not written");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phase count the title's own upscaler left unset (or refused) is
    /// answered; one it gave is kept.
    #[test]
    fn a_missing_phase_count_is_answered() {
        let mut out = 0i32;
        let mut q = GetJitterPhaseCount {
            header: Header { kind: GET_JITTER_PHASE_COUNT, next: std::ptr::null_mut() },
            render_width: 1920,
            display_width: 3840,
            count: &mut out,
        };
        let desc = (&raw mut q).cast::<Header>();
        assert_eq!(answer_phase_count(desc, PARAMETER), OK);
        assert_eq!(out, 32);
        out = 0;
        assert_eq!(answer_phase_count(desc, OK), OK);
        assert_eq!(out, 32);
        out = 5;
        assert_eq!(answer_phase_count(desc, OK), OK);
        assert_eq!(out, 5);
        q.render_width = 0;
        q.display_width = 0;
        out = 0;
        let desc = (&raw mut q).cast::<Header>();
        assert_eq!(answer_phase_count(desc, OK), OK);
        assert!(out >= 1);
    }

    /// AMD's sequence, which the title jitters its projection with.
    #[test]
    fn the_jitter_is_amds_halton_sequence() {
        assert_eq!(jitter_phase_count(2560, 3840), 18);
        assert_eq!(jitter_phase_count(1920, 3840), 32);
        assert_eq!(jitter_phase_count(3840, 3840), 8);
        assert_eq!(jitter_offset(0, 8), [0.0, 1.0 / 3.0 - 0.5]);
        assert_eq!(jitter_offset(1, 8), [-0.25, 2.0 / 3.0 - 0.5]);
        // It repeats after its phases, and stays inside the pixel.
        assert_eq!(jitter_offset(8, 8), jitter_offset(0, 8));
        for i in 0..64 {
            let [x, y] = jitter_offset(i, 32);
            assert!(x.abs() <= 0.5 && y.abs() <= 0.5);
        }
    }

    #[test]
    fn queries_answer_as_amds_upscaler_does() {
        let (mut w, mut h) = (0u32, 0u32);
        let mut q = GetRenderResolution {
            header: Header { kind: GET_RENDER_RESOLUTION, next: std::ptr::null_mut() },
            display: [3840, 2160],
            quality: 3,
            width: &mut w,
            height: &mut h,
        };
        assert_eq!(query(std::ptr::null_mut(), &raw mut q.header), OK);
        assert_eq!((w, h), (1920, 1080));
        let mut count = 0i32;
        let mut q = GetJitterPhaseCount {
            header: Header { kind: GET_JITTER_PHASE_COUNT, next: std::ptr::null_mut() },
            render_width: 1920,
            display_width: 3840,
            count: &mut count,
        };
        assert_eq!(query(std::ptr::null_mut(), &raw mut q.header), OK);
        assert_eq!(count, 32);
        let mut unknown = Header { kind: 0x7777, next: std::ptr::null_mut() };
        assert_eq!(query(std::ptr::null_mut(), &mut unknown), UNKNOWN_TYPE);
    }

    #[test]
    fn a_context_is_ours_only_with_our_mark() {
        let mut slot: *mut c_void = std::ptr::null_mut();
        let mut desc = CreateUpscale {
            header: Header { kind: CREATE_UPSCALE, next: std::ptr::null_mut() },
            flags: HDR | DEPTH_INVERTED,
            max_render: [1920, 1080],
            max_output: [3840, 2160],
            message: 0,
        };
        assert_eq!(create(&mut slot, &raw mut desc.header), OK);
        // SAFETY: the slot `create` filled.
        assert_eq!(unsafe { ours(&mut slot) }.map(|c| c.flags), Some(HDR | DEPTH_INVERTED));
        assert_eq!(destroy(&mut slot), OK);
        assert!(slot.is_null());
        assert_eq!(destroy(&mut slot), PARAMETER);
        let mut other = Header { kind: 0x2_0000, next: std::ptr::null_mut() };
        assert_eq!(create(&mut slot, &mut other), NO_PROVIDER);
    }
}
