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
    Console {
        #[default]
        SeriesX = "series-x" => "Xbox Series X",
        SeriesS = "series-s" => "Xbox Series S",
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
        #[default]
        Stable = "stable" => "Stable (latest release)",
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
    pub console: Console,
    pub preset: Preset,
    /// `VICTORY_AMBIENT`: 0 leaves the picture as the game makes it.
    pub ambient: f32,
    pub upscaler: Upscaler,
    pub dlss_dir: String,
    pub fidelityfx_dir: String,
    pub quality: Quality,
    pub frame_generation: FrameGen,
    /// `VICTORY_RENDER_SCALE` (0.2 to 1.0); 0 leaves it to the game.
    pub render_scale: f32,
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
            channel: Channel::Stable,
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
            console: Console::default(),
            preset: Preset::default(),
            ambient: 0.8,
            upscaler: Upscaler::default(),
            dlss_dir: String::new(),
            fidelityfx_dir: String::new(),
            quality: Quality::default(),
            frame_generation: FrameGen::default(),
            render_scale: 0.0,
            extra_env: String::new(),
            updates: Updates::default(),
            prepared: Prepared::default(),
            developer: Developer::default(),
        }
    }
}

const HEADER: &str =
    "# VI launcher settings. Edit in the launcher (Settings tab) or here while it is closed.\n\n";

impl Settings {
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
            Ok(s) => (s, None),
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
        match self.cache_dir.trim() {
            "" => paths::home().join("cache"),
            dir => PathBuf::from(dir),
        }
    }

    pub fn data_path(&self) -> PathBuf {
        match self.data_dir.trim() {
            "" => paths::home().join("data"),
            dir => PathBuf::from(dir),
        }
    }

    pub fn game_exe(&self) -> PathBuf {
        Path::new(self.game_dir.trim()).join(paths::GAME_EXE)
    }

    pub fn shader_archive(&self) -> PathBuf {
        Path::new(self.game_dir.trim()).join(paths::GAME_SHADERS)
    }
}

/// What is wrong with a game folder, in words for the user; `None` if it
/// has what a run needs.
pub fn game_folder_problem(dir: &str) -> Option<String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return Some("No game folder chosen yet.".to_owned());
    }
    let path = Path::new(dir);
    if !path.is_dir() {
        return Some(format!("{dir} is not a folder that exists."));
    }
    let missing: Vec<&str> = [paths::GAME_EXE, paths::GAME_SHADERS]
        .into_iter()
        .filter(|name| !path.join(name).is_file())
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "{dir} does not look like the game: {} {} missing. Choose the folder of your own Xbox Series copy, the one that holds {}.",
            missing.join(" and "),
            if missing.len() == 1 { "is" } else { "are" },
            paths::GAME_EXE
        ))
    }
}

