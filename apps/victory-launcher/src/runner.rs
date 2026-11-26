//! Starting `victory.exe`: the game (its output to a log file; it goes on
//! if the launcher is closed) and the shader steps (their output read line
//! by line, at below-normal priority, in a job that Cancel ends whole).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::settings::{self, Settings};
use crate::win;

/// A command, as it will be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The `VICTORY_*` variables; every other `VICTORY_*` the launcher
    /// itself was started with is taken away.
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

impl Spec {
    /// As one would type it, for messages and logs.
    pub fn command_line(&self) -> String {
        let quote = |s: &str| {
            if s.contains(' ') || s.is_empty() {
                format!("\"{s}\"")
            } else {
                s.to_owned()
            }
        };
        std::iter::once(quote(&self.program.display().to_string()))
            .chain(self.args.iter().map(|a| quote(a)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null());
        // Left-over developer switches of the window the launcher was
        // started from must not reach the game.
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("VICTORY_")
            {
                command.env_remove(name);
            }
        }
        command.envs(&self.env);
        command
    }
}

pub fn play_spec(s: &Settings, victory: &Path, cwd: &Path) -> Spec {
    Spec {
        program: victory.to_path_buf(),
        args: vec![
            "run".to_owned(),
            s.game_exe().display().to_string(),
            "--execute".to_owned(),
            "--console".to_owned(),
            s.console.word().to_owned(),
        ],
        env: settings::build_env(s),
        cwd: cwd.to_path_buf(),
    }
}

pub fn shaders_spec(s: &Settings, victory: &Path, cwd: &Path, progress_flag: bool) -> Spec {
    let mut args = vec![
        "shaders".to_owned(),
        s.shader_archive().display().to_string(),
    ];
    if progress_flag {
        args.push("--progress".to_owned());
    }
    Spec {
        program: victory.to_path_buf(),
        args,
        env: settings::build_env(s),
        cwd: cwd.to_path_buf(),
    }
}

pub fn warm_spec(s: &Settings, victory: &Path, cwd: &Path, progress_flag: bool) -> Spec {
    let mut args = vec!["warm".to_owned(), "--used".to_owned()];
    if progress_flag {
        args.push("--progress".to_owned());
    }
    Spec {
        program: victory.to_path_buf(),
        args,
        env: settings::build_env(s),
        cwd: cwd.to_path_buf(),
    }
}

/// `20261125-213045`, in UTC: for log file names.
pub fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86400, secs % 86400);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (
        doy - (153 * mp + 2) / 5 + 1,
        if mp < 10 { mp + 3 } else { mp - 9 },
    );
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn spawn_error(spec: &Spec, e: &std::io::Error) -> String {
    format!(
        "Could not start {} ({e}). The command was: {}",
        spec.program.display(),
        spec.command_line()
    )
}

/// The game, started.
pub struct Game {
    child: Child,
    pub log: PathBuf,
    pub started: Instant,
    pub pid: u32,
}

impl Game {
    /// `None` while it runs; then how it ended.
    pub fn ended(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) if status.success() => Some("The game was closed.".to_owned()),
            Ok(Some(status)) => Some(format!(
                "The game ended unexpectedly ({status}). The log has the details: {}",
                self.log.display()
            )),
            Err(e) => Some(format!("Lost track of the game ({e}).")),
        }
    }
}

