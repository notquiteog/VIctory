//! `victory-launcher`: the window ordinary players start the game from.
//! Install, first-run setup, Play, settings, shader and pipeline
//! preparation with progress, and updates.
//!
//! It only ever starts `victory.exe` with arguments and `VICTORY_*`
//! environment variables; it contains nothing of the game and never writes
//! to the game folder.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod caches;
mod install;
mod paths;
mod progress;
mod runner;
mod settings;
mod ui;
mod update;
mod win;

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use settings::Settings;

/// What the launcher was started with. Only `--victory-exe` and
/// `--uninstall` are for people; the rest is for scripts and tests.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Args {
    /// A `victory.exe` elsewhere than beside the launcher.
    pub victory_exe: Option<PathBuf>,
    /// Remove the program (what "Installed apps" runs).
    pub uninstall: bool,
    /// (The copy in the temporary folder that does the removing.)
    pub uninstall_run: bool,
    /// Without a window or a question.
    pub yes: bool,
    /// With `--uninstall --yes`: saves, caches and settings too.
    pub remove_data: bool,
    /// Install without a window and leave.
    pub install: bool,
    /// Write the Play command and its environment to this file and leave.
    pub print_env: Option<PathBuf>,
    /// Save a picture of the window to this file after `--after` seconds, and leave.
    pub screenshot: Option<PathBuf>,
    pub after: f32,
    /// The tab to open on.
    pub tab: Option<String>,
    /// Press a button at start: `prepare`, `check-update`.
    pub auto: Option<String>,
    /// (Set by the launcher when it restarts itself after an update.)
    pub updated: Option<String>,
    /// `--make-setup <victory.exe> <out.exe>`: write the one-file setup
    /// program (this launcher with that `victory.exe` packed behind it).
    pub make_setup: Option<(PathBuf, PathBuf)>,
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        after: 2.0,
        ..Args::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--victory-exe" => out.victory_exe = Some(PathBuf::from(value("--victory-exe")?)),
            "--uninstall" => out.uninstall = true,
            "--uninstall-run" => out.uninstall_run = true,
            "--yes" => out.yes = true,
            "--remove-data" => out.remove_data = true,
            "--install" => out.install = true,
            "--print-env" => out.print_env = Some(PathBuf::from(value("--print-env")?)),
            "--screenshot" => out.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--after" => {
                out.after = value("--after")?
                    .parse()
                    .map_err(|_| "--after needs seconds".to_owned())?
            }
            "--tab" => out.tab = Some(value("--tab")?),
            "--auto" => out.auto = Some(value("--auto")?),
            "--updated" => out.updated = Some(value("--updated")?),
            "--make-setup" => {
                let victory = PathBuf::from(value("--make-setup")?);
                out.make_setup = Some((victory, PathBuf::from(value("--make-setup")?)));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(out)
}

/// A line in `<home>\logs\launcher.log` (the program has no console).
pub fn log(line: &str) {
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("launcher.log"))
    {
        let _ = writeln!(file, "{} {line}", runner::stamp());
    }
}

/// Starts a copy of the launcher from the temporary folder to do the
/// removing (a program cannot delete the file it runs from).
pub fn relay_uninstall(args: &Args) -> Result<(), String> {
    let me = std::env::current_exe()
        .map_err(|e| format!("Could not find the launcher's own file: {e}"))?;
    let copy = std::env::temp_dir().join(format!("vi-uninstall-{}.exe", std::process::id()));
    std::fs::copy(&me, &copy)
        .map_err(|e| format!("Could not copy {} to {}: {e}", me.display(), copy.display()))?;
    let mut command = Command::new(&copy);
    command
        .arg("--uninstall-run")
        .current_dir(std::env::temp_dir());
    if args.yes {
        command.arg("--yes");
    }
    if args.remove_data {
        command.arg("--remove-data");
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {}: {e}", copy.display()))
}

/// Has the temporary copy delete itself a moment after it has ended.
pub fn delete_self_later() {
    let Ok(me) = std::env::current_exe() else {
        return;
    };
    if !paths::is_within(&me, &std::env::temp_dir()) {
        return;
    }
    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let _ = Command::new(comspec)
        .raw_arg(format!(
            "/d /c ping -n 4 127.0.0.1 >nul & del /f /q \"{}\"",
            me.display()
        ))
        .current_dir(std::env::temp_dir())
        .creation_flags(win::CREATE_NO_WINDOW)
        .spawn();
}

fn headless_uninstall(args: &Args) -> i32 {
    let targets = paths::install_targets();
    let (settings, _) = Settings::load(&paths::settings_file());
    let home = paths::home();
    let notes = install::uninstall(
        &targets,
        args.remove_data.then_some((&settings, home.as_path())),
    );
    if !args.remove_data {
        for note in &notes {
            log(&format!("uninstall: {note}"));
        }
    }
    delete_self_later();
    i32::from(!notes.is_empty())
}

fn headless_install(args: &Args, settings: &Settings) -> i32 {
    let targets = paths::install_targets();
    let result = std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|me| {
            let victory =
                install::find_victory(args.victory_exe.as_deref(), &settings.developer.victory_exe);
            install::install_with_logo(
                &targets,
                &me,
                victory.as_deref(),
                paths::user_logo().as_deref(),
            )
        });
    match result {
        Ok(done) => {
            for line in done {
                log(&format!("install: {line}"));
            }
            0
        }
        Err(e) => {
            log(&format!("install failed: {e}"));
            1
        }
    }
}

