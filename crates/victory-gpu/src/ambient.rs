//! A stand-in for the indirect light the title traces rays for.
//!
//! With the ray-tracing structure builder off (`crate::pipeline::builder_runs`)
//! the title's rays find nothing. Its program that resolves the diffuse light
//! of its screen probes (`CS_ScreenProbes_TemporalResolve_*`) then leaves the
//! sky's light outdoors, nothing indoors (walls are black where no lamp or
//! sun reaches) and, in blocks of the picture's tiles, what the probes'
//! memory held. Right after that program the light is raised to an estimate
//! and bounded by a multiple of it.
//!
//! That a stand-in can be put there, and in which of the program's images,
//! is beloooo's find (VIctory-Belobug-AMD, 06031237, 4f45ab88, cd9f78ce,
//! GPL-3.0-or-later), which puts a constant there, shaded in creases from the
//! scene's depth. The estimate here is worked out from the frame instead, so
//! it follows the hour, the room and its lamps by itself: the light the
//! visible surfaces send out (the title keeps it for its own screen traces:
//! `CS_CacheScreenSpaceRadiance`, the frame before) is averaged in 64x36
//! cells, and every point gets the mean of the cells weighted by the share of
//! its surroundings they cover, as a directional value (first-order harmonics
//! at 160x90 points, taken between them by depth).
//!
//! What it assumes: what is not on screen sends out what is (with a slow
//! mean of the last seconds, so that turning into a dark corner is not black
//! at once). No occlusion but the creases'.
//!
//! `VICTORY_AMBIENT`:
//! - unset or `0`/`off`: the title's own output.
//! - a number: that share of the estimate (1: all of it).
//! - `constant:<level>`: the constant, as beloooo's fork has it.
//!
//! `VICTORY_AMBIENT_SHADE=<strength>,<radius in metres>` sets the creases.
//!
//! `VICTORY_AMBIENT_MIN=<level>` sets a floor for the estimate so dark
//! rooms without direct light do not go pitch black.
//!
//! The air of a room is lit by the same probes: without rays they hold the
//! sky's light indoors too, a brown veil at sunset. `VICTORY_AMBIENT_AIR`
//! bounds what the air scatters by a multiple of the light around.
//!
//! The title's reflections are traced too: without rays every glossy
//! surface mirrors the sky, unoccluded (a white glaze on a wooden floor,
//! five to ten times the room). After the program that writes them last
//! (`CS_UpdateHistory`) they are held to a multiple of the estimate where
//! they are (beloooo's find, Belobug 44dcd4371 and c3bd5612c, which hold
//! them to a part of its constant): `VICTORY_AMBIENT_REFLECT=<most>[,<least>]`,
//! both as multiples of the light around; 0 leaves them.
//!
//! What is not on screen is the weak point: a surface that faces the eye
//! (a hand held up, a face) has everything that lights it behind the eye,
//! and got a fifth of the mean. The title draws its whole sky into a small
//! image for its reflections (`ps_sky_paraboloid_reflection`, 128x128); the
//! dome's mean gives a level and a hue, `gain * mean^power + least`
//! (beloooo's find and numbers, Belobug 705a2d094, e48eb83eb, 4f62c2a5e,
//! where it is the whole light). Here it is the least the estimate gives,
//! shaded in creases like the rest: `VICTORY_AMBIENT_SKY=gain,least,power`
//! sets it, `off` leaves the estimate without.
//! shortcut: the sky's light is taken to reach everything, indoors too.
//!
//! `VICTORY_AMBIENT_HUE=<multiple>` bounds how far what the resolve left
//! may be from the estimate's hue (1.25; 0: any hue).
//!
//! `VICTORY_AMBIENT_MAGENTA=0` leaves magenta in what the resolve left to
//! the bound alone (it is zeroed otherwise: the probes' memory, not light).

use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host};

/// Programs of the title the stand-in follows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    /// Resolves the diffuse light.
    Resolve,
    /// Keeps the light visible surfaces send out.
    Radiance,
    /// Adds the lamps' light to what the air scatters (the last program
    /// that writes that volume before it is summed up along the view).
    Air,
    /// Writes the reflections for the last time in a frame; the lighting
    /// programs read them next.
    Reflections,
}

/// The program's part in this, by its name in the container.
pub(crate) fn role(container: &[u8]) -> Option<Role> {
    let has = |name: &[u8]| container.windows(name.len()).any(|w| w == name);
    if has(b"CS_ScreenProbes_TemporalResolve") {
        Some(Role::Resolve)
    } else if has(b"CS_CacheScreenSpaceRadiance") {
        Some(Role::Radiance)
    } else if has(b"CS_AccumulateLocalLightInscatteringVolume") {
        Some(Role::Air)
    } else if has(b"CS_UpdateHistory") {
        Some(Role::Reflections)
    } else {
        None
    }
}

/// The program that tells the screen probes which rays to trace, by the
/// start of its name: left out while no rays run
/// (`crate::pipeline::idle_without_rays`; Belobug 6986a57).
const ACTIVATES_RAYS: &[u8] = b"CS_ScreenProbes_ActivateRays";

/// Whether the program in `container` is the one that activates the screen
/// probes' rays, by its name.
pub(crate) fn activates_rays(container: &[u8]) -> bool {
    container.windows(ACTIVATES_RAYS.len()).any(|w| w == ACTIVATES_RAYS)
}

