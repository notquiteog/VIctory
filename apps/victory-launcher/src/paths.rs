//! Where the launcher keeps things. Nothing here is a fixed path: every
//! location comes from the environment or a known-folder query.

use std::path::{Path, PathBuf};

/// The name the user sees: the window, the shortcuts, the "Installed apps"
/// entry, the program folder.
pub const DISPLAY_NAME: &str = "VI";
/// The name of what the user does not look at: the settings and cache
/// folder, the registry key.
pub const APP_NAME: &str = "VIctory";
pub const LAUNCHER_EXE: &str = "victory-launcher.exe";
pub const VICTORY_EXE: &str = "victory.exe";
pub const ICON_FILE: &str = "VI.ico";
pub const MANIFEST_FILE: &str = "install-manifest.txt";
/// The title's executable and its shader archive, as a dump names them.
pub const GAME_EXE: &str = "game_scarlett_master.exe";
pub const GAME_SHADERS: &str = "shaders.rpf";

/// Tests and trial installs put everything below this folder instead.
pub const INSTALL_OVERRIDE_ENV: &str = "VICTORY_INSTALL_DIR_OVERRIDE";
/// Tests move settings, logs and default cache/saves below this folder.
pub const HOME_OVERRIDE_ENV: &str = "VICTORY_LAUNCHER_HOME";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `%LOCALAPPDATA%`.
pub fn local_appdata() -> PathBuf {
    env_path("LOCALAPPDATA")
        .or_else(|| crate::win::known_folder(crate::win::Folder::LocalAppData))
        .unwrap_or_else(std::env::temp_dir)
}

/// Settings, logs, downloaded updates and (by default) cache and saves:
/// `%LOCALAPPDATA%\VIctory`.
pub fn home() -> PathBuf {
    env_path(HOME_OVERRIDE_ENV).unwrap_or_else(|| local_appdata().join(APP_NAME))
}

/// A picture of the user's own, shown instead of the built-in logo.
pub const USER_LOGO: &str = "logo.png";

/// The user's own logo, if there is one: in the settings folder, else
/// beside the launcher.
pub fn user_logo() -> Option<PathBuf> {
    [home().join(USER_LOGO), exe_dir().join(USER_LOGO)]
        .into_iter()
        .find(|p| p.is_file())
}

pub fn settings_file() -> PathBuf {
    home().join("settings.toml")
}

pub fn logs_dir() -> PathBuf {
    home().join("logs")
}

pub fn updates_dir() -> PathBuf {
    home().join("updates")
}

/// Where an install puts things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTargets {
    /// The program files.
    pub program_dir: PathBuf,
    /// The folder the Desktop shortcut goes into.
    pub desktop_dir: PathBuf,
    /// The folder the Start-menu shortcut goes into.
    pub start_menu_dir: PathBuf,
    /// The name of the key below `HKCU\...\Uninstall`.
    pub uninstall_key: String,
    /// A trial install (the override): nothing outside its folder but the
    /// registry key above, which has a name of its own.
    pub trial: bool,
}

/// The per-user install location, or everything below the override.
pub fn install_targets() -> InstallTargets {
    install_targets_from(
        env_path(INSTALL_OVERRIDE_ENV),
        &local_appdata(),
        crate::win::known_folder(crate::win::Folder::Desktop),
        crate::win::known_folder(crate::win::Folder::StartMenuPrograms),
    )
}

/// (Pure: the tests give it the folders.)
pub fn install_targets_from(
    over: Option<PathBuf>,
    local_appdata: &Path,
    desktop: Option<PathBuf>,
    start_menu: Option<PathBuf>,
) -> InstallTargets {
    match over {
        Some(root) => InstallTargets {
            desktop_dir: root.join("shortcuts").join("Desktop"),
            start_menu_dir: root.join("shortcuts").join("Start Menu"),
            program_dir: root.join("program"),
            uninstall_key: format!("{APP_NAME}-trial"),
            trial: true,
        },
        None => InstallTargets {
            program_dir: local_appdata.join("Programs").join(DISPLAY_NAME),
            desktop_dir: desktop.unwrap_or_else(|| local_appdata.join(APP_NAME).join("shortcuts")),
            start_menu_dir: start_menu
                .unwrap_or_else(|| local_appdata.join(APP_NAME).join("shortcuts")),
            uninstall_key: APP_NAME.to_owned(),
            trial: false,
        },
    }
}

/// The folder the running launcher is in.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

/// Whether two paths name the same place (as far as can be told without
/// both existing).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let text = p.to_string_lossy().replace('/', "\\").to_lowercase();
        text.trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .to_owned()
    };
    norm(a) == norm(b)
}

/// Whether `inner` is `outer` or below it, by components (no file access
/// but to resolve what exists).
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let text = p.to_string_lossy().replace('/', "\\").to_lowercase();
        text.trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .to_owned()
    };
    let (inner, outer) = (norm(inner), norm(outer));
    inner == outer || inner.starts_with(&(outer + "\\"))
}

/// Whether the running launcher is the installed one.
pub fn running_installed(targets: &InstallTargets) -> bool {
    same_path(&exe_dir(), &targets.program_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_install_is_per_user_programs() {
        let t = install_targets_from(
            None,
            Path::new(r"C:\Users\someone\AppData\Local"),
            Some(PathBuf::from(r"C:\Users\someone\Desktop")),
            Some(PathBuf::from(
                r"C:\Users\someone\AppData\Roaming\Microsoft\Windows\Start Menu\Programs",
            )),
        );
        assert_eq!(
            t.program_dir,
            PathBuf::from(r"C:\Users\someone\AppData\Local\Programs\VI")
        );
        assert_eq!(t.desktop_dir, PathBuf::from(r"C:\Users\someone\Desktop"));
        assert_eq!(t.uninstall_key, "VIctory");
        assert!(!t.trial);
    }

    #[test]
    fn override_keeps_everything_below_it() {
        let root = PathBuf::from(r"D:\trial");
        let t = install_targets_from(
            Some(root.clone()),
            Path::new(r"C:\x"),
            Some(PathBuf::from(r"C:\Desktop")),
            None,
        );
        assert!(t.trial);
        for p in [&t.program_dir, &t.desktop_dir, &t.start_menu_dir] {
            assert!(is_within(p, &root), "{}", p.display());
        }
        assert_ne!(t.uninstall_key, "VIctory");
    }

    #[test]
    fn within_is_by_component_and_case_blind() {
        assert!(is_within(
            Path::new(r"C:\Cache\shaders\00"),
            Path::new(r"c:\cache")
        ));
        assert!(is_within(Path::new(r"C:\cache"), Path::new(r"C:\cache\")));
        assert!(!is_within(
            Path::new(r"C:\cache-other\x"),
            Path::new(r"C:\cache")
        ));
        assert!(!is_within(Path::new(r"C:\"), Path::new(r"C:\cache")));
    }

    #[test]
    fn same_path_ignores_case_and_trailing_separator() {
        assert!(same_path(
            Path::new(r"C:\Programs\VIctory\"),
            Path::new(r"c:\programs\victory")
        ));
        assert!(!same_path(
            Path::new(r"C:\Programs\VIctory"),
            Path::new(r"C:\Programs")
        ));
    }
}
