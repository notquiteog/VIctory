//! Per-user install and uninstall: program files to
//! `%LOCALAPPDATA%\Programs\VI`, two shortcuts, one entry under
//! `HKCU\...\Uninstall`. No administrator rights, nothing outside the
//! user's own profile, and never anything of the game.

use std::path::{Path, PathBuf};

use crate::paths::{self, InstallTargets};
use crate::settings::Settings;
use crate::win::{self, RegValue, Shortcut};

/// The multi-size icon `build.rs` makes (the drawn default, unless the
/// build was given a logo of its own).
pub const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/vi.ico"));
/// The same picture as a PNG, for the window.
pub const LOGO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/logo.png"));
/// Whether that picture is the builder's own (`VICTORY_LAUNCHER_LOGO`).
pub const LOGO_IS_OWN: bool = matches!(env!("VICTORY_LOGO_IS_OWN").as_bytes(), b"1");

const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// A picture (PNG bytes) as a multi-size `.ico`, fitted into a square.
pub fn ico_from_png(png: &[u8]) -> Result<Vec<u8>, String> {
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    let logo = image::load_from_memory(png)
        .map_err(|e| format!("not a picture that can be read ({e})"))?
        .to_rgba8();
    let mut frames = Vec::new();
    for size in ICON_SIZES {
        let scale = size as f32 / logo.width().max(logo.height()) as f32;
        let w = ((logo.width() as f32 * scale).round() as u32).clamp(1, size);
        let h = ((logo.height() as f32 * scale).round() as u32).clamp(1, size);
        let small = image::imageops::resize(&logo, w, h, image::imageops::FilterType::Lanczos3);
        let mut square = image::RgbaImage::new(size, size);
        image::imageops::overlay(
            &mut square,
            &small,
            i64::from((size - w) / 2),
            i64::from((size - h) / 2),
        );
        frames.push(
            IcoFrame::as_png(square.as_raw(), size, size, image::ExtendedColorType::Rgba8)
                .map_err(|e| e.to_string())?,
        );
    }
    let mut out = Vec::new();
    IcoEncoder::new(&mut out)
        .encode_images(&frames)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes the icon the shortcuts wear into the program folder: made of
/// the user's own logo if there is one that can be read, else the built-in.
pub fn write_icon(program_dir: &Path, logo: Option<&Path>) -> Result<(), String> {
    let own = logo
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|png| ico_from_png(&png).ok());
    let icon = program_dir.join(paths::ICON_FILE);
    std::fs::write(&icon, own.as_deref().unwrap_or(ICON))
        .map_err(|e| format!("Could not write {}: {e}", icon.display()))
}

fn shortcut_name() -> String {
    format!("{}.lnk", paths::DISPLAY_NAME)
}

pub fn desktop_shortcut(t: &InstallTargets) -> PathBuf {
    t.desktop_dir.join(shortcut_name())
}

pub fn start_menu_shortcut(t: &InstallTargets) -> PathBuf {
    t.start_menu_dir.join(shortcut_name())
}

/// What is installed now, as far as the files and the registry say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    pub launcher: bool,
    pub victory: bool,
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub version: Option<String>,
}

impl Installed {
    pub fn any(&self) -> bool {
        self.launcher
            || self.victory
            || self.desktop_shortcut
            || self.start_menu_shortcut
            || self.version.is_some()
    }

    pub fn complete(&self) -> bool {
        self.launcher
            && self.victory
            && self.desktop_shortcut
            && self.start_menu_shortcut
            && self.version.is_some()
    }
}

pub fn status(t: &InstallTargets) -> Installed {
    Installed {
        launcher: t.program_dir.join(paths::LAUNCHER_EXE).is_file(),
        victory: t.program_dir.join(paths::VICTORY_EXE).is_file(),
        desktop_shortcut: desktop_shortcut(t).is_file(),
        start_menu_shortcut: start_menu_shortcut(t).is_file(),
        version: win::read_uninstall_value(&t.uninstall_key, "DisplayVersion"),
    }
}