/// Starts the game with its output in `<logs>\run-<stamp>.log` (the
/// command and environment written first).
pub fn start_game(spec: &Spec, logs: &Path) -> Result<Game, String> {
    std::fs::create_dir_all(logs)
        .map_err(|e| format!("Could not create {}: {e}", logs.display()))?;
    std::fs::create_dir_all(&spec.cwd)
        .map_err(|e| format!("Could not create {}: {e}", spec.cwd.display()))?;
    let log = logs.join(format!("run-{}.log", stamp()));
    let mut file = std::fs::File::create(&log)
        .map_err(|e| format!("Could not create {}: {e}", log.display()))?;
    let _ = writeln!(file, "launcher: {}", spec.command_line());
    for (name, value) in &spec.env {
        let _ = writeln!(file, "launcher: {name}={value}");
    }
    let _ = file.flush();
    let cloned = |f: &std::fs::File| {
        f.try_clone()
            .map_err(|e| format!("Could not write {}: {e}", log.display()))
    };
    let (out, err) = (cloned(&file)?, cloned(&file)?);
    let child = spec
        .command()
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .creation_flags(win::CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| spawn_error(spec, &e))?;
    let _ = std::fs::write(logs.join("latest.txt"), log.display().to_string());
    Ok(Game {
        pid: child.id(),
        child,
        log,
        started: Instant::now(),
    })
}

/// What a running step sends.
#[derive(Debug)]
pub enum Event {
    Line(String),
    /// It is over: the exit code, if it ended by itself.
    Ended(Option<i32>),
}

/// A shader step, running.
pub struct Step {
    pub events: Receiver<Event>,
    pub started: Instant,
    pub command_line: String,
    pub log: Option<PathBuf>,
    job: Option<win::Job>,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
}

impl Step {
    /// Ends the step and every process it started.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        match &self.job {
            Some(job) => job.end(),
            None => {
                let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
            }
        }
    }

    pub fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        // (The job's own drop ends what is left in it.)
        if self.job.is_none() {
            let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }
    }
}

fn pump(
    reader: impl std::io::Read + Send + 'static,
    tx: Sender<Event>,
    log: Option<Arc<Mutex<std::fs::File>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = plain(String::from_utf8_lossy(&raw).trim_end_matches(['\r', '\n']));
            if let Some(log) = &log {
                let _ = writeln!(log.lock().unwrap_or_else(|e| e.into_inner()), "{line}");
            }
            if tx.send(Event::Line(line)).is_err() {
                break;
            }
            wake();
        }
    })
}