/// The switches of `tools/play.ps1` without the developer aids (scripted
/// keys, DRED, capture, profiling): what "Recommended" sets.
const RECOMMENDED: &[(&str, &str)] = &[
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
    ("VICTORY_THIN", "1"),
    ("VICTORY_JOIN_PARTS", "8"),
    // Runs write down the pipelines the game makes: what "Warm up pipelines" works from.
    ("VICTORY_PIPELINE_BOOK", "1"),
];
/// What "Safe" leaves out of the above.
const NOT_IN_SAFE: &[&str] = &["VICTORY_THIN", "VICTORY_JOIN_PARTS"];
/// Never set by the launcher itself (the Advanced box can, knowingly).
#[cfg(test)]
const DEVELOPER_AIDS: &[&str] = &[
    "VICTORY_KEYS",
    "VICTORY_D3D12_DRED",
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

/// The `VICTORY_*` environment of a run with these settings: the three
/// folders, the preset, the picture choices, then the Advanced lines (which
/// win). Sorted by name.
pub fn build_env(s: &Settings) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut set = |name: &str, value: String| {
        env.insert(name.to_owned(), value);
    };
    if !s.game_dir.trim().is_empty() {
        set("VICTORY_GAME_DIR", s.game_dir.trim().to_owned());
    }
    set("VICTORY_CACHE_DIR", s.cache_path().display().to_string());
    set("VICTORY_DATA_DIR", s.data_path().display().to_string());
    if s.preset != Preset::Custom {
        for &(name, value) in RECOMMENDED {
            if s.preset == Preset::Safe && NOT_IN_SAFE.contains(&name) {
                continue;
            }
            set(name, value.to_owned());
        }
        if s.ambient > 0.0 {
            set("VICTORY_AMBIENT", number(s.ambient.clamp(0.0, 4.0)));
        }
    }
    // The upscaling switches are experimental: plain environment variables
    // that a `victory.exe` without them does not look at.
    match s.upscaler {
        Upscaler::Game => {}
        Upscaler::Dlss => {
            set("VICTORY_UPSCALER", "dlss".to_owned());
            set("VICTORY_DLSS_MODE", s.quality.word().to_owned());
        }
        Upscaler::Fsr => {
            set("VICTORY_UPSCALER", "fsr".to_owned());
            set("VICTORY_DLSS_MODE", s.quality.word().to_owned());
        }
    }
    let wants_dlss = s.upscaler == Upscaler::Dlss || s.frame_generation == FrameGen::Dlss;
    let wants_fsr = s.upscaler == Upscaler::Fsr || s.frame_generation == FrameGen::Fsr;
    if wants_dlss && !s.dlss_dir.trim().is_empty() {
        set("VICTORY_DLSS_DIR", s.dlss_dir.trim().to_owned());
    }
    if wants_fsr && !s.fidelityfx_dir.trim().is_empty() {
        set("VICTORY_FIDELITYFX_DIR", s.fidelityfx_dir.trim().to_owned());
    }
    if s.frame_generation != FrameGen::Off {
        set("VICTORY_FRAMEGEN", s.frame_generation.word().to_owned());
    }
    if s.render_scale > 0.0 {
        set(
            "VICTORY_RENDER_SCALE",
            number(s.render_scale.clamp(0.2, 1.0)),
        );
    }
    for (name, value) in parse_extra_env(&s.extra_env).0 {
        set(&name, value);
    }
    env
}

/// One line of help for each setting, shown under it.
pub mod help {
    pub const GAME_DIR: &str =
        "The folder of your own Xbox Series copy of the game. It is only read, never changed.";
    pub const CACHE_DIR: &str = "Translated shaders and prepared pipelines. Can be deleted at any time; needs tens of GB free.";
    pub const DATA_DIR: &str =
        "Your saved games and what the game stores between sessions. Keep this.";
    pub const CONSOLE: &str = "Which console the game believes it runs on. Series X is the tested one; Series S asks less of the PC.";
    pub const PRESET: &str = "Recommended: the fastest tested settings. Safe: without the two newest speed-ups, try it if the game misbehaves. Custom: only what you type under Advanced.";
    pub const AMBIENT: &str = "Brightens rooms that have little direct light. 0 is the game's own picture; 0.8 is the tested level.";
    pub const UPSCALER: &str = "Who enlarges the picture to your screen: the game's own code, NVIDIA DLSS or AMD FSR (you supply the vendor's files).";
    pub const VENDOR_DIR: &str = "The folder holding the vendor's own DLL files on your PC. VIctory does not ship or download them.";
    pub const QUALITY: &str =
        "Higher quality draws more pixels itself; Performance draws fewer and enlarges more.";
    pub const FRAMEGEN: &str = "Lets the graphics card insert in-between frames for smoother motion. Needs a card and files that support it.";
    pub const RENDER_SCALE: &str = "How much of the screen's resolution the game draws before upscaling. Lower is faster and blurrier.";
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
            console: Console::SeriesS,
            preset: Preset::Safe,
            ambient: 0.5,
            upscaler: Upscaler::Dlss,
            dlss_dir: r"D:\sdk\streamline".into(),
            quality: Quality::Performance,
            frame_generation: FrameGen::Fsr,
            render_scale: 0.67,
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
        assert!(text.contains("console = \"series-s\""), "{text}");
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

