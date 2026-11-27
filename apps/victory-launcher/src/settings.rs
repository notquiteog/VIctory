//! What the user chose, kept in a file a person can read
//! (`%LOCALAPPDATA%\VIctory\settings.toml`), and the environment a run of
//! `victory.exe` is given for it.
//!
//! Keys this version does not know are skipped and words it does not know
//! fall back to the default: an older or newer file never stops a launch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// An enum written as a word; a word that is not known reads as the default.
macro_rules! word_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $variant:ident = $word:literal => $label:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub enum $name { $($(#[$vm])* $variant),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            /// As the settings file has it.
            pub fn word(self) -> &'static str { match self { $($name::$variant => $word),+ } }
            /// As the window shows it.
            pub fn label(self) -> &'static str { match self { $($name::$variant => $label),+ } }
            pub fn from_word(word: &str) -> Self {
                let word = word.trim().to_ascii_lowercase();
                $(if word == $word { return $name::$variant; })+
                Self::default()
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.word()) }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Ok(Self::from_word(&String::deserialize(d)?))
            }
        }
    };
}

word_enum! {
    /// The console model the title is told it runs on (`run --console`).
    /// Series S is `scripts/play.ps1`'s default: the one measured fastest.
    Console {
        #[default]
        SeriesS = "series-s" => "Xbox Series S: 1080p picture, fastest (recommended)",
        SeriesX = "series-x" => "Xbox Series X: 2160p picture, sharper, about half as fast",
    }
}

impl Console {
    /// The height of the picture the title makes on this console.
    pub fn output_height(self) -> u32 {
        match self {
            Console::SeriesS => 1080,
            Console::SeriesX => 2160,
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Console::SeriesS => "Series S",
            Console::SeriesX => "Series X",
        }
    }
}

word_enum! {
    /// The height the world is drawn at before the title's own upscaler
    /// (`play.ps1 -Resolution`): `VICTORY_RENDER_SCALE` is it over the
    /// console's output height.
    Resolution {
        #[default]
        Half = "half" => "Half the picture (recommended)",
        R2160 = "2160" => "2160p",
        R1440 = "1440" => "1440p",
        R1080 = "1080" => "1080p",
        R720 = "720" => "720p",
        R540 = "540" => "540p",
        R480 = "480" => "480p",
    }
}

impl Resolution {
    /// The height, `None` for half the output.
    pub fn height(self) -> Option<u32> {
        match self {
            Resolution::Half => None,
            Resolution::R2160 => Some(2160),
            Resolution::R1440 => Some(1440),
            Resolution::R1080 => Some(1080),
            Resolution::R720 => Some(720),
            Resolution::R540 => Some(540),
            Resolution::R480 => Some(480),
        }
    }

    /// Whether the title can be drawn at this height on `console`: not
    /// above its output, and Series S not at its full 1080 (the title stops
    /// at a breakpoint of its own there; `play.ps1` refuses it too).
    pub fn fits(self, console: Console) -> bool {
        match self.height() {
            None => true,
            Some(h) => h <= console.output_height() && !(console == Console::SeriesS && h == 1080),
        }
    }

    /// The height the world is drawn at on `console` (an unfitting choice
    /// counts as half).
    pub fn drawn_height(self, console: Console) -> u32 {
        match self.height() {
            Some(h) if self.fits(console) => h,
            _ => console.output_height() / 2,
        }
    }

    /// `VICTORY_RENDER_SCALE`, written as `play.ps1` writes it (`0.####`).
    pub fn render_scale(self, console: Console) -> String {
        let scale = f64::from(self.drawn_height(console)) / f64::from(console.output_height());
        let text = format!("{scale:.4}");
        let text = text.trim_end_matches('0').trim_end_matches('.');
        if text.is_empty() {
            "0".to_owned()
        } else {
            text.to_owned()
        }
    }
}

word_enum! {
    /// Compiling the host pipelines ahead (`play.ps1`'s default, `-WarmAll`
    /// and `-NoWarm`).
    WarmMode {
        #[default]
        Used = "used" => "Those earlier sessions used before each start, the rest after the game closes (recommended)",
        All = "all" => "Every pipeline the game has created, before each start (long the first time)",
        Off = "off" => "Never (the game stutters while it compiles them)",
    }
}

word_enum! {
    Preset {
        #[default]
        Recommended = "recommended" => "Recommended",
        Safe = "safe" => "Safe",
        Custom = "custom" => "Custom",
    }
}

word_enum! {
    Upscaler {
        #[default]
        Game = "game" => "Game's own",
        Dlss = "dlss" => "NVIDIA DLSS",
        Fsr = "fsr" => "AMD FSR",
    }
}

word_enum! {
    Quality {
        Native = "native" => "Native (no upscaling, anti-aliasing only)",
        #[default]
        High = "quality" => "Quality",
        Balanced = "balanced" => "Balanced",
        Performance = "performance" => "Performance",
        Ultra = "ultra" => "Ultra performance",
    }
}

word_enum! {
    FrameGen {
        #[default]
        Off = "off" => "Off",
        Dlss = "dlss" => "NVIDIA DLSS frame generation",
        Fsr = "fsr" => "AMD FSR frame generation",
    }
}