/// The program that draws the sky as the title reflects it, by the start of
/// its name: a small image of the whole sky, in the light's own units.
const SKY: &[u8] = b"ps_sky_paraboloid_reflection";

/// Whether the program in `container` draws that image, by its name.
pub(crate) fn draws_sky(container: &[u8]) -> bool {
    container.windows(SKY.len()).any(|w| w == SKY)
}

/// The side of the sky dome, and the bytes of a row of it and of all of it
/// (four 16-bit floats a texel).
const SKY_SIDE: u32 = 128;
const SKY_ROW: u32 = SKY_SIDE * 8;
const SKY_BYTES: u64 = (SKY_SIDE * SKY_ROW) as u64;

/// The sky dome that program last drew into: the host copy, held, since
/// the title's object may go at any time.
///
/// The program draws twice a frame, on two lists recorded at once: the dome
/// (128x128, R16G16B16A16_FLOAT) and another image of the sky, three times
/// brighter on average. Only the dome is followed (Belobug e48eb83eb:
/// following whichever was recorded last made the light jump).
static SKY_IMAGE: Mutex<Option<std::sync::Arc<crate::resource::HostTexture>>> = Mutex::new(None);

/// At a draw of the sky's program into `image`.
pub(crate) fn sky_drawn(image: crate::object::This) {
    if !on() || sky().is_none() {
        return;
    }
    let Some(copy) = crate::resource::host_copy(image) else {
        return;
    };
    {
        let held = SKY_IMAGE.lock().unwrap_or_else(|e| e.into_inner());
        if held.as_ref().is_some_and(|h| std::sync::Arc::ptr_eq(h, &copy)) {
            return;
        }
    }
    // SAFETY: a live resource.
    let d = unsafe { copy.texture.GetDesc() };
    let dome = d.Format.0 == 10
        && d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
        && (d.Width, d.Height) == (u64::from(SKY_SIDE), SKY_SIDE)
        && d.SampleDesc.Count == 1;
    static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 6 {
        tracing::info!(
            format = d.Format.0,
            width = d.Width,
            height = d.Height,
            slices = d.DepthOrArraySize,
            levels = d.MipLevels,
            dome,
            "the sky's program draws into an image"
        );
    }
    if dome {
        *SKY_IMAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(copy);
    }
}

/// Gain, least level and power of the sky's part: its level is
/// `gain * mean^power + least` (`VICTORY_AMBIENT_SKY=gain,least,power`;
/// `off` or a gain and least of 0: none). The power is under one because
/// the title's exposure follows the light (Belobug 705a2d094: the sky's mean
/// goes from 0.001 at night to several units by day, the levels that look
/// right only from 0.07 to 1.4).
fn sky() -> Option<[f32; 3]> {
    static SET: OnceLock<Option<[f32; 3]>> = OnceLock::new();
    *SET.get_or_init(|| parse_sky(&std::env::var("VICTORY_AMBIENT_SKY").unwrap_or_default()))
}

/// What the sky gives unless something else is asked for.
const SKY_SET: [f32; 3] = [0.36, 0.05, 0.4];

fn parse_sky(set: &str) -> Option<[f32; 3]> {
    if matches!(set.trim(), "off" | "0") {
        return None;
    }
    let mut sky = SKY_SET;
    for (to, v) in sky.iter_mut().zip(set.split(',')) {
        if let Some(v) = v.trim().parse().ok().filter(|v: &f32| v.is_finite() && *v >= 0.0) {
            *to = v;
        }
    }
    (sky[0] > 0.0 || sky[1] > 0.0).then_some(sky)
}

/// Where the resolve writes the diffuse light (R9G9B9E5 as 32-bit integers)
/// and reads the scene's depth, and where the other writes the light of
/// visible surfaces: root parameter and place in its table, in dwords.
pub(crate) const LIGHT: (u8, u32) = (1, 176);
pub(crate) const DEPTH: (u8, u32) = (0, 464);
pub(crate) const RADIANCE: (u8, u32) = (1, 16);
/// Where the light the air scatters is.
pub(crate) const AIR: (u8, u32) = (1, 32);
/// Where the reflections are (R9G9B9E5 as 32-bit integers).
pub(crate) const REFLECTIONS: (u8, u32) = (1, 40);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Set {
    Off,
    Estimate(f32),
    Constant(f32),
}

fn parse(set: &str) -> Set {
    let set = set.trim();
    let number = |t: &str| {
        t.trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
    };
    match set.strip_prefix("constant:") {
        Some(level) => number(level).map_or(Set::Off, Set::Constant),
        None => number(set).map_or(Set::Off, Set::Estimate),
    }
}

fn set() -> Set {
    static SET: OnceLock<Set> = OnceLock::new();
    *SET.get_or_init(|| parse(&std::env::var("VICTORY_AMBIENT").unwrap_or_default()))
}

/// Whether the stand-in is on: set, and the title's rays find nothing.
pub(crate) fn on() -> bool {
    set() != Set::Off && !crate::pipeline::builder_runs()
}

/// The most the air scatters, as a multiple of the light around
/// (`VICTORY_AMBIENT_AIR=<multiple>`; unset or 0: what the title has).
fn air() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT_AIR").unwrap_or_default();
        set.trim()
            .parse()
            .ok()
            .filter(|v: &f32| v.is_finite() && *v > 0.0)
            .unwrap_or(0.0)
    })
}