/// Where a `victory.exe` to install or run is: the developer setting, the
/// `--victory-exe` argument, then beside the launcher.
pub fn find_victory(argument: Option<&Path>, setting: &str) -> Option<PathBuf> {
    let candidates = [
        argument.map(Path::to_path_buf),
        (!setting.trim().is_empty()).then(|| PathBuf::from(setting.trim())),
        Some(paths::exe_dir().join(paths::VICTORY_EXE)),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// The end of a self-contained setup program: the launcher, then a zip
/// holding `victory.exe`, then the zip's length and this mark.
const SETUP_MARK: &[u8; 8] = b"VISETUP1";

/// (The launcher's own length, the packed `victory.exe`'s) if `exe` is a
/// self-contained setup program.
pub fn payload(exe: &Path) -> Option<(u64, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(exe).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::End(-16)).ok()?;
    let mut tail = [0u8; 16];
    file.read_exact(&mut tail).ok()?;
    let packed = u64::from_le_bytes(tail[..8].try_into().ok()?);
    (&tail[8..] == SETUP_MARK && packed > 0 && packed + 16 < len)
        .then(|| (len - 16 - packed, packed))
}

/// Makes the one-file download: `launcher` with `victory` packed behind it.
pub fn make_setup(launcher: &Path, victory: &Path, out: &Path) -> Result<(), String> {
    use std::io::Write;
    let read =
        |p: &Path| std::fs::read(p).map_err(|e| format!("Could not read {}: {e}", p.display()));
    let mut bytes = read(launcher)?;
    if let Some((own, _)) = payload(launcher) {
        bytes.truncate(own as usize);
    }
    let failed = |e: &dyn std::fmt::Display| format!("Could not pack {}: {e}", victory.display());
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    zip.start_file(paths::VICTORY_EXE, options)
        .map_err(|e| failed(&e))?;
    zip.write_all(&read(victory)?).map_err(|e| failed(&e))?;
    let packed = zip.finish().map_err(|e| failed(&e))?.into_inner();
    bytes.extend_from_slice(&packed);
    bytes.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    bytes.extend_from_slice(SETUP_MARK);
    std::fs::write(out, bytes).map_err(|e| format!("Could not write {}: {e}", out.display()))
}

/// Unpacks the `victory.exe` a setup program carries.
fn extract_victory(setup: &Path, to: &Path) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let failed = |e: &dyn std::fmt::Display| {
        format!("Could not unpack victory.exe from {}: {e}", setup.display())
    };
    let (own, packed) = payload(setup).ok_or_else(|| failed(&"it does not carry one"))?;
    let mut file = std::fs::File::open(setup).map_err(|e| failed(&e))?;
    file.seek(SeekFrom::Start(own)).map_err(|e| failed(&e))?;
    let mut bytes = vec![0u8; packed as usize];
    file.read_exact(&mut bytes).map_err(|e| failed(&e))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| failed(&e))?;
    let mut entry = archive
        .by_name(paths::VICTORY_EXE)
        .map_err(|e| failed(&e))?;
    let mut out =
        std::fs::File::create(to).map_err(|e| format!("Could not write {}: {e}", to.display()))?;
    std::io::copy(&mut entry, &mut out)
        .map(|_| ())
        .map_err(|e| failed(&e))
}

/// Copies the launcher without what a setup program carries behind it.
fn copy_launcher(from: &Path, to: &Path) -> Result<(), String> {
    if paths::same_path(from, to) {
        return Ok(());
    }
    match payload(from) {
        None => copy(from, to),
        Some((own, _)) => {
            let mut bytes = std::fs::read(from)
                .map_err(|e| format!("Could not read {}: {e}", from.display()))?;
            bytes.truncate(own as usize);
            std::fs::write(to, bytes).map_err(|e| format!("Could not write {}: {e}", to.display()))
        }
    }
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if paths::same_path(from, to) {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("Could not copy {} to {}: {e}", from.display(), to.display()))
}