word_enum! {
    /// Which releases the updater follows.
    Channel {
        Stable = "stable" => "Stable (latest release)",
        #[default]
        Nightly = "nightly" => "Latest build (newest commit on main)",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The user's own copy of the game (never copied, moved or changed).
    pub game_dir: String,
    /// Empty: `%LOCALAPPDATA%\VIctory\cache`.
    pub cache_dir: String,
    /// Empty: `%LOCALAPPDATA%\VIctory\data` (the saves).
    pub data_dir: String,
    /// The first-run page was gone through.
    pub setup_done: bool,
    /// Which defaults the file was last brought up to ([`DEFAULTS`]); a
    /// file without it is of 0.1.0.
    #[serde(default)]
    pub defaults: u32,
    pub console: Console,
    /// The height the world is drawn at (`play.ps1 -Resolution`).
    pub resolution: Resolution,
    /// The title's own ray tracing (`play.ps1 -NoRayTracing` when off).
    pub ray_tracing: bool,
    /// Ray tracing on the card's ray-tracing cores instead of in shaders
    /// (`play.ps1 -HardwareRT`, `VICTORY_RT_HARDWARE=1`; docs/hardware-rt.md).
    /// Work in progress, off by default.
    pub ray_tracing_hardware: bool,
    /// Part of a graphics card's name or its index (`VICTORY_D3D12_ADAPTER`);
    /// empty: the system's first.
    pub adapter: String,
    /// NVIDIA: start a copy named `victory_sm.exe`, for a Smooth Motion
    /// profile set for that name in the NVIDIA App (`play.ps1 -SmoothMotion`).
    pub smooth_motion: bool,
    /// Compiling pipelines ahead around each session.
    pub warm: WarmMode,
    pub preset: Preset,
    /// `VICTORY_AMBIENT`: 0 leaves the picture as the game makes it.
    pub ambient: f32,
    pub upscaler: Upscaler,
    pub dlss_dir: String,
    pub fidelityfx_dir: String,
    pub quality: Quality,
    pub frame_generation: FrameGen,
    /// Lines of `NAME=value` given to the game as they are.
    pub extra_env: String,
    pub updates: Updates,
    pub prepared: Prepared,
    pub developer: Developer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    /// Look for a new version when the launcher starts (once a day at most).
    pub check_on_start: bool,
    /// When it last looked (seconds since 1970).
    pub last_check: u64,
    pub channel: Channel,
    /// The channel the installed program came from (set when an update is
    /// put in place): from a latest build, Stable offers the release again.
    pub installed_from: Channel,
}

/// Which `victory.exe` the caches were last prepared with.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prepared {
    pub shaders_build: String,
    pub shaders_when: u64,
    pub pipelines_build: String,
    pub pipelines_when: u64,
    /// An update replaced the program since: both may be out of date.
    pub possibly_stale: bool,
}

/// Not shown in the window: for development and tests.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Developer {
    /// A `victory.exe` elsewhere than beside the launcher (or the
    /// `victory-stub.exe` stand-in, to try the launcher without the game).
    pub victory_exe: String,
    /// Another place to look for releases: a URL of release JSON, or a
    /// folder with `latest.json` (`VICTORY_UPDATE_FEED` goes first).
    pub update_feed: String,
}

impl Default for Updates {
    fn default() -> Self {
        Updates {
            check_on_start: true,
            last_check: 0,
            channel: Channel::Nightly,
            installed_from: Channel::Stable,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            game_dir: String::new(),
            cache_dir: String::new(),
            data_dir: String::new(),
            setup_done: false,
            defaults: DEFAULTS,
            console: Console::default(),
            resolution: Resolution::default(),
            ray_tracing: true,
            ray_tracing_hardware: false,
            adapter: String::new(),
            smooth_motion: false,
            warm: WarmMode::default(),
            preset: Preset::default(),
            ambient: 0.8,
            upscaler: Upscaler::default(),
            dlss_dir: String::new(),
            fidelityfx_dir: String::new(),
            quality: Quality::default(),
            frame_generation: FrameGen::default(),
            extra_env: String::new(),
            updates: Updates::default(),
            prepared: Prepared::default(),
            developer: Developer::default(),
        }
    }
}

const HEADER: &str =
    "# VI launcher settings. Edit in the launcher (Settings tab) or here while it is closed.\n\n";

/// The defaults this version has: 2 is Series S at half resolution with ray
/// tracing, as `scripts/play.ps1` (0.1.0 had Series X); 3 is updates from
/// the Latest build channel, rebuilt from every change (Stable before).
pub const DEFAULTS: u32 = 3;