/// Strength and radius of the creases' shade.
fn shade() -> [f32; 2] {
    static SET: OnceLock<[f32; 2]> = OnceLock::new();
    *SET.get_or_init(|| {
        let mut shade = [1.0, 0.6];
        let set = std::env::var("VICTORY_AMBIENT_SHADE").unwrap_or_default();
        for (to, v) in shade.iter_mut().zip(set.split(',')) {
            if let Ok(v) = v.trim().parse() {
                *to = v;
            }
        }
        shade
    })
}

/// Minimum indirect light for the estimate (`VICTORY_AMBIENT_MIN=<level>`;
/// unset or 0: no floor).
fn least() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| parse_min(&std::env::var("VICTORY_AMBIENT_MIN").unwrap_or_default()))
}

fn parse_min(set: &str) -> f32 {
    set.trim()
        .parse()
        .ok()
        .filter(|v: &f32| v.is_finite() && *v > 0.0)
        .unwrap_or(0.0)
}

/// Whether magenta in what the resolve left is taken for the probes'
/// memory, not light, and zeroed before the raise (beloooo's find, Belobug
/// 81ebb7bdc: the bound alone keeps its hue, dimmed). On unless
/// `VICTORY_AMBIENT_MAGENTA=0`.
fn magenta_out() -> bool {
    static SET: OnceLock<bool> = OnceLock::new();
    *SET.get_or_init(|| std::env::var_os("VICTORY_AMBIENT_MAGENTA").is_none_or(|v| v != "0"))
}

/// The most and the least a reflection holds, as multiples of the light
/// around (`VICTORY_AMBIENT_REFLECT=<most>[,<least>]`; a most of 0 leaves
/// the title's reflections alone, a least of 0 leaves dark ones dark).
/// shortcut: stands in for what the title's traced reflections would see.
fn reflect() -> [f32; 2] {
    static SET: OnceLock<[f32; 2]> = OnceLock::new();
    *SET.get_or_init(|| parse_reflect(&std::env::var("VICTORY_AMBIENT_REFLECT").unwrap_or_default()))
}

/// What holds the reflections unless something else is asked for.
const REFLECT: [f32; 2] = [0.5, 0.0];

fn parse_reflect(set: &str) -> [f32; 2] {
    let mut reflect = REFLECT;
    for (to, v) in reflect.iter_mut().zip(set.split(',')) {
        if let Some(v) = v.trim().parse().ok().filter(|v: &f32| v.is_finite() && *v >= 0.0) {
            *to = v;
        }
    }
    reflect
}

/// The most of what the resolve left that counts, as a multiple of the
/// estimate's brightness (`VICTORY_AMBIENT_MOST=<multiple>`).
fn most() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| {
        std::env::var("VICTORY_AMBIENT_MOST")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|v: &f32| v.is_finite() && *v >= 0.0)
            .unwrap_or(MOST)
    })
}

const MOST: f32 = 4.0;

/// How far from the estimate's hue what the resolve left may be: the factor
/// each channel stays within of what the estimate's hue has at the same
/// brightness (`VICTORY_AMBIENT_HUE=<factor>`, 1 or more; 0: any hue).
///
/// Without rays the resolve goes on blending its own history where it has
/// no new samples, and that history is the probes' memory: on blinds behind
/// glass, on foliage and on thin plants it stayed a saturated blue or
/// purple from frame to frame (captures light-base and light-mag,
/// 2026-11-26). The bound on its brightness let it through: a pure blue of
/// four times the estimate's brightness has 55 times its blue.
fn hue() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| {
        std::env::var("VICTORY_AMBIENT_HUE")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|v: &f32| v.is_finite() && (*v >= 1.0 || *v == 0.0))
            .unwrap_or(1.25)
    })
}

/// What is done with the resolve's other images of the light's kind (it
/// writes three: `VICTORY_AMBIENT_OTHERS`): 0: left as the title has them;
/// 1: raised and bounded as the light is; 2 (diagnostics): the first is
/// painted red and the second green, to see what each lights.
fn others_set() -> u8 {
    static SET: OnceLock<u8> = OnceLock::new();
    *SET.get_or_init(|| {
        std::env::var("VICTORY_AMBIENT_OTHERS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(OTHERS)
    })
}

const OTHERS: u8 = 0;

/// Whether the resolve's other images are asked for.
pub(crate) fn others() -> bool {
    others_set() != 0
}

/// The words of the passes' constants.
const WORDS: usize = 22;

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=22, b0), DescriptorTable(SRV(t0)), DescriptorTable(UAV(u0)), UAV(u1), UAV(u2), UAV(u3)"
#define CX 64
#define CY 36
#define CELLS (CX * CY)
#define PX 160
#define PY 90
cbuffer Set : register(b0) {
    float level; float constant; float strength; float radius;
    float near; float tan_y; uint width; uint height;
    float follow; float most; float air; float least;
    float magenta; float held; float held_least;
    float sky_gain; float sky_least; float sky_power;
    float paint_r; float paint_g; float paint_b;
    float hue;
};
Texture2D<float> depth : register(t0);
// Cells: the light they send out (then, at CELLS, the slow mean), from
// CELLS + 1 on where they are, and last what the sky gives.
RWStructuredBuffer<float4> cells : register(u1);
#define SKY (2 * CELLS + 1)
// Three a point: red, green and blue, each a mean and a direction.
RWStructuredBuffer<float4> points : register(u2);
static const float3 LUMA = float3(0.2126, 0.7152, 0.0722);