/// The Play command and environment, as text.
pub fn describe_play(settings: &Settings, victory: &Path) -> String {
    let spec = runner::play_spec(settings, victory, &paths::home());
    let mut text = format!("{}\n", spec.command_line());
    for (name, value) in &spec.env {
        text.push_str(&format!("{name}={value}\n"));
    }
    text
}

/// What the installed launcher does before its window: finishes an update
/// that is waiting. `Ok(true)`: a new launcher was started; leave.
fn finish_update(targets: &paths::InstallTargets, settings: &mut Settings) -> Result<bool, String> {
    update::remove_old_files(&targets.program_dir);
    let Some((tag, channel, files)) =
        update::apply_pending(&paths::updates_dir(), &targets.program_dir)?
    else {
        return Ok(false);
    };
    install::add_to_manifest(&targets.program_dir, &files);
    settings.prepared.possibly_stale = true;
    settings.updates.installed_from = channel;
    let _ = settings.save(&paths::settings_file());
    log(&format!(
        "update: {tag} is in place ({} files)",
        files.len()
    ));
    let new = targets.program_dir.join(paths::LAUNCHER_EXE);
    Command::new(&new)
        .args(["--updated", &tag])
        .current_dir(&targets.program_dir)
        .spawn()
        .map(|_| true)
        .map_err(|e| {
            format!(
                "The update is in place but {} could not be started: {e}",
                new.display()
            )
        })
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            log(&format!("arguments: {e}"));
            std::process::exit(2);
        }
    };
    if let Some((victory, out)) = &args.make_setup {
        let made = std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|me| install::make_setup(&me, victory, out));
        if let Err(e) = &made {
            log(&format!("make-setup: {e}"));
        }
        std::process::exit(i32::from(made.is_err()));
    }
    if args.uninstall && !args.uninstall_run {
        if let Err(e) = relay_uninstall(&args) {
            log(&e);
            std::process::exit(1);
        }
        return;
    }
    if args.uninstall_run {
        if args.yes {
            std::process::exit(headless_uninstall(&args));
        }
        ui::run_uninstall();
        delete_self_later();
        return;
    }
    let (mut settings, mut notice) = Settings::load(&paths::settings_file());
    if args.install {
        std::process::exit(headless_install(&args, &settings));
    }
    if let Some(file) = &args.print_env {
        let victory =
            install::find_victory(args.victory_exe.as_deref(), &settings.developer.victory_exe)
                .unwrap_or_else(|| paths::exe_dir().join(paths::VICTORY_EXE));
        let written = std::fs::write(file, describe_play(&settings, &victory));
        std::process::exit(i32::from(written.is_err()));
    }
    let targets = paths::install_targets();
    if paths::running_installed(&targets) {
        match finish_update(&targets, &mut settings) {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log(&format!("update: {e}"));
                notice = Some(e);
            }
        }
        // The installed-apps entry follows the program that is there.
        if install::status(&targets).version.as_deref() != Some(update::current_version()) {
            let _ = install::write_registry(&targets, update::current_version());
        }
    }
    ui::run(args, settings, notice, targets);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Args, String> {
        parse_args(text.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn arguments() {
        assert_eq!(
            parse("").unwrap(),
            Args {
                after: 2.0,
                ..Args::default()
            }
        );
        let a = parse("--victory-exe C:\\b\\victory.exe --tab shaders --screenshot out.png --after 3.5 --auto prepare").unwrap();
        assert_eq!(a.victory_exe, Some(PathBuf::from(r"C:\b\victory.exe")));
        assert_eq!(
            (a.tab.as_deref(), a.auto.as_deref(), a.after),
            (Some("shaders"), Some("prepare"), 3.5)
        );
        let a = parse("--uninstall --yes --remove-data").unwrap();
        assert!(a.uninstall && a.yes && a.remove_data && !a.uninstall_run);
        assert!(parse("--victory-exe").is_err());
        assert!(parse("--frobnicate").is_err());
    }

    #[test]
    fn the_play_command_is_described_with_its_environment() {
        let s = Settings {
            game_dir: r"D:\Games\Mine".into(),
            ..Settings::default()
        };
        let text = describe_play(&s, Path::new(r"C:\p\victory.exe"));
        let mut lines = text.lines();
        assert_eq!(
            lines.next().unwrap(),
            r"C:\p\victory.exe run D:\Games\Mine\game_scarlett_master.exe --execute --console series-x"
        );
        assert!(
            text.contains("\nVICTORY_THIN=1\n")
                && text.contains("\nVICTORY_GAME_DIR=D:\\Games\\Mine\n")
        );
        assert!(!text.contains("VICTORY_KEYS"));
    }
}