impl Settings {
    /// Takes a file written by an older launcher to this one's defaults
    /// where it only held the old default; says so, if anything changed.
    pub fn bring_up_to_date(&mut self) -> Option<String> {
        if self.defaults >= DEFAULTS {
            return None;
        }
        let was = self.defaults;
        self.defaults = DEFAULTS;
        let mut notes = Vec::new();
        // 0.1.0 wrote its default console (Series X) into every file.
        if was < 2 && self.console == Console::SeriesX {
            self.console = Console::SeriesS;
            notes.push("The recommended settings changed: the game now starts as an Xbox Series S (1080p picture, the world drawn at 540p, ray tracing on), the fastest measured. Series X can be chosen again under Settings.");
        }
        // Files before 3 hold Stable, the old default channel.
        if was < 3 && self.updates.channel == Channel::Stable {
            self.updates.channel = Channel::Nightly;
            notes.push("Updates now come from the Latest build channel, rebuilt from every change, so fixes reach you as soon as they are made. Stable can be chosen again under Updates.");
        }
        (!notes.is_empty()).then(|| notes.join("\n\n"))
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn to_toml(&self) -> String {
        format!(
            "{HEADER}{}",
            toml::to_string_pretty(self).unwrap_or_default()
        )
    }

    /// The saved settings, or the defaults; with what went wrong, if a file
    /// was there and could not be read (it is kept beside as `.unreadable`).
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("Could not read {}: {e}", path.display())),
                );
            }
        };
        match Self::from_toml(&text) {
            Ok(mut s) => {
                let note = s.bring_up_to_date();
                (s, note)
            }
            Err(e) => {
                let kept = path.with_extension("toml.unreadable");
                let _ = std::fs::copy(path, &kept);
                (
                    Self::default(),
                    Some(format!(
                        "The settings file {} could not be understood ({}). Default settings are used; the old file was kept as {}.",
                        path.display(),
                        e.lines().next().unwrap_or_default(),
                        kept.display()
                    )),
                )
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        }
        let partial = path.with_extension("toml.partial");
        std::fs::write(&partial, self.to_toml())
            .map_err(|e| format!("Could not write {}: {e}", partial.display()))?;
        std::fs::rename(&partial, path)
            .map_err(|e| format!("Could not write {}: {e}", path.display()))
    }

    pub fn cache_path(&self) -> PathBuf {
        match folder(&self.cache_dir) {
            "" => paths::home().join("cache"),
            dir => PathBuf::from(dir),
        }
    }

    pub fn data_path(&self) -> PathBuf {
        match folder(&self.data_dir) {
            "" => paths::home().join("data"),
            dir => PathBuf::from(dir),
        }
    }

    pub fn game_exe(&self) -> PathBuf {
        Path::new(folder(&self.game_dir)).join(paths::GAME_EXE)
    }

    pub fn shader_archive(&self) -> PathBuf {
        Path::new(folder(&self.game_dir)).join(paths::GAME_SHADERS)
    }
}

/// A folder as typed or pasted: without the spaces and quotes around it
/// (Explorer's "Copy as path" adds quotes).
pub fn folder(text: &str) -> &str {
    text.trim().trim_matches('"').trim()
}

/// What is wrong with a game folder, in words for the user; `None` if it
/// has what a run needs. (The window asks every frame: an answer is kept
/// for a second.)
pub fn game_folder_problem(dir: &str) -> Option<String> {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    thread_local! {
        static LAST: RefCell<Option<(String, Instant, Option<String>)>> = const { RefCell::new(None) };
    }
    if cfg!(test) {
        return look_at_game_folder(dir);
    }
    LAST.with_borrow_mut(|last| {
        if let Some((d, when, answer)) = last
            && d == dir
            && when.elapsed() < Duration::from_secs(1)
        {
            return answer.clone();
        }
        let answer = look_at_game_folder(dir);
        *last = Some((dir.to_owned(), Instant::now(), answer.clone()));
        answer
    })
}

fn look_at_game_folder(dir: &str) -> Option<String> {
    let dir = folder(dir);
    if dir.is_empty() {
        return Some(
            "No game folder chosen yet. Choose the folder of your own Xbox Series copy of the game."
                .to_owned(),
        );
    }
    let path = Path::new(dir);
    if path.is_file() {
        return Some(format!(
            "{dir} is a file. Choose the folder it is in, the one that holds {}.",
            paths::GAME_EXE
        ));
    }
    if !path.is_dir() {
        return Some(format!(
            "The folder {dir} was not found. If it is on a removable or network drive, connect it; otherwise choose the folder again."
        ));
    }
    if let Err(e) = std::fs::read_dir(path) {
        return Some(format!("The folder {dir} cannot be read ({e})."));
    }
    let missing: Vec<&str> = [paths::GAME_EXE, paths::GAME_SHADERS]
        .into_iter()
        .filter(|name| !path.join(name).is_file())
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut text = format!(
        "{dir} does not look like the game: {} {} missing. Choose the folder of your own Xbox Series copy, the one that holds {}.",
        missing.join(" and "),
        if missing.len() == 1 { "is" } else { "are" },
        paths::GAME_EXE
    );
    if let Some(found) = game_folder_below(path) {
        text.push_str(&format!(" It seems to be in {}.", found.display()));
    }
    Some(text)
}

/// A folder up to two levels below `dir` that holds the game's program and
/// its shader archive: where the game is when its parent was chosen.
pub fn game_folder_below(dir: &Path) -> Option<PathBuf> {
    let holds =
        |p: &Path| p.join(paths::GAME_EXE).is_file() && p.join(paths::GAME_SHADERS).is_file();
    let subdirs = |p: &Path| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(p)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .take(200)
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        dirs
    };
    for child in subdirs(dir) {
        if holds(&child) {
            return Some(child);
        }
        if let Some(grandchild) = subdirs(&child).into_iter().find(|g| holds(g)) {
            return Some(grandchild);
        }
    }
    None
}