// The point seen at a texel, in metres from the eye (x right, y up, z away).
float3 seen(int2 p, float2 size) {
    float d = depth.Load(int3(clamp(p, int2(0, 0), int2(size) - 1), 0));
    float z = min(near / max(d, 1e-7), 1000.0);
    float2 ndc = (float2(p) + 0.5) / size * 2.0 - 1.0;
    return float3(ndc.x * tan_y * size.x / size.y * z, -ndc.y * tan_y * z, z);
}

#ifdef KEEP
RWTexture2D<float3> radiance : register(u0);
[RootSignature(RS)]
[numthreads(8, 4, 1)]
void keep(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    radiance.GetDimensions(w, h);
    float3 sum = 0;
    for (uint y = 0; y < 5; y++) for (uint x = 0; x < 5; x++) {
        uint2 p = uint2((float2(at.xy) + (float2(x, y) + 0.5) / 5.0) * float2(w, h) / float2(CX, CY));
        float3 c = max(radiance[p], 0.0);
        c = any(isnan(c)) || any(isinf(c)) ? 0.0 : c;
        // Lamps and glints count, but not for everything.
        sum += c / max(1.0, dot(c, LUMA) / 20.0);
    }
    cells[at.y * CX + at.x] = float4(sum / 25.0, 1.0);
}
// The sky dome as the title drew it: 128 rows of 128 texels, four 16-bit
// floats each.
RWStructuredBuffer<uint2> dome : register(u3);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void mean() {
    float3 sum = 0;
    for (uint i = 0; i < CELLS; i++) sum += cells[i].rgb;
    float4 was = cells[CELLS];
    float3 now = sum / CELLS;
    cells[CELLS] = float4(was.a > 0.0 ? lerp(was.rgb, now, follow) : now, 1.0);
    // What the sky gives: level and hue from the dome's mean (as Belobug
    // 705a2d094 has it).
    if (sky_gain <= 0.0 && sky_least <= 0.0) {
        cells[SKY] = 0.0;
        return;
    }
    float3 all = 0;
    const uint N = 16;
    for (uint y = 0; y < N; y++) for (uint x = 0; x < N; x++) {
        uint2 t = dome[((y * 2 + 1) * 128 / (N * 2)) * 128 + (x * 2 + 1) * 128 / (N * 2)];
        float3 there = float3(f16tof32(t.x & 0xffff), f16tof32(t.x >> 16), f16tof32(t.y & 0xffff));
        there = any(isnan(there)) || any(isinf(there)) ? 0.0 : there;
        // The sun's own texels are held down: it lights by itself.
        all += clamp(there, 0.0, 8.0);
    }
    all /= N * N;
    float m = dot(all, LUMA);
    // The sky's colour, half washed out (the ground lights too); a dark
    // sky has none worth following.
    const float3 NIGHT = float3(0.6, 0.75, 1.0);
    float3 hue = lerp(NIGHT, lerp(float3(1.0, 1.0, 1.0), all / max(m, 1e-6), 0.5), saturate(m / 0.05));
    float3 gives = hue * (sky_gain * pow(max(m, 0.0), sky_power) + sky_least);
    float4 gave = cells[SKY];
    cells[SKY] = float4(gave.a > 0.0 ? lerp(gave.rgb, gives, 0.1) : gives, 1.0);
}
#endif

#ifdef PLACE
[RootSignature(RS)]
[numthreads(8, 4, 1)]
void place(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    float3 p = seen(int2((float2(at.xy) + 0.5) * size / float2(CX, CY)), size);
    cells[CELLS + 1 + at.y * CX + at.x] = float4(p, p.z * p.z);
}
#endif

#ifdef GATHER
[RootSignature(RS)]
[numthreads(8, 2, 1)]
void gather(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    float3 p = seen(int2((float2(at.xy) + 0.5) * size / float2(PX, PY)), size);
    float r2 = 0.25 + 0.0025 * p.z * p.z;
    float whole = 0;
    float3 toward = 0, light = 0, red = 0, green = 0, blue = 0;
    for (uint i = 0; i < CELLS; i++) {
        float4 c = cells[CELLS + 1 + i];
        float3 L = cells[i].rgb;
        float3 d = c.xyz - p;
        float dd = dot(d, d);
        float w = c.w / (dd + r2);
        float3 dir = d / sqrt(dd + 0.25 * r2);
        whole += w;
        toward += w * dir;
        light += w * L;
        red += w * L.r * dir;
        green += w * L.g * dir;
        blue += w * L.b * dir;
    }
    whole = max(whole, 1e-6);
    light /= whole;
    toward /= whole;
    uint at0 = (at.y * PX + at.x) * 3;
    points[at0] = float4(red / whole - light.r * toward, light.r);
    points[at0 + 1] = float4(green / whole - light.g * toward, light.g);
    points[at0 + 2] = float4(blue / whole - light.b * toward, light.b);
}
#endif

