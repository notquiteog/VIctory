//! What a temporal upscaler is given each frame and which one runs, apart
//! from whose library does the work: the title hands these inputs to its
//! AMD upscaler interface (`victory-hle`'s FidelityFX module, the hook),
//! and a backend records the upscale into the same host list. Two
//! backends: NVIDIA DLSS ([`crate::streamline`]) and AMD FidelityFX (the
//! user's PC build of the library the title itself calls, which takes the
//! title's own description unchanged).
//!
//! `VICTORY_UPSCALER` picks: `title` (the title's own code, as without any
//! of this), `dlss`, `fsr`, or `auto` (DLSS on an NVIDIA host with
//! `VICTORY_DLSS_DIR`, else FidelityFX with `VICTORY_FIDELITYFX_DIR`, else
//! the title's). `VICTORY_FRAMEGEN` picks frame generation the same way:
//! `dlss`, `fsr` or `off`. `VICTORY_UPSCALE_QUALITY` is the quality mode of
//! either.

use std::sync::OnceLock;

use windows::Win32::Graphics::Direct3D12::{ID3D12GraphicsCommandList, ID3D12Resource};

/// Whose upscaler serves the title's (`VICTORY_UPSCALER`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    /// The variable is unset (or names the title's picker value, a number:
    /// `title_patch`): as before it existed, FidelityFX if its folder is
    /// named, else the title's own.
    Unset,
    Title,
    Dlss,
    Fsr,
    Auto,
}

impl Choice {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "title" | "own" | "off" => Choice::Title,
            "dlss" | "nvidia" => Choice::Dlss,
            "fsr" | "fidelityfx" | "amd" => Choice::Fsr,
            "auto" => Choice::Auto,
            _ => return None,
        })
    }
}

/// What `VICTORY_UPSCALER` asks for.
pub fn asked() -> Choice {
    static ASKED: OnceLock<Choice> = OnceLock::new();
    *ASKED.get_or_init(|| match std::env::var("VICTORY_UPSCALER") {
        Ok(text) if text.trim().is_empty() || text.trim().parse::<u32>().is_ok() => Choice::Unset,
        Ok(text) => Choice::parse(&text).unwrap_or_else(|| {
            tracing::warn!(text, "VICTORY_UPSCALER wants title, dlss, fsr or auto; title");
            Choice::Title
        }),
        Err(_) => Choice::Unset,
    })
}

/// Whose frame generation runs (`VICTORY_FRAMEGEN`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameGeneration {
    Off,
    Dlss,
    Fsr,
}

/// What `VICTORY_FRAMEGEN` asks for (`VICTORY_DLSS_FG=1` is `dlss`); off
/// without it.
pub fn asked_frame_generation() -> FrameGeneration {
    static ASKED: OnceLock<FrameGeneration> = OnceLock::new();
    *ASKED.get_or_init(|| match std::env::var("VICTORY_FRAMEGEN").map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        Ok("dlss" | "nvidia") => FrameGeneration::Dlss,
        Ok("fsr" | "fidelityfx" | "amd") => FrameGeneration::Fsr,
        Ok("off" | "0" | "") => FrameGeneration::Off,
        Ok(text) => {
            tracing::warn!(text, "VICTORY_FRAMEGEN wants dlss, fsr or off; off");
            FrameGeneration::Off
        }
        Err(_) if std::env::var_os("VICTORY_DLSS_FG").is_some_and(|v| v != "0") => FrameGeneration::Dlss,
        Err(_) => FrameGeneration::Off,
    })
}

/// A quality mode (`VICTORY_UPSCALE_QUALITY`): how much smaller than the
/// output the title renders.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// Native resolution, anti-aliasing only (DLAA, FSR native AA).
    Native,
    Quality,
    Balanced,
    Performance,
    UltraPerformance,
}

impl Quality {
    pub const ALL: [Quality; 5] =
        [Quality::Native, Quality::Quality, Quality::Balanced, Quality::Performance, Quality::UltraPerformance];