/// What every preset but Custom sets: `scripts/play.ps1`'s own set, for
/// AMD and NVIDIA alike.
const BASE: &[(&str, &str)] = &[
    ("VICTORY_TEXTURE_NO_WAIT", "1"),
    ("VICTORY_RELAY", "2"),
    ("VICTORY_WIDE_BACK", "1"),
    ("VICTORY_CARD_REHASH_RUNS", "1"),
    ("VICTORY_JOIN_COPIES", "1"),
    ("VICTORY_INDIRECT_WINDOW", "1"),
    ("VICTORY_RELAY_COPY", "1"),
    ("VICTORY_CARD_STEPS", "2"),
    ("VICTORY_INDIRECT_BATCH", "2"),
    ("VICTORY_MAP_THREADS", "1"),
    ("VICTORY_TITLE_PRIORITIES", "0"),
    ("VICTORY_CARD_AWAKE", "1"),
    ("VICTORY_SHOW_AFTER", "1"),
    ("VICTORY_AMBIENT_MIN", "0.08"),
    // (A light record of what the card did, for the log if it fails.)
    ("VICTORY_D3D12_DRED", "light"),
    ("VICTORY_SKIP_PIPELINES", "none"),
    // Runs write down the pipelines the game makes: what warming up works from.
    ("VICTORY_PIPELINE_BOOK", "1"),
    // The title's own temporal upscaler on every profile (Series S would
    // pick FSR, whose programs are not translated: white blocks).
    ("VICTORY_OWN_UPSCALER", "1"),
    // Series S presents the interface as a second plane: laid over the game.
    ("VICTORY_COMPOSE", "1"),
];
/// Recommended adds the newer, faster submission paths ...
const RECOMMENDED: &[(&str, &str)] = &[("VICTORY_THIN", "1"), ("VICTORY_JOIN_PARTS", "8")];
/// ... which Safe (`play.ps1 -Safe`) goes without, and two more besides.
const SAFE: &[(&str, &str)] = &[("VICTORY_UNDER_WIDE", "0"), ("VICTORY_LAND_ECHO", "0")];
/// The passes that would trace rays, skipped when ray tracing is off
/// (`play.ps1 -NoRayTracing`; shader hashes, nothing of the game).
const RT_OFF_HASHES: &str = include_str!("../../../scripts/rt_off_hashes.txt");
/// Never set by the launcher itself (the Advanced box can, knowingly).
#[cfg(test)]
const DEVELOPER_AIDS: &[&str] = &[
    "VICTORY_KEYS",
    "VICTORY_CAPTURE",
    "VICTORY_PROFILE",
    "VICTORY_AUDIO_CAPTURE",
];

/// The `NAME=value` lines of the Advanced box. Lines that are not of that
/// form are returned apart, to be shown, and are otherwise skipped.
pub fn parse_extra_env(text: &str) -> (Vec<(String, String)>, Vec<String>) {
    let (mut good, mut bad) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `$env:NAME = 'value'` and `set NAME=value` are what people paste.
        let line = line.strip_prefix("$env:").unwrap_or(line);
        let line = line.strip_prefix("set ").unwrap_or(line);
        match line.split_once('=') {
            Some((name, value)) => {
                let name = name.trim();
                let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                let name_ok =
                    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if name_ok {
                    good.push((name.to_owned(), value.to_owned()));
                } else {
                    bad.push(line.to_owned());
                }
            }
            None => bad.push(line.to_owned()),
        }
    }
    (good, bad)
}

fn number(v: f32) -> String {
    let text = format!("{v:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// The `VICTORY_*` environment of a run with these settings, as
/// `scripts/play.ps1` builds it: the three folders, the preset, the
/// console's render scale, ray tracing, the card, the picture choices, then
/// the Advanced lines (which win, as `-More` does). Sorted by name.
pub fn build_env(s: &Settings) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut set = |name: &str, value: String| {
        env.insert(name.to_owned(), value);
    };
    if !folder(&s.game_dir).is_empty() {
        set("VICTORY_GAME_DIR", folder(&s.game_dir).to_owned());
    }
    set("VICTORY_CACHE_DIR", s.cache_path().display().to_string());
    set("VICTORY_DATA_DIR", s.data_path().display().to_string());
    let vendor_upscaler = s.upscaler != Upscaler::Game;
    if s.preset != Preset::Custom {
        for &(name, value) in BASE {
            if vendor_upscaler && name == "VICTORY_OWN_UPSCALER" {
                // The vendor's upscaler takes the title's FSR call: the
                // title must make it, at the scale of the quality mode.
                continue;
            }
            set(name, value.to_owned());
        }
        let extra = if s.preset == Preset::Safe {
            SAFE
        } else {
            RECOMMENDED
        };
        for &(name, value) in extra {
            set(name, value.to_owned());
        }
        set("VICTORY_AMBIENT", number(s.ambient.clamp(0.0, 4.0)));
    }
    if !vendor_upscaler {
        set("VICTORY_RENDER_SCALE", s.resolution.render_scale(s.console));
    }
    if s.ray_tracing && s.ray_tracing_hardware {
        set("VICTORY_RT_HARDWARE", "1".to_owned());
    }
    if !s.ray_tracing {
        set("VICTORY_BVH_BUILDER", "0".to_owned());
        let hashes = RT_OFF_HASHES.trim();
        if !hashes.is_empty() {
            set("VICTORY_SKIP_PIPELINES", hashes.to_owned());
        }
    }
    if !s.adapter.trim().is_empty() {
        set("VICTORY_D3D12_ADAPTER", s.adapter.trim().to_owned());
    }
    // The vendors' upscalers: the user's own copies of their libraries.
    match s.upscaler {
        Upscaler::Game => {}
        Upscaler::Dlss => {
            set("VICTORY_UPSCALER", "dlss".to_owned());
            set("VICTORY_UPSCALE_QUALITY", s.quality.word().to_owned());
        }
        Upscaler::Fsr => {
            set("VICTORY_UPSCALER", "fsr".to_owned());
            set("VICTORY_UPSCALE_QUALITY", s.quality.word().to_owned());
        }
    }
    let wants_dlss = s.upscaler == Upscaler::Dlss || s.frame_generation == FrameGen::Dlss;
    let wants_fsr = s.upscaler == Upscaler::Fsr || s.frame_generation == FrameGen::Fsr;
    if wants_dlss && !folder(&s.dlss_dir).is_empty() {
        set("VICTORY_DLSS_DIR", folder(&s.dlss_dir).to_owned());
    }
    if wants_fsr && !folder(&s.fidelityfx_dir).is_empty() {
        set(
            "VICTORY_FIDELITYFX_DIR",
            folder(&s.fidelityfx_dir).to_owned(),
        );
    }
    if s.frame_generation != FrameGen::Off {
        set("VICTORY_FRAMEGEN", s.frame_generation.word().to_owned());
    }
    for (name, value) in parse_extra_env(&s.extra_env).0 {
        set(&name, value);
    }
    env
}

/// `Series S, the world drawn at 540p, ray tracing on`, as `play.ps1` says
/// it (from the environment, so that Advanced lines count).
pub fn describe(s: &Settings) -> String {
    let env = build_env(s);
    let out = s.console.output_height();
    let drawn = env
        .get("VICTORY_RENDER_SCALE")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|scale| {
            format!(
                "the world drawn at {}p",
                (f64::from(out) * scale).round() as u32
            )
        })
        .unwrap_or_else(|| "the world drawn at the upscaler's size".to_owned());
    let rays = if env.get("VICTORY_BVH_BUILDER").map(String::as_str) == Some("0") {
        "off"
    } else if env.get("VICTORY_RT_HARDWARE").map(String::as_str) == Some("1") {
        "on (hardware)"
    } else {
        "on"
    };
    format!("{}, {drawn}, ray tracing {rays}", s.console.short())
}