#ifdef AIR
RWTexture3D<float3> scattered : register(u0);
// No more than a multiple of the light around: of the room as the last
// seconds showed it, and of the surface seen in this direction (towards a
// window the air outside may be as bright as the sky is).
[RootSignature(RS)]
[numthreads(8, 8, 2)]
void clear_air(uint3 at : SV_DispatchThreadID) {
    uint w, h, d;
    scattered.GetDimensions(w, h, d);
    if (at.x >= w || at.y >= h || at.z >= d) return;
    uint2 q = min(uint2((float2(at.xy) + 0.5) / float2(w, h) * float2(PX, PY)), uint2(PX - 1, PY - 1));
    uint at0 = (q.y * PX + q.x) * 3;
    float3 seen_there = float3(points[at0].w, points[at0 + 1].w, points[at0 + 2].w);
    float far = float(at.z) / float(d);
    float cap = max(air * lerp(dot(cells[CELLS].rgb, LUMA), dot(seen_there, LUMA), far), air * least);
    float3 c = scattered[at];
    float has = dot(c, LUMA);
    if (has > cap) scattered[at] = c * (cap / has);
}
#endif

#if defined(RAISE) || defined(HOLD)
RWTexture2D<uint> light : register(u0);
float3 unpack(uint v) {
    return float3(v & 511, (v >> 9) & 511, (v >> 18) & 511) * exp2(float(v >> 27) - 24.0);
}
uint pack(float3 c) {
    c = clamp(c, 0.0, 65408.0);
    float top = max(c.r, max(c.g, c.b));
    int e = clamp(int(floor(log2(max(top, 1e-9)))) + 16, 0, 31);
    float scale = exp2(float(24 - e));
    if (top * scale >= 511.5) { e = min(e + 1, 31); scale = exp2(float(24 - e)); }
    uint3 m = min(uint3(c * scale + 0.5), 511u);
    return m.r | (m.g << 9) | (m.b << 18) | (uint(e) << 27);
}
#endif

#ifdef HOLD
// Reflections: no brighter than a multiple of the light around, as the last
// resolve's points have it (no depth here: taken between the four around).
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void hold(uint3 at : SV_DispatchThreadID) {
    if (at.x >= width || at.y >= height) return;
    float3 around;
    if (constant > 0.0) {
        around = float3(1.0, 0.98, 0.94) * constant;
    } else {
        float2 g = (float2(at.xy) + 0.5) / float2(width, height) * float2(PX, PY) - 0.5;
        int2 g0 = int2(floor(g));
        float2 f = g - float2(g0);
        float3 all = 0;
        for (int k = 0; k < 4; k++) {
            int2 q = clamp(g0 + int2(k & 1, k >> 1), int2(0, 0), int2(PX, PY) - 1);
            float2 b = float2(k & 1 ? f.x : 1 - f.x, k >> 1 ? f.y : 1 - f.y);
            uint at0 = (q.y * PX + q.x) * 3;
            all += b.x * b.y * float3(points[at0].w, points[at0 + 1].w, points[at0 + 2].w);
        }
        around = lerp(all, cells[CELLS].rgb, 0.2) * level;
        if (least > 0.0) around = max(around, float3(1.0, 0.98, 0.94) * least);
    }
    float3 c = unpack(light[at.xy]);
    float has = dot(c, LUMA);
    float most = dot(around, LUMA) * held;
    if (has > most) light[at.xy] = pack(c * (most / has));
    else if (has < dot(around, LUMA) * held_least) light[at.xy] = pack(around * held_least);
}
#endif