fn write_manifest(program_dir: &Path, files: &[&str]) -> Result<(), String> {
    let path = program_dir.join(paths::MANIFEST_FILE);
    // (An update may have added files: keep what an older list names.)
    let mut names: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    names.extend(files.iter().map(|f| (*f).to_owned()));
    names.push(paths::MANIFEST_FILE.to_owned());
    names.retain(|n| !n.trim().is_empty());
    names.sort();
    names.dedup();
    std::fs::write(&path, names.join("\n") + "\n")
        .map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// Adds files an update brought to the list uninstall goes by.
pub fn add_to_manifest(program_dir: &Path, files: &[PathBuf]) {
    let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let _ = write_manifest(program_dir, &names);
}

pub fn write_registry(t: &InstallTargets, version: &str) -> Result<(), String> {
    let launcher = t.program_dir.join(paths::LAUNCHER_EXE);
    let size_kb = [paths::LAUNCHER_EXE, paths::VICTORY_EXE]
        .iter()
        .map(|f| std::fs::metadata(t.program_dir.join(f)).map_or(0, |m| m.len()))
        .sum::<u64>()
        / 1024;
    let display_name = if t.trial {
        format!("{} (trial install)", paths::DISPLAY_NAME)
    } else {
        paths::DISPLAY_NAME.to_owned()
    };
    win::write_uninstall_entry(
        &t.uninstall_key,
        &[
            ("DisplayName", RegValue::Text(&display_name)),
            ("DisplayVersion", RegValue::Text(version)),
            ("Publisher", RegValue::Text("VIctory contributors")),
            (
                "InstallLocation",
                RegValue::Text(&t.program_dir.display().to_string()),
            ),
            (
                "DisplayIcon",
                RegValue::Text(&t.program_dir.join(paths::ICON_FILE).display().to_string()),
            ),
            (
                "UninstallString",
                RegValue::Text(&format!("\"{}\" --uninstall", launcher.display())),
            ),
            (
                "URLInfoAbout",
                RegValue::Text(&format!("https://github.com/{}", crate::update::REPO)),
            ),
            ("NoModify", RegValue::Number(1)),
            ("NoRepair", RegValue::Number(1)),
            (
                "EstimatedSize",
                RegValue::Number(size_kb.min(u64::from(u32::MAX)) as u32),
            ),
        ],
    )
}

/// (The tests': an install with the built-in icon.)
#[cfg(test)]
pub fn install(
    t: &InstallTargets,
    launcher_from: &Path,
    victory_from: Option<&Path>,
) -> Result<Vec<String>, String> {
    install_with_logo(t, launcher_from, victory_from, None)
}

/// Installs (or repairs, or installs over) from the running launcher and
/// the given `victory.exe`, or without one from what a self-contained setup
/// program carries. The shortcuts wear the user's own `logo` if given.
/// Returns what was done, line by line.
pub fn install_with_logo(
    t: &InstallTargets,
    launcher_from: &Path,
    victory_from: Option<&Path>,
    logo: Option<&Path>,
) -> Result<Vec<String>, String> {
    match victory_from {
        Some(victory) if !victory.is_file() => {
            return Err(format!(
                "victory.exe was not found at {}. It has to be beside the launcher (or named with --victory-exe).",
                victory.display()
            ));
        }
        None if payload(launcher_from).is_none() => {
            return Err(format!(
                "victory.exe was not found beside {}. Unpack the whole download into one folder, or use the one-file VI-Setup.exe.",
                launcher_from.display()
            ));
        }
        _ => {}
    }
    let mut done = Vec::new();
    std::fs::create_dir_all(&t.program_dir)
        .map_err(|e| format!("Could not create {}: {e}", t.program_dir.display()))?;
    let launcher = t.program_dir.join(paths::LAUNCHER_EXE);
    copy_launcher(launcher_from, &launcher)?;
    match victory_from {
        Some(victory) => copy(victory, &t.program_dir.join(paths::VICTORY_EXE))?,
        None => extract_victory(launcher_from, &t.program_dir.join(paths::VICTORY_EXE))?,
    }
    let icon = t.program_dir.join(paths::ICON_FILE);
    write_icon(&t.program_dir, logo)?;
    write_manifest(
        &t.program_dir,
        &[paths::LAUNCHER_EXE, paths::VICTORY_EXE, paths::ICON_FILE],
    )?;
    done.push(format!(
        "Program files copied to {}",
        t.program_dir.display()
    ));
    let shortcut = Shortcut {
        target: launcher,
        arguments: String::new(),
        working_dir: t.program_dir.clone(),
        icon,
        description: "Play your own copy of the game on this PC".to_owned(),
    };
    for link in [desktop_shortcut(t), start_menu_shortcut(t)] {
        win::create_shortcut(&link, &shortcut)?;
        done.push(format!("Shortcut created: {}", link.display()));
    }
    write_registry(t, crate::update::current_version())?;
    done.push("Added to the list of installed apps (for this Windows user only)".to_owned());
    Ok(done)
}

/// Deletes a program file, trying again for a few seconds: the launcher
/// that asked for the uninstall may still be ending.
fn remove_program_file(path: &Path) -> std::io::Result<()> {
    let began = std::time::Instant::now();
    loop {
        match std::fs::remove_file(path) {
            // (Access denied: a running program. Sharing violation: a file being read.)
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32)) && began.elapsed().as_secs() < 6 => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            other => return other,
        }
    }
}