/// One line of help for each setting, shown under it.
pub mod help {
    pub const GAME_DIR: &str =
        "The folder of your own Xbox Series copy of the game. It is only read, never changed.";
    pub const CACHE_DIR: &str = "Translated shaders and prepared pipelines. Can be deleted at any time; needs tens of GB free.";
    pub const DATA_DIR: &str =
        "Your saved games and what the game stores between sessions. Keep this.";
    pub const CONSOLE: &str = "Which console the game believes it runs on. Series S makes a 1080p picture and is the fastest measured; Series X makes 2160p, sharper and about half as fast.";
    pub const RESOLUTION: &str = "The height the world is drawn at before the game's own upscaler enlarges it to the picture. Lower is faster and blurrier. Series S cannot be drawn at its full 1080p.";
    pub const RAY_TRACING: &str = "The game's own ray-traced lighting and reflections. Off is faster; a simpler light stands in for it.";
    pub const RAY_TRACING_HARDWARE: &str = "Where the rays are traced. Software: in the game's own shaders, as on the console (tested). Hardware: on the graphics card's ray-tracing cores (RTX cores and the like). Work in progress: today it only builds the card's own structures beside the game's, and the game's shaders still trace.";
    pub const ADAPTER: &str = "On a PC with more than one graphics card: the one to use.";
    pub const SMOOTH_MOTION: &str = "Starts the game under the name victory_sm.exe, so that a Smooth Motion profile you set for that name in the NVIDIA App applies (the driver's frame generation). Nothing is downloaded.";
    pub const WARM: &str = "Compiling the graphics card's pipelines ahead, so the game does not stutter making them. What is done once is kept; stopping it leaves the rest for next time.";
    pub const PRESET: &str = "Recommended: the fastest tested settings. Safe: without the newest speed-ups, the first thing to try if the game misbehaves. Custom: only the choices above and what you type under Advanced.";
    pub const AMBIENT: &str = "Brightens rooms that have little direct light. 0 is the game's own picture; 0.8 is the tested level.";
    pub const UPSCALER: &str = "Who enlarges the picture to your screen: the game's own code, NVIDIA DLSS or AMD FSR (you supply the vendor's files).";
    pub const VENDOR_DIR: &str = "The folder holding the vendor's own DLL files on your PC. VIctory does not ship or download them.";
    pub const QUALITY: &str =
        "Higher quality draws more pixels itself; Performance draws fewer and enlarges more.";
    pub const FRAMEGEN: &str = "Lets the graphics card insert in-between frames for smoother motion. Needs a card and files that support it.";
    pub const EXTRA_ENV: &str = "One NAME=value per line, given to the game as it is. For switches this window has no control for.";
    pub const CHANNEL: &str = "Stable: tested releases. Latest build: rebuilt from every change, newest fixes first, may break.";
    pub const CHECK_UPDATES: &str = "Asks GitHub for the newest release when the launcher starts, at most once a day. Nothing about you is sent.";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_game(preset: Preset) -> Settings {
        Settings {
            game_dir: r"D:\Games\MyCopy".into(),
            cache_dir: r"D:\VIctory\cache".into(),
            data_dir: r"D:\VIctory\data".into(),
            preset,
            ..Settings::default()
        }
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            game_dir: r"D:\Games\My Copy".into(),
            console: Console::SeriesX,
            resolution: Resolution::R1440,
            ray_tracing: false,
            adapter: "RTX".into(),
            smooth_motion: true,
            warm: WarmMode::All,
            preset: Preset::Safe,
            ambient: 0.5,
            upscaler: Upscaler::Dlss,
            dlss_dir: r"D:\sdk\streamline".into(),
            quality: Quality::Performance,
            frame_generation: FrameGen::Fsr,
            extra_env: "VICTORY_PANEL=1\nVICTORY_X=two words".into(),
            updates: Updates {
                check_on_start: false,
                last_check: 1_795_000_000,
                channel: Channel::Nightly,
                installed_from: Channel::Nightly,
            },
            prepared: Prepared {
                shaders_build: "0.1.0 abc".into(),
                shaders_when: 5,
                possibly_stale: true,
                ..Default::default()
            },
            ..Settings::default()
        };
        let text = s.to_toml();
        assert!(text.contains("console = \"series-x\""), "{text}");
        assert!(text.contains("resolution = \"1440\""), "{text}");
        assert_eq!(Settings::from_toml(&text).unwrap(), s);
    }

    #[test]
    fn unknown_keys_and_words_do_not_break_loading() {
        let text = "game_dir = 'D:\\g'\nfuture_switch = 3\npreset = 'ludicrous'\nconsole = 'series-s'\n[updates]\nchannel = 'beta'\n[brand_new_table]\nx = 1\n";
        let s = Settings::from_toml(text).unwrap();
        assert_eq!(s.game_dir, r"D:\g");
        assert_eq!(s.preset, Preset::Recommended);
        assert_eq!(s.console, Console::SeriesS);
        assert!(s.updates.check_on_start);
    }

    #[test]
    fn a_broken_file_gives_defaults_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "game_dir = [this is not toml").unwrap();
        let (s, problem) = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(problem.unwrap().contains("settings.toml"));
        assert!(dir.path().join("settings.toml.unreadable").is_file());
        // And a missing one is no problem at all.
        let (_, problem) = Settings::load(&dir.path().join("none.toml"));
        assert!(problem.is_none());
    }

    #[test]
    fn save_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("settings.toml");
        let s = with_game(Preset::Safe);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), (s, None));
    }

    /// `scripts/play.ps1`'s environment with no flags (of 2026-11-26), the
    /// three folders apart.
    const PLAY_PS1_DEFAULT: &[(&str, &str)] = &[
        ("VICTORY_AMBIENT", "0.8"),
        ("VICTORY_AMBIENT_MIN", "0.08"),
        ("VICTORY_CARD_AWAKE", "1"),
        ("VICTORY_CARD_REHASH_RUNS", "1"),
        ("VICTORY_CARD_STEPS", "2"),
        ("VICTORY_COMPOSE", "1"),
        ("VICTORY_D3D12_DRED", "light"),
        ("VICTORY_INDIRECT_BATCH", "2"),
        ("VICTORY_INDIRECT_WINDOW", "1"),
        ("VICTORY_JOIN_COPIES", "1"),
        ("VICTORY_JOIN_PARTS", "8"),
        ("VICTORY_MAP_THREADS", "1"),
        ("VICTORY_OWN_UPSCALER", "1"),
        ("VICTORY_PIPELINE_BOOK", "1"),
        ("VICTORY_RELAY", "2"),
        ("VICTORY_RELAY_COPY", "1"),
        ("VICTORY_RENDER_SCALE", "0.5"),
        ("VICTORY_SHOW_AFTER", "1"),
        ("VICTORY_SKIP_PIPELINES", "none"),
        ("VICTORY_TEXTURE_NO_WAIT", "1"),
        ("VICTORY_THIN", "1"),
        ("VICTORY_TITLE_PRIORITIES", "0"),
        ("VICTORY_WIDE_BACK", "1"),
    ];

    fn without_folders(s: &Settings) -> Vec<(String, String)> {
        build_env(s)
            .into_iter()
            .filter(|(k, _)| {
                !["VICTORY_GAME_DIR", "VICTORY_CACHE_DIR", "VICTORY_DATA_DIR"].contains(&k.as_str())
            })
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = list
            .iter()
            .map(|&(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn the_defaults_are_the_play_script_with_no_flags() {
        let s = with_game(Preset::Recommended);
        assert_eq!(s.console, Console::SeriesS);
        assert!(s.ray_tracing);
        assert_eq!(without_folders(&s), pairs(PLAY_PS1_DEFAULT));
        let env = build_env(&s);
        assert_eq!(env["VICTORY_GAME_DIR"], r"D:\Games\MyCopy");
        assert_eq!(env["VICTORY_CACHE_DIR"], r"D:\VIctory\cache");
        assert_eq!(env["VICTORY_DATA_DIR"], r"D:\VIctory\data");
        for aid in DEVELOPER_AIDS {
            assert!(!env.contains_key(*aid), "{aid} must not be set by a preset");
        }
        assert_eq!(
            describe(&s),
            "Series S, the world drawn at 540p, ray tracing on"
        );
    }

    #[test]
    fn safe_is_the_play_script_with_safe() {
        let mut want = pairs(PLAY_PS1_DEFAULT);
        want.retain(|(k, _)| k != "VICTORY_THIN" && k != "VICTORY_JOIN_PARTS");
        want.push(("VICTORY_LAND_ECHO".into(), "0".into()));
        want.push(("VICTORY_UNDER_WIDE".into(), "0".into()));
        want.sort();
        assert_eq!(without_folders(&with_game(Preset::Safe)), want);
    }

    #[test]
    fn consoles_and_resolutions_give_the_play_scripts_render_scale() {
        let mut s = with_game(Preset::Recommended);
        let scale = |s: &Settings| build_env(s)["VICTORY_RENDER_SCALE"].clone();
        // (console, -Resolution, what play.ps1 sets)
        for (console, resolution, want) in [
            (Console::SeriesS, Resolution::Half, "0.5"),
            (Console::SeriesS, Resolution::R720, "0.6667"),
            (Console::SeriesS, Resolution::R540, "0.5"),
            (Console::SeriesS, Resolution::R480, "0.4444"),
            (Console::SeriesX, Resolution::Half, "0.5"),
            (Console::SeriesX, Resolution::R2160, "1"),
            (Console::SeriesX, Resolution::R1440, "0.6667"),
            (Console::SeriesX, Resolution::R1080, "0.5"),
            (Console::SeriesX, Resolution::R540, "0.25"),
            (Console::SeriesX, Resolution::R480, "0.2222"),
        ] {
            s.console = console;
            s.resolution = resolution;
            assert!(resolution.fits(console));
            assert_eq!(scale(&s), want, "{console:?} {resolution:?}");
        }
        // What play.ps1 refuses falls back to half here.
        for (console, resolution) in [
            (Console::SeriesS, Resolution::R1080),
            (Console::SeriesS, Resolution::R1440),
            (Console::SeriesS, Resolution::R2160),
        ] {
            assert!(!resolution.fits(console));
            s.console = console;
            s.resolution = resolution;
            assert_eq!(scale(&s), "0.5");
        }
        s.console = Console::SeriesX;
        s.resolution = Resolution::Half;
        assert_eq!(
            describe(&s),
            "Series X, the world drawn at 1080p, ray tracing on"
        );
        // `--console` is only given for Series S (Series X is victory's default).
        assert_eq!(Console::default(), Console::SeriesS);
    }

    #[test]
    fn hardware_ray_tracing_is_the_play_scripts_hardware_rt() {
        let mut s = Settings::default();
        assert!(!s.ray_tracing_hardware);
        assert!(!build_env(&s).contains_key("VICTORY_RT_HARDWARE"));
        s.ray_tracing_hardware = true;
        assert_eq!(build_env(&s)["VICTORY_RT_HARDWARE"], "1");
        assert!(describe(&s).ends_with("ray tracing on (hardware)"));
        // Without ray tracing there is nothing to trace on the card.
        s.ray_tracing = false;
        assert!(!build_env(&s).contains_key("VICTORY_RT_HARDWARE"));
    }

    #[test]
    fn ray_tracing_off_is_the_play_scripts_no_ray_tracing() {
        let mut s = with_game(Preset::Recommended);
        s.ray_tracing = false;
        let env = build_env(&s);
        assert_eq!(env["VICTORY_BVH_BUILDER"], "0");
        assert_eq!(env["VICTORY_SKIP_PIPELINES"], RT_OFF_HASHES.trim());
        assert!(env["VICTORY_SKIP_PIPELINES"].len() > 100);
        assert!(describe(&s).ends_with("ray tracing off"));
    }

    #[test]
    fn the_card_is_passed_by_name() {
        let mut s = with_game(Preset::Recommended);
        s.adapter = " RTX 5060 ".into();
        assert_eq!(build_env(&s)["VICTORY_D3D12_ADAPTER"], "RTX 5060");
    }

    #[test]
    fn custom_sets_only_folders_choices_and_what_is_typed() {
        let mut s = with_game(Preset::Custom);
        s.extra_env = "VICTORY_RELAY=1\n# a note\n\nnot a setting\n$env:VICTORY_PANEL = '1'\nset VICTORY_Q=\"a b\"\n9BAD NAME=1".into();
        let env = build_env(&s);
        let names: Vec<&str> = env.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "VICTORY_CACHE_DIR",
                "VICTORY_DATA_DIR",
                "VICTORY_GAME_DIR",
                "VICTORY_PANEL",
                "VICTORY_Q",
                "VICTORY_RELAY",
                "VICTORY_RENDER_SCALE",
            ]
        );
        assert_eq!(env["VICTORY_Q"], "a b");
        let (_, bad) = parse_extra_env(&s.extra_env);
        assert_eq!(bad, ["not a setting", "9BAD NAME=1"]);
    }

    #[test]
    fn advanced_lines_win_over_the_preset() {
        let mut s = with_game(Preset::Recommended);
        s.extra_env = "VICTORY_RELAY=0\nVICTORY_AMBIENT=1\nVICTORY_RENDER_SCALE=0.75".into();
        let env = build_env(&s);
        assert_eq!(env["VICTORY_RELAY"], "0");
        assert_eq!(env["VICTORY_AMBIENT"], "1");
        assert_eq!(
            describe(&s),
            "Series S, the world drawn at 810p, ray tracing on"
        );
    }

    #[test]
    fn upscaling_switches_are_passed_as_environment() {
        let mut s = with_game(Preset::Recommended);
        s.upscaler = Upscaler::Dlss;
        s.dlss_dir = r"D:\sdk\streamline\bin".into();
        s.fidelityfx_dir = r"D:\sdk\ffx".into();
        s.quality = Quality::Balanced;
        s.frame_generation = FrameGen::Dlss;
        let env = build_env(&s);
        assert_eq!(env["VICTORY_UPSCALER"], "dlss");
        assert_eq!(env["VICTORY_DLSS_DIR"], r"D:\sdk\streamline\bin");
        assert_eq!(env["VICTORY_UPSCALE_QUALITY"], "balanced");
        assert_eq!(env["VICTORY_FRAMEGEN"], "dlss");
        // The vendor's upscaler needs the title's FSR call, at its own scale.
        assert!(!env.contains_key("VICTORY_OWN_UPSCALER"));
        assert!(!env.contains_key("VICTORY_RENDER_SCALE"));
        // The other vendor's folder is not handed over when it is not used.
        assert!(!env.contains_key("VICTORY_FIDELITYFX_DIR"));
        s.upscaler = Upscaler::Fsr;
        s.frame_generation = FrameGen::Off;
        let env = build_env(&s);
        assert_eq!(env["VICTORY_UPSCALER"], "fsr");
        assert_eq!(env["VICTORY_FIDELITYFX_DIR"], r"D:\sdk\ffx");
        assert!(!env.contains_key("VICTORY_FRAMEGEN") && !env.contains_key("VICTORY_DLSS_DIR"));
        s.frame_generation = FrameGen::Fsr;
        s.upscaler = Upscaler::Game;
        let env = build_env(&s);
        assert_eq!(env["VICTORY_OWN_UPSCALER"], "1");
        assert_eq!(env["VICTORY_FRAMEGEN"], "fsr");
    }

    #[test]
    fn ambient_zero_is_the_titles_own_and_numbers_are_short() {
        let mut s = with_game(Preset::Recommended);
        s.ambient = 0.0;
        assert_eq!(build_env(&s)["VICTORY_AMBIENT"], "0");
        assert_eq!(number(1.0), "1");
        assert_eq!(number(0.35), "0.35");
        assert_eq!(number(0.0), "0");
    }

    #[test]
    fn a_file_of_0_1_0_is_brought_to_the_new_defaults_once() {
        // What 0.1.0 wrote: every field, its default console among them.
        let old = "game_dir = 'D:\\g'\nsetup_done = true\nconsole = \"series-x\"\npreset = \"recommended\"\nambient = 0.8\nrender_scale = 0.0\n[updates]\nchannel = \"stable\"\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, old).unwrap();
        let (mut s, note) = Settings::load(&path);
        let note = note.unwrap();
        assert!(note.contains("Series S") && note.contains("Latest build"));
        assert_eq!(
            (s.console, s.defaults, s.ray_tracing, s.updates.channel),
            (Console::SeriesS, DEFAULTS, true, Channel::Nightly)
        );
        assert_eq!(s.warm, WarmMode::Used);
        // Chosen again after that, it stays.
        s.console = Console::SeriesX;
        s.save(&path).unwrap();
        let (s, note) = Settings::load(&path);
        assert_eq!((s.console, note), (Console::SeriesX, None));
        // A 0.1.0 file that had Series S says nothing.
        std::fs::write(
            &path,
            "console = \"series-s\"\n[updates]\nchannel = \"nightly\"\n",
        )
        .unwrap();
        assert_eq!(Settings::load(&path).1, None);
        // A file of defaults 2 keeps its console; Stable moves to Latest build once.
        std::fs::write(
            &path,
            "defaults = 2\nconsole = \"series-x\"\n[updates]\nchannel = \"stable\"\n",
        )
        .unwrap();
        let (mut s, note) = Settings::load(&path);
        assert!(note.unwrap().contains("Latest build"));
        assert_eq!(
            (s.console, s.updates.channel),
            (Console::SeriesX, Channel::Nightly)
        );
        s.updates.channel = Channel::Stable;
        s.save(&path).unwrap();
        let (s, note) = Settings::load(&path);
        assert_eq!((s.updates.channel, note), (Channel::Stable, None));
    }

    #[test]
    fn pasted_folders_lose_their_quotes() {
        assert_eq!(folder("  \"D:\\Games\\My Copy\"  "), r"D:\Games\My Copy");
        let s = Settings {
            game_dir: "\"D:\\Games\\My Copy\"".into(),
            ..Settings::default()
        };
        assert_eq!(
            s.game_exe(),
            PathBuf::from(r"D:\Games\My Copy\game_scarlett_master.exe")
        );
        assert_eq!(build_env(&s)["VICTORY_GAME_DIR"], r"D:\Games\My Copy");
    }

    #[test]
    fn game_folder_is_checked_for_what_a_run_needs() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().display().to_string();
        assert!(game_folder_problem("").unwrap().contains("No game folder"));
        assert!(
            game_folder_problem(&format!("{text}\\nowhere"))
                .unwrap()
                .contains("was not found")
        );
        let problem = game_folder_problem(&text).unwrap();
        assert!(
            problem.contains(paths::GAME_EXE) && problem.contains(paths::GAME_SHADERS),
            "{problem}"
        );
        std::fs::write(dir.path().join(paths::GAME_EXE), b"").unwrap();
        let problem = game_folder_problem(&text).unwrap();
        assert!(problem.contains("shaders.rpf is missing"), "{problem}");
        std::fs::write(dir.path().join(paths::GAME_SHADERS), b"").unwrap();
        assert_eq!(game_folder_problem(&text), None);
        // A file, not a folder.
        let exe = dir.path().join(paths::GAME_EXE).display().to_string();
        assert!(game_folder_problem(&exe).unwrap().contains("is a file"));
    }

    #[test]
    fn the_parent_of_the_game_folder_points_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Grand Copy \u{fc}").join("Content");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(paths::GAME_EXE), b"").unwrap();
        std::fs::write(game.join(paths::GAME_SHADERS), b"").unwrap();
        let problem = game_folder_problem(&dir.path().display().to_string()).unwrap();
        assert!(
            problem.contains(&format!("It seems to be in {}", game.display())),
            "{problem}"
        );
        assert_eq!(game_folder_below(dir.path()), Some(game.clone()));
        assert_eq!(game_folder_problem(&game.display().to_string()), None);
    }
}