/// A line without the colour codes a log writes for a terminal
/// (`ESC [ ... letter`).
pub fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Starts a step at below-normal priority, without a window. Each line of
/// its output (both streams) arrives as an [`Event`]; `wake` is called
/// after each, to have the window look.
pub fn start_step(
    spec: &Spec,
    logs: &Path,
    name: &str,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<Step, String> {
    let _ = std::fs::create_dir_all(logs);
    let _ = std::fs::create_dir_all(&spec.cwd);
    let log_path = logs.join(format!("{name}-{}.log", stamp()));
    let log = std::fs::File::create(&log_path).ok().map(|mut f| {
        let _ = writeln!(f, "launcher: {}", spec.command_line());
        Arc::new(Mutex::new(f))
    });
    let job = win::Job::new();
    let mut child = spec
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(win::CREATE_NO_WINDOW | win::BELOW_NORMAL_PRIORITY_CLASS)
        .spawn()
        .map_err(|e| spawn_error(spec, &e))?;
    let job = job.filter(|j| j.add(&child));
    let (tx, events) = channel();
    let out = child
        .stdout
        .take()
        .map(|s| pump(s, tx.clone(), log.clone(), wake.clone()));
    let err = child
        .stderr
        .take()
        .map(|s| pump(s, tx.clone(), log.clone(), wake.clone()));
    let child = Arc::new(Mutex::new(child));
    let waiter = child.clone();
    std::thread::spawn(move || {
        for pump in [out, err].into_iter().flatten() {
            let _ = pump.join();
        }
        // Both streams are closed: it has ended, or is about to.
        let code = loop {
            match waiter.lock().unwrap_or_else(|e| e.into_inner()).try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {}
                Err(_) => break None,
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let _ = tx.send(Event::Ended(code));
        wake();
    });
    Ok(Step {
        events,
        started: Instant::now(),
        command_line: spec.command_line(),
        log: log.is_some().then_some(log_path),
        job,
        child,
        cancelled: Arc::new(AtomicBool::new(false)),
    })
}

/// Runs a short command and returns what it printed (both streams), or
/// `None` if it could not be started or took longer than `limit`.
pub fn capture(program: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(win::CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if began.elapsed() > limit => {
                let _ = child.kill();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    Some(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// What is known of a `victory.exe`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VictoryInfo {
    pub path: PathBuf,
    /// `victory 0.1.0`, as it says itself.
    pub version: String,
    /// Version and the first digits of the file's SHA-256: what the caches
    /// are remembered to have been made with.
    pub build: String,
    /// Its `shaders` and `warm` take `--progress`.
    pub progress_flag: bool,
}

/// Asks a `victory.exe` what it is (two short `--help`-like runs and a hash
/// of the file). `Err` in words if it is not there or does not answer.
pub fn inspect_victory(path: &Path) -> Result<VictoryInfo, String> {
    if !path.is_file() {
        return Err(format!("victory.exe was not found at {}", path.display()));
    }
    let hash = crate::update::sha256_file(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let limit = Duration::from_secs(20);
    let version = capture(path, &["--version"], limit)
        .map(|v| v.lines().next().unwrap_or_default().trim().to_owned())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "{} did not answer when asked for its version",
                path.display()
            )
        })?;
    let progress_flag =
        capture(path, &["warm", "--help"], limit).is_some_and(|help| help.contains("--progress"));
    Ok(VictoryInfo {
        path: path.to_path_buf(),
        build: format!("{version} ({})", &hash[..12]),
        version,
        progress_flag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Console, Preset};

    fn sample(preset: Preset) -> Settings {
        Settings {
            game_dir: r"D:\Games\My Copy".into(),
            preset,
            ..Settings::default()
        }
    }

    #[test]
    fn play_runs_the_title_executable_with_execute_and_the_console() {
        let mut s = sample(Preset::Recommended);
        s.console = Console::SeriesS;
        let spec = play_spec(&s, Path::new(r"C:\p\victory.exe"), Path::new(r"C:\home"));
        assert_eq!(
            spec.args,
            [
                "run",
                r"D:\Games\My Copy\game_scarlett_master.exe",
                "--execute",
                "--console",
                "series-s"
            ]
        );
        assert_eq!(
            spec.command_line(),
            r#"C:\p\victory.exe run "D:\Games\My Copy\game_scarlett_master.exe" --execute --console series-s"#
        );
        assert_eq!(spec.env, settings::build_env(&s));
    }

    #[test]
    fn steps_name_the_archive_and_ask_for_progress_only_if_understood() {
        let s = sample(Preset::Safe);
        let victory = Path::new(r"C:\p\victory.exe");
        let cwd = Path::new(r"C:\home");
        assert_eq!(
            shaders_spec(&s, victory, cwd, false).args,
            ["shaders", r"D:\Games\My Copy\shaders.rpf"]
        );
        assert_eq!(
            shaders_spec(&s, victory, cwd, true).args.last().unwrap(),
            "--progress"
        );
        assert_eq!(warm_spec(&s, victory, cwd, false).args, ["warm", "--used"]);
        assert_eq!(
            warm_spec(&s, victory, cwd, true).args,
            ["warm", "--used", "--progress"]
        );
    }

    fn cmd() -> PathBuf {
        PathBuf::from(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into()))
    }

    /// The environment a child really gets, for each preset: `cmd /c set`
    /// stands in for the game.
    #[test]
    fn a_child_gets_exactly_the_built_environment() {
        // SAFETY: (test) set before any child is started; no other test
        // reads this name.
        unsafe { std::env::set_var("VICTORY_KEYS", "10:Enter") };
        for preset in [Preset::Recommended, Preset::Safe, Preset::Custom] {
            let s = sample(preset);
            let dir = tempfile::tempdir().unwrap();
            let spec = Spec {
                program: cmd(),
                args: vec!["/d".into(), "/c".into(), "set".into(), "VICTORY_".into()],
                env: settings::build_env(&s),
                cwd: dir.path().to_path_buf(),
            };
            let step = start_step(&spec, dir.path(), "test", Arc::new(|| {})).unwrap();
            let mut seen = BTreeMap::new();
            let code = loop {
                match step.events.recv_timeout(Duration::from_secs(30)).unwrap() {
                    Event::Line(line) => {
                        if let Some((name, value)) = line.split_once('=') {
                            seen.insert(name.to_owned(), value.to_owned());
                        }
                    }
                    Event::Ended(code) => break code,
                }
            };
            assert_eq!(code, Some(0));
            assert_eq!(seen, spec.env, "{preset:?}");
            assert!(
                !seen.contains_key("VICTORY_KEYS"),
                "a developer switch of the parent leaked"
            );
            assert_eq!(
                seen.contains_key("VICTORY_THIN"),
                preset == Preset::Recommended
            );
        }
    }

    #[test]
    fn cancel_ends_a_step_and_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        // A command that starts another and both wait a long time.
        let spec = Spec {
            program: cmd(),
            args: vec![
                "/d".into(),
                "/c".into(),
                "echo started& ping -n 120 127.0.0.1 >nul".into(),
            ],
            env: BTreeMap::new(),
            cwd: dir.path().to_path_buf(),
        };
        let step = start_step(&spec, dir.path(), "test", Arc::new(|| {})).unwrap();
        match step.events.recv_timeout(Duration::from_secs(30)).unwrap() {
            Event::Line(line) => assert_eq!(line, "started"),
            other => panic!("{other:?}"),
        }
        let began = Instant::now();
        step.cancel();
        loop {
            if let Event::Ended(_) = step.events.recv_timeout(Duration::from_secs(30)).unwrap() {
                break;
            }
        }
        assert!(began.elapsed() < Duration::from_secs(20));
        assert!(step.was_cancelled());
    }

    #[test]
    fn the_game_stand_in_logs_its_command_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let s = sample(Preset::Safe);
        let mut spec = play_spec(&s, &cmd(), dir.path());
        // (cmd does not take `run ...`: give it something it does.)
        spec.args = vec!["/d".into(), "/c".into(), "set".into(), "VICTORY_".into()];
        let mut game = start_game(&spec, &dir.path().join("logs")).unwrap();
        let began = Instant::now();
        let ended = loop {
            if let Some(ended) = game.ended() {
                break ended;
            }
            assert!(began.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(ended, "The game was closed.");
        let log = std::fs::read_to_string(&game.log).unwrap();
        assert!(log.contains("launcher: VICTORY_RELAY=2"), "{log}");
        assert!(log.contains("\nVICTORY_RELAY=2"), "{log}");
        assert!(!log.contains("VICTORY_THIN"), "{log}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("logs").join("latest.txt")).unwrap(),
            game.log.display().to_string()
        );
    }

    #[test]
    fn a_program_that_is_not_there_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec {
            program: dir.path().join("no-such.exe"),
            args: vec!["warm".into()],
            env: BTreeMap::new(),
            cwd: dir.path().to_path_buf(),
        };
        let Err(e) = start_step(&spec, dir.path(), "test", Arc::new(|| {})) else {
            panic!()
        };
        assert!(e.contains("no-such.exe") && e.contains("warm"), "{e}");
    }

    #[test]
    fn colour_codes_are_taken_out_of_output() {
        let line = "\u{1b}[2m2026-11-26T06:47:48Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m victory: \u{1b}[3mlevel\u{1b}[0m=default";
        assert_eq!(
            plain(line),
            "2026-11-26T06:47:48Z  INFO victory: level=default"
        );
        assert_eq!(plain("PROGRESS 3 9"), "PROGRESS 3 9");
        assert_eq!(plain("a [not a code] b"), "a [not a code] b");
    }

    #[test]
    fn stamps_are_dates() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20") && s.as_bytes()[8] == b'-', "{s}");
    }
}