/// Removes shortcuts, the registry entry and the program files the
/// manifest names (and the folder, if that leaves it empty). With
/// `remove_data` (the settings and the launcher's own folder), also
/// settings, logs, caches and (if in that folder) saves. Returns what could not be done or was left on
/// purpose; an empty list is a clean uninstall.
pub fn uninstall(t: &InstallTargets, remove_data: Option<(&Settings, &Path)>) -> Vec<String> {
    let mut notes = Vec::new();
    for link in [desktop_shortcut(t), start_menu_shortcut(t)] {
        if link.is_file()
            && let Err(e) = std::fs::remove_file(&link)
        {
            notes.push(format!("Could not delete {}: {e}", link.display()));
        }
    }
    if t.trial {
        let _ = std::fs::remove_dir(&t.desktop_dir);
        let _ = std::fs::remove_dir(&t.start_menu_dir);
        if let Some(parent) = t.desktop_dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    if let Err(e) = win::delete_uninstall_entry(&t.uninstall_key) {
        notes.push(e);
    }
    let manifest =
        std::fs::read_to_string(t.program_dir.join(paths::MANIFEST_FILE)).unwrap_or_default();
    let mut names: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    names.extend([
        paths::LAUNCHER_EXE,
        paths::VICTORY_EXE,
        paths::ICON_FILE,
        paths::MANIFEST_FILE,
    ]);
    for name in names {
        // Only names below the program folder: a manifest is a text file anyone can edit.
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let path = t.program_dir.join(relative);
        for path in [
            path.clone(),
            PathBuf::from(format!("{}.old", path.display())),
        ] {
            if path.is_file()
                && let Err(e) = remove_program_file(&path)
            {
                notes.push(format!("Could not delete {}: {e}. If the game is running, close it and uninstall again.", path.display()));
            }
        }
        if let Some(parent) = path.parent()
            && parent != t.program_dir
        {
            let _ = std::fs::remove_dir(parent);
        }
    }
    if t.program_dir.is_dir()
        && let Err(e) = std::fs::remove_dir(&t.program_dir)
    {
        notes.push(format!(
            "The folder {} was not removed ({e}); it holds files that were not installed by VI.",
            t.program_dir.display()
        ));
    }
    if let Some((s, home)) = remove_data {
        let (delete, kept) =
            crate::caches::data_removal_plan(&s.cache_path(), &s.data_path(), home);
        notes.extend(kept);
        let game = Path::new(s.game_dir.trim());
        for dir in delete {
            if !s.game_dir.trim().is_empty()
                && (paths::is_within(&dir, game) || paths::is_within(game, &dir))
            {
                notes.push(format!(
                    "{} was left alone: it overlaps the game folder.",
                    dir.display()
                ));
                continue;
            }
            if dir.is_dir()
                && let Err(e) = std::fs::remove_dir_all(&dir)
            {
                notes.push(format!("Could not delete {}: {e}", dir.display()));
            }
        }
        for file in [
            home.join("settings.toml"),
            home.join("settings.toml.unreadable"),
            home.join(paths::USER_LOGO),
        ] {
            let _ = std::fs::remove_file(file);
        }
        let _ = std::fs::remove_dir(s.cache_path());
        let _ = std::fs::remove_dir(home);
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(root: &Path, key: &str) -> InstallTargets {
        let mut t = paths::install_targets_from(Some(root.to_path_buf()), root, None, None);
        // (A key of its own per test: they run side by side.)
        t.uninstall_key = format!("VIctory-test-{key}-{}", std::process::id());
        t
    }

    #[test]
    fn a_users_own_logo_becomes_the_shortcuts_icon() {
        let dir = tempfile::tempdir().unwrap();
        let t = trial(&dir.path().join("trial"), "logo");
        std::fs::write(dir.path().join("victory-launcher.exe"), b"launcher").unwrap();
        std::fs::write(dir.path().join("victory.exe"), b"victory").unwrap();
        // A wide red picture of the user's.
        let logo = dir.path().join("logo.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([255, 0, 0, 255]))
            .save(&logo)
            .unwrap();
        install_with_logo(
            &t,
            &dir.path().join("victory-launcher.exe"),
            Some(&dir.path().join("victory.exe")),
            Some(&logo),
        )
        .unwrap();
        let written = std::fs::read(t.program_dir.join("VI.ico")).unwrap();
        assert_ne!(written, ICON);
        let icon = image::load_from_memory_with_format(&written, image::ImageFormat::Ico)
            .unwrap()
            .to_rgba8();
        // The largest frame: square, the picture across the middle, nothing above it.
        assert_eq!(icon.dimensions(), (256, 256));
        assert_eq!(icon.get_pixel(128, 128).0, [255, 0, 0, 255]);
        assert_eq!(icon.get_pixel(128, 10).0[3], 0);
        // A file that is no picture: the built-in icon, not a failed install.
        std::fs::write(&logo, b"not a png").unwrap();
        install_with_logo(
            &t,
            &dir.path().join("victory-launcher.exe"),
            Some(&dir.path().join("victory.exe")),
            Some(&logo),
        )
        .unwrap();
        assert_eq!(std::fs::read(t.program_dir.join("VI.ico")).unwrap(), ICON);
        assert!(ico_from_png(b"not a png").is_err());
        assert!(uninstall(&t, None).is_empty());
    }

    #[test]
    fn the_built_in_logo_is_the_drawn_one() {
        // A build without a logo of its own: the dark tile with light letters,
        // clear corners. (Skipped when the builder gave a picture.)
        if LOGO_IS_OWN {
            return;
        }
        let logo = image::load_from_memory(LOGO).unwrap().to_rgba8();
        assert_eq!(logo.dimensions(), (256, 256));
        assert_eq!(logo.get_pixel(0, 0).0[3], 0);
        assert_eq!(logo.get_pixel(128, 30).0, [24, 27, 33, 255]);
        // In the stroke of the I.
        assert_eq!(logo.get_pixel(190, 128).0, [240, 242, 245, 255]);
    }

    #[test]
    fn the_icon_is_a_multi_size_ico_of_the_logo() {
        // ICONDIR: reserved 0, type 1, count; then 16-byte entries.
        assert_eq!(&ICON[..4], [0, 0, 1, 0]);
        let count = u16::from_le_bytes([ICON[4], ICON[5]]) as usize;
        let mut sizes: Vec<u32> = (0..count)
            .map(|i| match ICON[6 + i * 16] {
                0 => 256,
                n => u32::from(n),
            })
            .collect();
        sizes.sort();
        assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 256]);
    }

    #[test]
    fn install_then_uninstall_into_a_trial_folder() {
        let dir = tempfile::tempdir().unwrap();
        let t = trial(&dir.path().join("trial"), "roundtrip");
        let source = dir.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("victory-launcher.exe"), b"launcher").unwrap();
        std::fs::write(source.join("victory.exe"), b"victory").unwrap();
        assert!(!status(&t).any());

        let done = install(
            &t,
            &source.join("victory-launcher.exe"),
            Some(&source.join("victory.exe")),
        )
        .unwrap();
        assert_eq!(done.len(), 4);
        let s = status(&t);
        assert!(s.complete(), "{s:?}");
        assert_eq!(s.version.as_deref(), Some(crate::update::current_version()));
        assert_eq!(
            std::fs::read(t.program_dir.join("victory.exe")).unwrap(),
            b"victory"
        );
        assert_eq!(std::fs::read(t.program_dir.join("VI.ico")).unwrap(), ICON);

        // The shortcuts are named VI, start the installed launcher and wear the icon file.
        for link in [desktop_shortcut(&t), start_menu_shortcut(&t)] {
            assert_eq!(link.file_name().unwrap(), "VI.lnk");
            let read = win::read_shortcut(&link).unwrap();
            assert!(
                paths::same_path(&read.target, &t.program_dir.join("victory-launcher.exe")),
                "{read:?}"
            );
            assert!(
                paths::same_path(&read.icon, &t.program_dir.join("VI.ico")),
                "{read:?}"
            );
            assert!(
                paths::same_path(&read.working_dir, &t.program_dir),
                "{read:?}"
            );
        }
        let uninstall_string =
            win::read_uninstall_value(&t.uninstall_key, "UninstallString").unwrap();
        assert!(
            uninstall_string.ends_with("victory-launcher.exe\" --uninstall"),
            "{uninstall_string}"
        );
        assert_eq!(
            win::read_uninstall_value(&t.uninstall_key, "DisplayName").unwrap(),
            "VI (trial install)"
        );

        // Installing again over it is fine (repair).
        install(
            &t,
            &source.join("victory-launcher.exe"),
            Some(&source.join("victory.exe")),
        )
        .unwrap();

        // An update's leftovers and a stranger's file.
        std::fs::write(t.program_dir.join("victory.exe.old"), b"x").unwrap();
        std::fs::write(t.program_dir.join("my-notes.txt"), b"mine").unwrap();
        let notes = uninstall(&t, None);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("not installed by VI"));
        assert!(t.program_dir.join("my-notes.txt").is_file());
        assert!(
            !t.program_dir.join("victory.exe").exists()
                && !t.program_dir.join("victory.exe.old").exists()
        );
        let s = status(&t);
        assert!(!s.any(), "{s:?}");
        std::fs::remove_file(t.program_dir.join("my-notes.txt")).unwrap();
        assert!(uninstall(&t, None).is_empty());
        assert!(!t.program_dir.exists());
    }

    #[test]
    fn install_without_victory_says_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        let t = trial(&dir.path().join("trial"), "missing");
        let e = install(
            &t,
            &dir.path().join("victory-launcher.exe"),
            Some(&dir.path().join("victory.exe")),
        )
        .unwrap_err();
        assert!(e.contains("victory.exe was not found at"), "{e}");
        std::fs::write(
            dir.path().join("victory-launcher.exe"),
            b"a launcher without anything behind it",
        )
        .unwrap();
        let e = install(&t, &dir.path().join("victory-launcher.exe"), None).unwrap_err();
        assert!(e.contains("victory.exe was not found beside"), "{e}");
        assert!(!status(&t).any());
    }

    #[test]
    fn a_one_file_setup_program_carries_victory_and_installs_both() {
        let dir = tempfile::tempdir().unwrap();
        let t = trial(&dir.path().join("trial"), "setup");
        let launcher: Vec<u8> = (0..50_000u32).flat_map(|n| n.to_le_bytes()).collect();
        let victory: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        std::fs::write(dir.path().join("victory-launcher.exe"), &launcher).unwrap();
        std::fs::write(dir.path().join("victory.exe"), &victory).unwrap();
        let setup = dir.path().join("VI-Setup.exe");
        assert_eq!(payload(&dir.path().join("victory-launcher.exe")), None);
        make_setup(
            &dir.path().join("victory-launcher.exe"),
            &dir.path().join("victory.exe"),
            &setup,
        )
        .unwrap();
        let (own, packed) = payload(&setup).unwrap();
        assert_eq!(own, launcher.len() as u64);
        assert!(packed < victory.len() as u64, "packed {packed}");
        // Made from a setup program again, it does not grow.
        let again = dir.path().join("again.exe");
        make_setup(&setup, &dir.path().join("victory.exe"), &again).unwrap();
        assert_eq!(
            std::fs::read(&again).unwrap(),
            std::fs::read(&setup).unwrap()
        );

        install(&t, &setup, None).unwrap();
        assert!(status(&t).complete());
        // Installed: the plain launcher and the whole victory.exe.
        assert_eq!(
            std::fs::read(t.program_dir.join("victory-launcher.exe")).unwrap(),
            launcher
        );
        assert_eq!(
            std::fs::read(t.program_dir.join("victory.exe")).unwrap(),
            victory
        );
        assert!(uninstall(&t, None).is_empty());
    }

    #[test]
    fn uninstall_with_data_spares_the_game_and_outside_saves() {
        let dir = tempfile::tempdir().unwrap();
        let t = trial(&dir.path().join("trial"), "data");
        let game = dir.path().join("game");
        let saves = dir.path().join("my saves");
        let cache = dir.path().join("cache");
        for d in [
            &game,
            &saves,
            &cache.join("shaders"),
            &cache.join("pipelines"),
            &cache.join("other"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(game.join(paths::GAME_EXE), b"game").unwrap();
        std::fs::write(saves.join("save"), b"save").unwrap();
        std::fs::write(cache.join("shaders").join("s"), b"s").unwrap();
        let s = Settings {
            game_dir: game.display().to_string(),
            cache_dir: cache.display().to_string(),
            data_dir: saves.display().to_string(),
            ..Settings::default()
        };
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("logs")).unwrap();
        std::fs::write(home.join("settings.toml"), "x").unwrap();
        let notes = uninstall(&t, Some((&s, &home)));
        assert!(!home.exists());
        assert!(notes.iter().any(|n| n.contains("left alone")), "{notes:?}");
        assert!(game.join(paths::GAME_EXE).is_file());
        assert!(saves.join("save").is_file());
        assert!(!cache.join("shaders").exists() && !cache.join("pipelines").exists());
        // What the launcher does not know in the cache folder stays.
        assert!(cache.join("other").is_dir());
    }

    #[test]
    fn manifest_names_outside_the_program_folder_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let t = trial(&dir.path().join("trial"), "manifest");
        std::fs::create_dir_all(&t.program_dir).unwrap();
        let outside = dir.path().join("precious.txt");
        std::fs::write(&outside, b"keep").unwrap();
        std::fs::write(
            t.program_dir.join(paths::MANIFEST_FILE),
            format!("..\\..\\precious.txt\n{}\n", outside.display()),
        )
        .unwrap();
        uninstall(&t, None);
        assert!(outside.is_file());
    }
}