    /// `native` (or `dlaa`), `quality`, `balanced`, `performance`, `ultra`.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "dlaa" | "native" | "nativeaa" => Quality::Native,
            "quality" => Quality::Quality,
            "balanced" => Quality::Balanced,
            "performance" => Quality::Performance,
            "ultra" | "ultra-performance" | "ultraperformance" => Quality::UltraPerformance,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Quality::Native => "native",
            Quality::Quality => "quality",
            Quality::Balanced => "balanced",
            Quality::Performance => "performance",
            Quality::UltraPerformance => "ultra",
        }
    }

    /// The render size over the output size, per axis, as DLSS has it.
    pub fn dlss_scale(self) -> f32 {
        match self {
            Quality::Native => 1.0,
            Quality::Quality => 2.0 / 3.0,
            Quality::Balanced => 0.58,
            Quality::Performance => 0.5,
            Quality::UltraPerformance => 1.0 / 3.0,
        }
    }

    /// The same as AMD's upscaler has it (its ratios 1, 1.5, 1.7, 2, 3).
    pub fn fsr_scale(self) -> f32 {
        1.0 / self.fsr_ratio()
    }

    /// AMD's upscaler's ratio: output over render, per axis.
    pub fn fsr_ratio(self) -> f32 {
        match self {
            Quality::Native => 1.0,
            Quality::Quality => 1.5,
            Quality::Balanced => 1.7,
            Quality::Performance => 2.0,
            Quality::UltraPerformance => 3.0,
        }
    }

    /// The mode whose DLSS ratio is nearest `scale`.
    pub fn nearest(scale: f32) -> Self {
        let mut best = Quality::Quality;
        for m in Quality::ALL {
            if (m.dlss_scale() - scale).abs() < (best.dlss_scale() - scale).abs() {
                best = m;
            }
        }
        best
    }
}

/// The mode `VICTORY_UPSCALE_QUALITY` asks for (`VICTORY_DLSS_MODE` is the
/// same); `None` without either.
pub fn asked_quality() -> Option<Quality> {
    static ASKED: OnceLock<Option<Quality>> = OnceLock::new();
    *ASKED.get_or_init(|| {
        let text = std::env::var("VICTORY_UPSCALE_QUALITY").or_else(|_| std::env::var("VICTORY_DLSS_MODE")).ok()?;
        let parsed = Quality::parse(&text);
        if parsed.is_none() {
            tracing::warn!(text, "VICTORY_UPSCALE_QUALITY wants native, quality, balanced, performance or ultra; ignored");
        }
        parsed
    })
}

/// A host texture and the `D3D12_RESOURCE_STATES` it is in at the upscale
/// (and is left in).
pub type Texture<'a> = (&'a ID3D12Resource, u32);

/// One frame's upscale: what the title hands its upscaler.
pub struct Frame<'a> {
    /// The jittered, lit frame at render size (before tone mapping if
    /// `hdr`).
    pub color: Texture<'a>,
    pub depth: Texture<'a>,
    pub motion: Texture<'a>,
    /// A 1x1 exposure texture, if the title has one and does not ask for
    /// automatic exposure.
    pub exposure: Option<Texture<'a>>,
    /// Where the upscaled frame goes, at output size.
    pub output: Texture<'a>,
    pub render: [u32; 2],
    pub output_size: [u32; 2],
    /// The projection's sub-pixel offset in render pixels.
    pub jitter: [f32; 2],
    /// What a motion vector is multiplied by to give render pixels.
    pub motion_scale: [f32; 2],
    /// Motion vectors at output size, not render size.
    pub motion_at_output: bool,
    /// Motion vectors that still have the jitter in them.
    pub motion_jittered: bool,
    pub depth_inverted: bool,
    pub hdr: bool,
    pub auto_exposure: bool,
    pub pre_exposure: f32,
    /// The frame has nothing to do with the last one (a camera cut).
    pub reset: bool,
    /// The camera: planes, vertical field of view in radians.
    pub near: f32,
    pub far: f32,
    pub fov_y: f32,
    /// Since the last frame, milliseconds.
    pub frame_time_ms: f32,
}

/// A vendor's upscaler on the host device.
pub trait Upscaler: Sync {
    fn name(&self) -> &'static str;

    /// Records the upscale of `frame` into `list` (open, ours). The list's
    /// descriptor heaps, root signatures and pipeline are not ours any more
    /// afterwards; the textures are left in the states they were given in.
    fn upscale(&self, list: &ID3D12GraphicsCommandList, frame: &Frame<'_>) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_and_modes_parse() {
        assert_eq!(Choice::parse(" DLSS "), Some(Choice::Dlss));
        assert_eq!(Choice::parse("fsr"), Some(Choice::Fsr));
        assert_eq!(Choice::parse("7"), None);
        assert_eq!(Quality::parse(" Quality "), Some(Quality::Quality));
        assert_eq!(Quality::parse("dlaa"), Some(Quality::Native));
        assert_eq!(Quality::parse("ultra"), Some(Quality::UltraPerformance));
        assert_eq!(Quality::parse("fast"), None);
    }

    #[test]
    fn modes_have_each_vendors_ratios() {
        assert_eq!(Quality::nearest(0.5), Quality::Performance);
        assert_eq!(Quality::nearest(0.66), Quality::Quality);
        assert_eq!(Quality::nearest(0.95), Quality::Native);
        assert_eq!((3840.0 * Quality::Quality.dlss_scale()) as u32, 2560);
        assert!((Quality::Balanced.fsr_scale() - 0.588).abs() < 1e-3);
        assert_eq!(Quality::Performance.fsr_scale(), Quality::Performance.dlss_scale());
    }
}