    #[test]
    fn recommended_is_the_play_script_without_developer_aids() {
        let env = build_env(&with_game(Preset::Recommended));
        for (name, value) in RECOMMENDED {
            assert_eq!(env.get(*name).map(String::as_str), Some(*value), "{name}");
        }
        assert_eq!(env["VICTORY_THIN"], "1");
        assert_eq!(env["VICTORY_JOIN_PARTS"], "8");
        assert_eq!(env["VICTORY_AMBIENT"], "0.8");
        assert_eq!(env["VICTORY_GAME_DIR"], r"D:\Games\MyCopy");
        assert_eq!(env["VICTORY_CACHE_DIR"], r"D:\VIctory\cache");
        assert_eq!(env["VICTORY_DATA_DIR"], r"D:\VIctory\data");
        for aid in DEVELOPER_AIDS {
            assert!(!env.contains_key(*aid), "{aid} must not be set by a preset");
        }
        // Nothing experimental unless asked for.
        for name in [
            "VICTORY_UPSCALER",
            "VICTORY_FRAMEGEN",
            "VICTORY_RENDER_SCALE",
            "VICTORY_DLSS_DIR",
            "VICTORY_DLSS_MODE",
        ] {
            assert!(!env.contains_key(name), "{name}");
        }
    }

    #[test]
    fn safe_is_recommended_without_the_two_newest() {
        let recommended = build_env(&with_game(Preset::Recommended));
        let safe = build_env(&with_game(Preset::Safe));
        let gone: Vec<&String> = recommended
            .keys()
            .filter(|k| !safe.contains_key(*k))
            .collect();
        assert_eq!(gone, ["VICTORY_JOIN_PARTS", "VICTORY_THIN"]);
        assert!(safe.iter().all(|(k, v)| recommended.get(k) == Some(v)));
    }

    #[test]
    fn custom_sets_only_folders_and_what_is_typed() {
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
                "VICTORY_RELAY"
            ]
        );
        assert_eq!(env["VICTORY_Q"], "a b");
        let (_, bad) = parse_extra_env(&s.extra_env);
        assert_eq!(bad, ["not a setting", "9BAD NAME=1"]);
    }

    #[test]
    fn advanced_lines_win_over_the_preset() {
        let mut s = with_game(Preset::Recommended);
        s.extra_env = "VICTORY_RELAY=0\nVICTORY_AMBIENT=1".into();
        let env = build_env(&s);
        assert_eq!(env["VICTORY_RELAY"], "0");
        assert_eq!(env["VICTORY_AMBIENT"], "1");
    }

    #[test]
    fn upscaling_switches_are_passed_as_environment() {
        let mut s = with_game(Preset::Recommended);
        s.upscaler = Upscaler::Dlss;
        s.dlss_dir = r"D:\sdk\streamline\bin".into();
        s.fidelityfx_dir = r"D:\sdk\ffx".into();
        s.quality = Quality::Balanced;
        s.frame_generation = FrameGen::Dlss;
        s.render_scale = 0.5;
        let env = build_env(&s);
        assert_eq!(env["VICTORY_UPSCALER"], "dlss");
        assert_eq!(env["VICTORY_DLSS_DIR"], r"D:\sdk\streamline\bin");
        assert_eq!(env["VICTORY_DLSS_MODE"], "balanced");
        assert_eq!(env["VICTORY_FRAMEGEN"], "dlss");
        assert_eq!(env["VICTORY_RENDER_SCALE"], "0.5");
        // The other vendor's folder is not handed over when it is not used.
        assert!(!env.contains_key("VICTORY_FIDELITYFX_DIR"));
        s.upscaler = Upscaler::Fsr;
        s.frame_generation = FrameGen::Off;
        let env = build_env(&s);
        assert_eq!(env["VICTORY_UPSCALER"], "fsr");
        assert_eq!(env["VICTORY_FIDELITYFX_DIR"], r"D:\sdk\ffx");
        assert!(!env.contains_key("VICTORY_FRAMEGEN") && !env.contains_key("VICTORY_DLSS_DIR"));
    }

    #[test]
    fn ambient_zero_is_left_out_and_numbers_are_short() {
        let mut s = with_game(Preset::Recommended);
        s.ambient = 0.0;
        assert!(!build_env(&s).contains_key("VICTORY_AMBIENT"));
        assert_eq!(number(1.0), "1");
        assert_eq!(number(0.35), "0.35");
        assert_eq!(number(0.0), "0");
    }

    #[test]
    fn game_folder_is_checked_for_what_a_run_needs() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().display().to_string();
        assert!(game_folder_problem("").unwrap().contains("No game folder"));
        assert!(
            game_folder_problem(&format!("{text}\\nowhere"))
                .unwrap()
                .contains("not a folder")
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
    }
}