#ifdef RAISE
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void raise(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    if (at.x >= width || at.y >= height) return;
    int2 p = int2(at.xy);
    float3 c = seen(p, size);
    float3 l = seen(p + int2(-1, 0), size), r = seen(p + int2(1, 0), size);
    float3 u = seen(p + int2(0, -1), size), d = seen(p + int2(0, 1), size);
    float3 dx = abs(l.z - c.z) < abs(r.z - c.z) ? c - l : r - c;
    float3 dy = abs(u.z - c.z) < abs(d.z - c.z) ? c - u : d - c;
    float3 n = normalize(cross(dy, dx) + float3(0, 0, -1e-9));
    if (dot(n, c) > 0.0) n = -n;
    // How open it is (as in beloooo's cd9f78ce).
    float open = 1.0;
    float px = min(radius * size.y / (2.0 * tan_y * c.z), 128.0);
    float noise = frac(52.9829189 * frac(dot(float2(p), float2(0.06711056, 0.00583715))));
    if (strength > 0.0 && px >= 1.5 && c.z < 400.0) {
        float sum = 0.0;
        const int N = 12;
        for (int i = 0; i < N; i++) {
            float a = i * 2.399963 + noise * 6.2831853;
            float2 o = float2(cos(a), sin(a)) * (sqrt((i + 0.5) / N) * px);
            float3 v = seen(p + int2(round(o)), size) - c;
            float vv = dot(v, v);
            float over = dot(v, n) - 0.004 * c.z - 0.005;
            sum += max(over, 0.0) / sqrt(vv + 1e-6) * saturate(1.0 - vv / (radius * radius));
        }
        open = saturate(1.0 - strength * 2.0 * sum / N);
    }
    float3 paint = float3(paint_r, paint_g, paint_b);
    float3 found;
    if (constant > 0.0) {
        found = float3(1.0, 0.98, 0.94) * constant * open;
        light[p] = pack(found * paint);
        return;
    }
    // Between the four points around, those at its own depth first; which
    // four is dithered a little.
    float2 g = (float2(p) + 0.5) / size * float2(PX, PY) - 0.5 + (noise - 0.5) * 0.5;
    int2 g0 = int2(floor(g));
    float2 f = g - float2(g0);
    float4 sr = 0, sg = 0, sb = 0;
    float sum = 0;
    for (int k = 0; k < 4; k++) {
        int2 q = clamp(g0 + int2(k & 1, k >> 1), int2(0, 0), int2(PX, PY) - 1);
        float z = seen(int2((float2(q) + 0.5) * size / float2(PX, PY)), size).z;
        float2 b = float2(k & 1 ? f.x : 1 - f.x, k >> 1 ? f.y : 1 - f.y);
        float w = (b.x * b.y + 1e-3) / (0.02 + abs(z - c.z) / c.z);
        uint at0 = (q.y * PX + q.x) * 3;
        sr += w * points[at0];
        sg += w * points[at0 + 1];
        sb += w * points[at0 + 2];
        sum += w;
    }
    float3 all = float3(sr.w, sg.w, sb.w) / sum;
    found = all + 1.2 * float3(dot(sr.xyz, n), dot(sg.xyz, n), dot(sb.xyz, n)) / sum;
    found = clamp(found, 0.2 * all, 2.5 * all);
    found = lerp(found, cells[CELLS].rgb, 0.2) * level * open;
    if (least > 0.0) {
        found = max(found, float3(1.0, 0.98, 0.94) * least * open);
    }
    // What bounds what the program left: the estimate without the sky's
    // part, which is no measure of what the probes' memory may hold.
    float bound = dot(found, LUMA);
    // No less than what the sky gives: what lights a surface that faces the
    // eye is not on screen.
    found = max(found, cells[SKY].rgb * open);
    float3 was = unpack(light[p]);
    // Magenta (green far below red and blue) is what the probes' memory
    // held, not light (beloooo's find, Belobug 81ebb7bdc).
    if (magenta > 0.0 && was.g < 0.5 * min(was.r, was.b) && max(was.r, was.b) > 0.02) was = 0.0;
    // What the program left counts as far as it can be true: the sky's light
    // outdoors, not the blocks of other memory.
    float top = max(bound * most, 1e-6);
    float has = dot(was, LUMA);
    was *= min(1.0, top / max(has, 1e-6));
    // Nor the hue of other memory: the bound above is on brightness alone,
    // and a blue or purple far below it in brightness still has dozens of
    // times the estimate's blue. Every channel stays within a factor of
    // what the estimate's own hue has at that brightness.
    if (hue > 0.0) {
        float3 like = found * (dot(was, LUMA) / max(dot(found, LUMA), 1e-6));
        was = clamp(was, like / hue, like * hue);
    }
    light[p] = pack(max(was, found) * paint);
}
#endif
"#;

/// Bytes of the cells' buffer and of the points'.
const CELLS: u64 = 64 * 36;
const CELL_BYTES: u64 = (2 * CELLS + 2) * 16;
const POINT_BYTES: u64 = 160 * 90 * 48;

struct Pass {
    root: ID3D12RootSignature,
    keep: ID3D12PipelineState,
    mean: ID3D12PipelineState,
    place: ID3D12PipelineState,
    gather: ID3D12PipelineState,
    raise: ID3D12PipelineState,
    air: ID3D12PipelineState,
    hold: ID3D12PipelineState,
    cells: ID3D12Resource,
    points: ID3D12Resource,
    /// The sky dome's texels, copied out of the title's image.
    dome: ID3D12Resource,
}
// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

fn make(
    device: &ID3D12Device,
    buffer: impl Fn(u64) -> Option<ID3D12Resource>,
) -> windows::core::Result<Pass> {
    let mut root: Option<ID3D12RootSignature> = None;
    let mut pipeline = |part: &str, entry| -> windows::core::Result<ID3D12PipelineState> {
        let code = crate::host::compile(&format!("#define {part}\n{SOURCE}"), entry, s!("cs_5_1"))?;
        // SAFETY: the root signature is inside the shader; plain creations.
        unsafe {
            if root.is_none() {
                root = Some(device.CreateRootSignature(0, &code)?);
            }
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: code.as_ptr().cast(),
                    BytecodeLength: code.len(),
                },
                ..Default::default()
            };
            device.CreateComputePipelineState(&desc)
        }
    };
    let keep = pipeline("KEEP", s!("keep"))?;
    let mean = pipeline("KEEP", s!("mean"))?;
    let place = pipeline("PLACE", s!("place"))?;
    let gather = pipeline("GATHER", s!("gather"))?;
    let raise = pipeline("RAISE", s!("raise"))?;
    let air = pipeline("AIR", s!("clear_air"))?;
    let hold = pipeline("HOLD", s!("hold"))?;
    let made = |bytes| buffer(bytes).ok_or_else(windows::core::Error::empty);
    Ok(Pass {
        root: root.ok_or_else(windows::core::Error::empty)?,
        keep,
        mean,
        place,
        gather,
        raise,
        air,
        hold,
        cells: made(CELL_BYTES)?,
        points: made(POINT_BYTES)?,
        dome: made(SKY_BYTES)?,
    })
}

fn pass() -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| {
        let host = host()?;
        make(&host.device, |bytes| host.uav_buffer(bytes))
            .inspect_err(|e| tracing::warn!(error = %e, "no stand-in for the indirect light"))
            .ok()
    })
    .as_ref()
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        ..Default::default()
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Whether the light of a frame has been kept: the estimate has something
/// to begin with.
static KEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a resolve has gathered the light around every point: what the
/// reflections are held to.
static GATHERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the passes read and write, as views in the heap the list has set.
pub(crate) struct Views {
    /// The image the title's program just wrote.
    pub(crate) image: D3D12_GPU_DESCRIPTOR_HANDLE,
    /// For [`Role::Resolve`]: the program's other images of the same kind
    /// and size, by their place in its table ([`others`]).
    pub(crate) others: Vec<(u32, D3D12_GPU_DESCRIPTOR_HANDLE)>,
    /// The scene's depth, for [`Role::Resolve`].
    pub(crate) depth: D3D12_GPU_DESCRIPTOR_HANDLE,
    /// The size of the image, and how many slices it has.
    pub(crate) size: [u32; 2],
    pub(crate) slices: u32,
}

/// After the title's program of that role, in `list` with the program's view
/// heap set. Replaces the list's compute root signature and pipeline; false
/// if it recorded nothing.
pub(crate) fn after(list: &HostList, role: Role, views: &Views) -> bool {
    use std::sync::atomic::Ordering;
    let Some(pass) = pass() else {
        return false;
    };
    let (level, constant) = match set() {
        Set::Off => return false,
        Set::Estimate(level) => (level, 0.0f32),
        Set::Constant(level) => (0.0, level),
    };
    if role != Role::Radiance && constant == 0.0 && !KEPT.load(Ordering::Relaxed) {
        return false;
    }
    if role == Role::Air && (constant > 0.0 || air() == 0.0) {
        return false;
    }
    let [held, held_least] = reflect();
    if role == Role::Reflections && (held == 0.0 || (constant == 0.0 && !GATHERED.load(Ordering::Relaxed))) {
        return false;
    }
    static SAID: Mutex<u32> = Mutex::new(0);
    {
        let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
        if *said < 4 {
            *said += 1;
            tracing::info!(?role, size = ?views.size, level, constant, "a stand-in for the indirect light");
        }
    }
    if role == Role::Reflections {
        static HELD: std::sync::Once = std::sync::Once::new();
        HELD.call_once(|| tracing::info!(size = ?views.size, held, held_least, "reflections held to the light around"));
    }
    list.keep(&pass.cells);
    list.keep(&pass.points);
    list.keep(&pass.dome);
    // The sky dome, if the title has drawn it: its texels are copied out
    // once a frame, before the mean that reads them.
    let dome = (role == Role::Radiance && constant == 0.0)
        .then(sky)
        .flatten()
        .and_then(|set| Some((set, SKY_IMAGE.lock().unwrap_or_else(|e| e.into_inner()).clone()?)));
    let [sky_gain, sky_least, sky_power] = dome.as_ref().map_or([0.0, 0.0, 1.0], |d| d.0);
    let [strength, radius] = shade();
    let floats = [level, constant, strength, radius, 0.1, 0.45];
    let mut words = [0u32; WORDS];
    for (to, v) in words.iter_mut().zip(floats) {
        *to = v.to_bits();
    }
    words[6] = views.size[0];
    words[7] = views.size[1];
    words[8] = 0.04f32.to_bits();
    words[9] = most().to_bits();
    words[10] = air().to_bits();
    words[11] = least().to_bits();
    words[12] = f32::from(magenta_out()).to_bits();
    words[13] = held.to_bits();
    words[14] = held_least.to_bits();
    words[15] = sky_gain.to_bits();
    words[16] = sky_least.to_bits();
    words[17] = sky_power.to_bits();
    for w in &mut words[18..21] {
        *w = 1.0f32.to_bits();
    }
    words[21] = hue().to_bits();
    if role == Role::Resolve && !views.others.is_empty() {
        static OTHER: std::sync::Once = std::sync::Once::new();
        OTHER.call_once(|| {
            tracing::info!(
                places = ?views.others.iter().map(|o| o.0).collect::<Vec<_>>(),
                set = others_set(),
                "the resolve's other images of the light's kind"
            );
        });
    }
    let l = &list.list;
    // SAFETY: an open list with the heap of the views set; the title's
    // program has just used both views, so their textures are in the states
    // these views need; buffers the list keeps.
    unsafe {
        uav_barrier(l);
        l.SetComputeRootSignature(&pass.root);
        l.SetComputeRoot32BitConstants(0, WORDS as u32, words.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(1, views.depth);
        l.SetComputeRootDescriptorTable(2, views.image);
        l.SetComputeRootUnorderedAccessView(3, pass.cells.GetGPUVirtualAddress());
        l.SetComputeRootUnorderedAccessView(4, pass.points.GetGPUVirtualAddress());
        l.SetComputeRootUnorderedAccessView(5, pass.dome.GetGPUVirtualAddress());
        let run = |pso: &ID3D12PipelineState, x, y, z| {
            l.SetPipelineState(pso);
            l.Dispatch(x, y, z);
            uav_barrier(l);
        };
        match role {
            Role::Radiance => {
                run(&pass.keep, 8, 9, 1);
                if let Some((set, copy)) = &dome {
                    static FOLLOWS: std::sync::Once = std::sync::Once::new();
                    FOLLOWS.call_once(|| {
                        tracing::info!(gain = set[0], least = set[1], power = set[2], "the stand-in for the indirect light has the sky's as its least");
                    });
                    list.keep(&copy.texture);
                    // (The list's first use of it: its prelude brings it there.)
                    let source = D3D12_RESOURCE_STATE_COPY_SOURCE;
                    if let Some(before) = list.transition_copy(copy, source.0 as u32) {
                        l.ResourceBarrier(&[crate::host::transition(
                            &copy.texture,
                            D3D12_RESOURCE_STATES(before as i32),
                            source,
                        )]);
                    }
                    let from = D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&copy.texture),
                        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                    };
                    let to = D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&pass.dome),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                                Offset: 0,
                                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                    Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT,
                                    Width: SKY_SIDE,
                                    Height: SKY_SIDE,
                                    Depth: 1,
                                    RowPitch: SKY_ROW,
                                },
                            },
                        },
                    };
                    // The buffer rests in the common state; a copy takes it
                    // from there by itself, the rest is said.
                    let (common, dest, written) =
                        (D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
                    l.ResourceBarrier(&[crate::host::transition(&pass.dome, common, dest)]);
                    l.CopyTextureRegion(&to, 0, 0, 0, &from, None);
                    l.ResourceBarrier(&[crate::host::transition(&pass.dome, dest, written)]);
                    run(&pass.mean, 1, 1, 1);
                    l.ResourceBarrier(&[crate::host::transition(&pass.dome, written, common)]);
                } else {
                    run(&pass.mean, 1, 1, 1);
                }
                KEPT.store(true, Ordering::Relaxed);
            }
            Role::Air => run(
                &pass.air,
                views.size[0].div_ceil(8),
                views.size[1].div_ceil(8),
                views.slices.div_ceil(2),
            ),
            Role::Reflections => run(
                &pass.hold,
                views.size[0].div_ceil(8),
                views.size[1].div_ceil(8),
                1,
            ),
            Role::Resolve => {
                if constant == 0.0 {
                    run(&pass.place, 8, 9, 1);
                    run(&pass.gather, 20, 45, 1);
                    GATHERED.store(true, Ordering::Relaxed);
                }
                run(
                    &pass.raise,
                    views.size[0].div_ceil(8),
                    views.size[1].div_ceil(8),
                    1,
                );
                for (i, other) in views.others.iter().enumerate() {
                    if others_set() == 2 {
                        let paint = [[4.0f32, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 4.0]][i % 3].map(f32::to_bits);
                        l.SetComputeRoot32BitConstants(0, 3, paint.as_ptr().cast(), 18);
                    }
                    l.SetComputeRootDescriptorTable(2, other.1);
                    run(
                        &pass.raise,
                        views.size[0].div_ceil(8),
                        views.size[1].div_ceil(8),
                        1,
                    );
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings() {
        assert_eq!(parse(""), Set::Off);
        assert_eq!(parse("off"), Set::Off);
        assert_eq!(parse("0"), Set::Off);
        assert_eq!(parse("0.8"), Set::Estimate(0.8));
        assert_eq!(parse("constant:0.3"), Set::Constant(0.3));
        assert_eq!(parse("constant:nan"), Set::Off);
    }

    #[test]
    fn minimum_floor() {
        assert_eq!(parse_min(""), 0.0);
        assert_eq!(parse_min("-1"), 0.0);
        assert_eq!(parse_min("nan"), 0.0);
        assert_eq!(parse_min("0.08"), 0.08);
    }

    #[test]
    fn reflections_held() {
        assert_eq!(parse_reflect(""), REFLECT);
        assert_eq!(parse_reflect("0"), [0.0, REFLECT[1]]);
        assert_eq!(parse_reflect("2,0.25"), [2.0, 0.25]);
        assert_eq!(parse_reflect("-1,nan"), REFLECT);
    }

    #[test]
    fn the_sky() {
        assert_eq!(parse_sky(""), Some(SKY_SET));
        assert_eq!(parse_sky("off"), None);
        assert_eq!(parse_sky("0,0"), None);
        assert_eq!(parse_sky("0.5,,0.3"), Some([0.5, SKY_SET[1], 0.3]));
        assert!(draws_sky(b"..ps_sky_paraboloid_reflection_1c.."));
        assert!(!draws_sky(b"ps_sky"));
    }

    #[test]
    fn roles_by_name() {
        assert_eq!(
            role(b"..CS_ScreenProbes_TemporalResolve_a911b49c_Wrapped.."),
            Some(Role::Resolve)
        );
        assert_eq!(
            role(b"..CS_CacheScreenSpaceRadiance_eed2fc35.."),
            Some(Role::Radiance)
        );
        assert_eq!(
            role(b"CS_AccumulateLocalLightInscatteringVolume_7e2f5eef"),
            Some(Role::Air)
        );
        assert_eq!(role(b"..CS_UpdateHistory_5f1c.."), Some(Role::Reflections));
        assert_eq!(role(b"CS_TemporalResolve"), None);
    }

    /// The passes compile with the system's compiler and the host takes them.
    #[test]
    fn the_passes_build() {
        let Some(host) = host() else {
            return;
        };
        make(&host.device, |bytes| host.uav_buffer(bytes)).expect("the passes");
    }
}
