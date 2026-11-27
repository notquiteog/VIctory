//! The window: one page per job (Play, Setup, Settings, Shaders, Updates,
//! Install). Anything that takes time runs on a thread of its own and
//! reports back through a channel; the window never waits.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, RichText};

use crate::paths::{self, DISPLAY_NAME, InstallTargets};
use crate::progress::{self, Progress};
use crate::runner::{self, Event, VictoryInfo, WarmWhat};
use crate::settings::{
    self, Channel, Console, FrameGen, Preset, Quality, Resolution, Settings, Upscaler, WarmMode,
    help,
};
use crate::update::{self, Feed, Found, Offer, Release, Transfer};
use crate::{Args, caches, install, win};

/// The one line every public face of the project carries.
pub const NOTICE: &str = "VI is an unofficial community project. It is not affiliated with, endorsed by or supported by \
                          Rockstar Games or Take-Two Interactive. It contains no game code or data; you must own the game.";

/// The picture to show: the user's own `logo.png` if there is one that can
/// be read, else the built-in. With whether it is somebody's own (those go
/// on a light tile: line art is often dark on nothing).
fn logo_picture() -> (image::RgbaImage, bool) {
    let own = paths::user_logo().and_then(|path| image::open(path).ok());
    match own {
        Some(picture) => (picture.to_rgba8(), true),
        None => (
            image::load_from_memory(install::LOGO)
                .map(|p| p.to_rgba8())
                .unwrap_or_else(|_| image::RgbaImage::new(1, 1)),
            install::LOGO_IS_OWN,
        ),
    }
}
/// The commit limit (memory plus page file) below which a warning shows.
const WANTED_COMMIT: u64 = 62 << 30;
const TAIL_LINES: usize = 400;
/// (How "another victory.exe runs" begins, to know the note again.)
const ALREADY_RUNNING: &str = "is already running";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Play,
    Setup,
    Settings,
    Shaders,
    Updates,
    Install,
}

impl Tab {
    const ALL: [(Tab, &'static str); 6] = [
        (Tab::Play, "Play"),
        (Tab::Setup, "Setup"),
        (Tab::Settings, "Settings"),
        (Tab::Shaders, "Shaders"),
        (Tab::Updates, "Updates"),
        (Tab::Install, "Install"),
    ];

    fn from_word(word: &str) -> Option<Tab> {
        Tab::ALL
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(word))
            .map(|&(tab, _)| tab)
    }
}

/// What the PC has.
#[derive(Debug, Clone, Default)]
struct SysInfo {
    adapter: Option<win::Adapter>,
    /// Every hardware card, in the system's order.
    adapters: Vec<win::Adapter>,
    ram: u64,
    commit: u64,
    free_disk: Option<u64>,
}

/// What the workers report.
enum Msg {
    Victory(Result<VictoryInfo, String>),
    Sys(SysInfo),
    Cache(caches::Status),
    UpdateChecked(Result<Found, String>, Channel),
    UpdateStaged(Result<String, String>),
    Installed(Result<Vec<String>, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Shaders,
    Warm(WarmWhat),
}

impl StepKind {
    fn title(self) -> &'static str {
        match self {
            StepKind::Shaders => "Preparing shaders",
            StepKind::Warm(WarmWhat::Used) => "Compiling the pipelines earlier sessions used",
            StepKind::Warm(WarmWhat::All) => "Compiling every pipeline the game has created",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            StepKind::Shaders => "shaders",
            StepKind::Warm(_) => "pipelines",
        }
    }
}

/// Why a step runs: the Shaders page's buttons, or Play's own steps around
/// a session (`play.ps1`'s warm-ups).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Prepare,
    BeforePlay,
    AfterPlay,
}

struct ActiveStep {
    kind: StepKind,
    purpose: Purpose,
    step: runner::Step,
    progress: Progress,
    tail: VecDeque<String>,
    last_line: String,
}

enum UpdateState {
    Idle,
    Checking,
    UpToDate(String),
    NoReleases,
    /// A release to offer; `true` if it is the way back from a latest
    /// build to the stable release (which may be older).
    Available(Release, bool),
    Downloading(Release, Arc<Transfer>),
    Ready(String),
    Failed(String),
}

enum Confirm {
    ClearShaders,
    ClearPipelines { forget: bool },
    PrepareBeforePlay,
    BackToStable(Box<Release>),
}

/// Whether the shaders in the cache are this `victory.exe`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Freshness {
    /// `victory.exe` has not answered yet.
    Unknown,
    Never,
    OtherBuild(String),
    AfterUpdate,
    Ready,
}

fn freshness(prepared_build: &str, possibly_stale: bool, current: Option<&str>) -> Freshness {
    match current {
        None => Freshness::Unknown,
        Some(_) if prepared_build.is_empty() => Freshness::Never,
        Some(current) if prepared_build != current => {
            Freshness::OtherBuild(prepared_build.to_owned())
        }
        Some(_) if possibly_stale => Freshness::AfterUpdate,
        Some(_) => Freshness::Ready,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn ago(when: u64) -> String {
    if when == 0 {
        return "never".to_owned();
    }
    let secs = now_secs().saturating_sub(when);
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

struct App {
    args: Args,
    settings: Settings,
    saved: Settings,
    notice: Option<String>,
    tab: Tab,
    targets: InstallTargets,
    installed: install::Installed,
    running_installed: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    victory: Option<Result<VictoryInfo, String>>,
    sys: Option<SysInfo>,
    cache: Option<caches::Status>,
    cache_scanning: bool,
    game: Option<runner::Game>,
    game_note: Option<(bool, String)>,
    /// The log of the session that ended badly, and what it said last.
    crash: Option<(PathBuf, Vec<String>)>,
    /// `victory.exe` processes this window did not start (another window,
    /// a script): (process id, name).
    others: Vec<(u32, String)>,
    others_looked: Option<Instant>,
    queue: VecDeque<(StepKind, Purpose)>,
    active: Option<ActiveStep>,
    /// Cancel was "Skip and play now": the game starts when the step ends.
    skip_to_play: bool,
    /// Play was pressed while a warm-up after a session was stopping:
    /// (with shaders first) once it has.
    pending_play: Option<bool>,
    /// The Play button has been given the keyboard focus once.
    focused: bool,
    /// How the last step (or run of steps) came out: (went well, words).
    step_note: Option<(bool, String)>,
    last_tail: VecDeque<String>,
    play_after_steps: bool,
    confirm: Option<Confirm>,
    update: UpdateState,
    install_busy: bool,
    install_note: Option<Result<Vec<String>, String>>,
    /// The running program is the one-file setup: it carries `victory.exe`.
    carries_victory: bool,
    logo: Option<(egui::TextureHandle, bool)>,
    logo_note: Option<(bool, String)>,
    started: Instant,
    auto_done: bool,
    shot: Arc<AtomicBool>,
    shot_asked: bool,
}

pub fn run(args: Args, settings: Settings, notice: Option<String>, targets: InstallTargets) {
    win::com_init();
    let icon = Some({
        let rgba = image::imageops::resize(
            &logo_picture().0,
            128,
            128,
            image::imageops::FilterType::Lanczos3,
        );
        let (width, height) = rgba.dimensions();
        Arc::new(egui::IconData {
            rgba: rgba.into_raw(),
            width,
            height,
        })
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(DISPLAY_NAME)
        .with_inner_size([940.0, 640.0])
        .with_min_inner_size([760.0, 520.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        DISPLAY_NAME,
        options,
        Box::new(move |cc| {
            Ok(Box::new(App::new(
                &cc.egui_ctx,
                args,
                settings,
                notice,
                targets,
            )))
        }),
    );
    if let Err(e) = result {
        crate::log(&format!("window: {e}"));
        win::message_box(
            &format!("{DISPLAY_NAME} launcher"),
            &format!(
                "The launcher's window could not be opened ({e}).\n\nIt draws with OpenGL: update the graphics driver, or start it on the PC itself rather than over Remote Desktop. This is noted in {}.",
                paths::logs_dir().join("launcher.log").display()
            ),
        );
    }
}

impl App {
    fn new(
        ctx: &egui::Context,
        args: Args,
        settings: Settings,
        mut notice: Option<String>,
        targets: InstallTargets,
    ) -> Self {
        let (tx, rx) = channel();
        let installed = install::status(&targets);
        let running_installed = paths::running_installed(&targets);
        let tab = args.tab.as_deref().and_then(Tab::from_word).unwrap_or(
            if !running_installed && !installed.complete() {
                Tab::Install
            } else if !settings.setup_done
                || settings::game_folder_problem(&settings.game_dir).is_some()
            {
                Tab::Setup
            } else {
                Tab::Play
            },
        );
        if let Some(tag) = &args.updated {
            notice = Some(format!(
                "Updated to {tag}. Your settings, saves and caches were kept; shaders may need preparing again (Shaders tab)."
            ));
        }
        let update = match update::pending(&paths::updates_dir()) {
            Some((tag, _)) => UpdateState::Ready(tag),
            None => UpdateState::Idle,
        };
        let mut app = App {
            saved: settings.clone(),
            settings,
            notice,
            tab,
            installed,
            running_installed,
            targets,
            tx,
            rx,
            victory: None,
            sys: None,
            cache: None,
            cache_scanning: false,
            game: None,
            game_note: None,
            crash: None,
            others: Vec::new(),
            others_looked: None,
            queue: VecDeque::new(),
            active: None,
            skip_to_play: false,
            pending_play: None,
            focused: false,
            step_note: None,
            last_tail: VecDeque::new(),
            play_after_steps: false,
            confirm: None,
            update,
            install_busy: false,
            install_note: None,
            carries_victory: std::env::current_exe()
                .is_ok_and(|me| install::payload(&me).is_some()),
            logo: None,
            logo_note: None,
            started: Instant::now(),
            auto_done: false,
            shot: Arc::new(AtomicBool::new(false)),
            shot_asked: false,
            args,
        };
        app.inspect_victory(ctx);
        app.inspect_system(ctx);
        app.scan_cache(ctx);
        if update::check_due(
            app.settings.updates.check_on_start,
            app.settings.updates.last_check,
            now_secs(),
        ) && matches!(app.update, UpdateState::Idle)
        {
            app.check_update(ctx);
        }
        app
    }

    /// Runs `work` on a thread and sends what it returns to the window.
    fn spawn(&self, ctx: &egui::Context, work: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    fn victory_path(&self) -> Option<PathBuf> {
        install::find_victory(
            self.args.victory_exe.as_deref(),
            &self.settings.developer.victory_exe,
        )
    }

    fn victory_info(&self) -> Option<&VictoryInfo> {
        self.victory.as_ref().and_then(|v| v.as_ref().ok())
    }

    fn inspect_victory(&mut self, ctx: &egui::Context) {
        self.victory = None;
        let path = self.victory_path();
        let beside = paths::exe_dir().join(paths::VICTORY_EXE);
        let carries = self.carries_victory;
        self.spawn(ctx, move || {
            Msg::Victory(match path {
                Some(path) => runner::inspect_victory(&path),
                None if carries => Err(format!(
                    "This is the setup program. Install {DISPLAY_NAME} first (Install tab), then start it from the Desktop or the Start menu."
                )),
                None => Err(format!(
                    "victory.exe was not found at {}. It belongs in the same folder as the launcher; download the release again or reinstall.",
                    beside.display()
                )),
            })
        });
    }

    fn inspect_system(&mut self, ctx: &egui::Context) {
        let cache = self.settings.cache_path();
        self.spawn(ctx, move || {
            let (ram, commit) = win::memory().unwrap_or_default();
            let adapters = win::adapters();
            Msg::Sys(SysInfo {
                adapter: adapters.iter().cloned().reduce(|best, a| {
                    if a.vram_bytes > best.vram_bytes {
                        a
                    } else {
                        best
                    }
                }),
                adapters,
                ram,
                commit,
                free_disk: win::free_disk(&cache),
            })
        });
    }

    fn scan_cache(&mut self, ctx: &egui::Context) {
        if self.cache_scanning {
            return;
        }
        self.cache_scanning = true;
        let cache = self.settings.cache_path();
        self.spawn(ctx, move || Msg::Cache(caches::scan(&cache)));
    }

    fn feed(&self) -> Feed {
        Feed::choose(
            std::env::var(update::FEED_ENV).ok(),
            &self.settings.developer.update_feed,
            self.settings.updates.channel,
        )
    }

    fn check_update(&mut self, ctx: &egui::Context) {
        self.update = UpdateState::Checking;
        self.settings.updates.last_check = now_secs();
        let (feed, channel) = (self.feed(), self.settings.updates.channel);
        self.spawn(ctx, move || {
            Msg::UpdateChecked(update::fetch_latest(&feed, channel), channel)
        });
    }

    fn download_update(&mut self, ctx: &egui::Context, release: Release) {
        let transfer = Arc::new(Transfer::default());
        self.update = UpdateState::Downloading(release.clone(), transfer.clone());
        let (feed, channel) = (self.feed(), self.settings.updates.channel);
        self.spawn(ctx, move || {
            Msg::UpdateStaged(
                update::download_and_stage(
                    &feed,
                    &release,
                    channel,
                    &paths::updates_dir(),
                    &transfer,
                )
                .map(|()| update::release_name(&release)),
            )
        });
    }

    fn shader_freshness(&self) -> Freshness {
        let p = &self.settings.prepared;
        freshness(
            &p.shaders_build,
            p.possibly_stale,
            self.victory_info().map(|v| v.build.as_str()),
        )
    }

    fn busy(&self) -> bool {
        self.active.is_some() || !self.queue.is_empty()
    }

    /// Only the warm-up after a session runs: Play may stop it and start.
    fn only_after_play(&self) -> bool {
        self.queue.is_empty()
            && self
                .active
                .as_ref()
                .is_some_and(|a| a.purpose == Purpose::AfterPlay)
    }

    /// Looks (every two seconds at most) for `victory.exe` processes that
    /// this window did not start: a game started from elsewhere.
    fn look_for_others(&mut self) {
        if self
            .others_looked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        self.others_looked = Some(Instant::now());
        let Some(victory) = self.victory_info() else {
            self.others.clear();
            return;
        };
        let name = victory
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| paths::VICTORY_EXE.to_owned());
        // (While a step or the game of this window runs, the processes it
        // starts are its own; it is looked again before the next start.)
        if self.game.is_some() || self.active.is_some() {
            self.others.clear();
            return;
        }
        self.others = win::processes_named(&[&name, runner::SMOOTH_MOTION_EXE]);
        if self.others.is_empty() {
            // What was said of them is over.
            for n in [&mut self.game_note, &mut self.step_note] {
                if n.as_ref().is_some_and(|(_, t)| t.contains(ALREADY_RUNNING)) {
                    *n = None;
                }
            }
        }
    }

    /// Why nothing that starts `victory.exe` may start now, if so.
    fn others_running(&self) -> Option<String> {
        let (pid, name) = self.others.first()?;
        Some(format!(
            "{name} {ALREADY_RUNNING} (process {pid}), started from somewhere else: the game, or a step of it, from another window or a script. Close it first; two at once do not work."
        ))
    }

    fn queue_steps(&mut self, ctx: &egui::Context, steps: &[(StepKind, Purpose)]) {
        if self.busy() {
            return;
        }
        self.step_note = None;
        self.queue = steps.iter().copied().collect();
        self.start_next_step(ctx);
    }

    /// The file a warm-up of `what` works from: without it there is
    /// nothing to make yet.
    fn warm_book(&self, what: WarmWhat) -> PathBuf {
        let dir = self.settings.cache_path().join(caches::PIPELINES_DIR);
        match what {
            WarmWhat::Used => dir.join("early-pipelines.txt"),
            WarmWhat::All => dir.join("known-pipelines.bin"),
        }
    }

    fn start_next_step(&mut self, ctx: &egui::Context) {
        while let Some((kind, purpose)) = self.queue.pop_front() {
            let Some(victory) = self.victory_info().cloned() else {
                self.fail_steps(
                    "victory.exe is not available, so nothing can be prepared.".to_owned(),
                );
                return;
            };
            if let Some(problem) = settings::game_folder_problem(&self.settings.game_dir) {
                self.fail_steps(problem);
                return;
            }
            self.others_looked = None;
            self.look_for_others();
            if let Some(why) = self.others_running() {
                self.fail_steps(why);
                return;
            }
            if let StepKind::Warm(what) = kind
                && std::fs::metadata(self.warm_book(what)).map_or(0, |m| m.len()) == 0
            {
                // (Around a session that is no news: the first one has nothing yet.)
                if purpose == Purpose::Prepare {
                    self.step_note = Some((
                        true,
                        "Nothing to compile yet: the game notes which pipelines it makes while you play. Play once, then come back."
                            .to_owned(),
                    ));
                }
                continue;
            }
            let home = paths::home();
            let (spec, name) = match kind {
                StepKind::Shaders => (
                    runner::shaders_spec(
                        &self.settings,
                        &victory.path,
                        &home,
                        victory.progress_flag,
                    ),
                    "shaders",
                ),
                StepKind::Warm(what) => (
                    runner::warm_spec(
                        &self.settings,
                        &victory.path,
                        &home,
                        what,
                        victory.progress_flag,
                    ),
                    "warm",
                ),
            };
            let wake_ctx = ctx.clone();
            match runner::start_step(
                &spec,
                &paths::logs_dir(),
                name,
                Arc::new(move || wake_ctx.request_repaint()),
            ) {
                Ok(step) => {
                    self.last_tail.clear();
                    self.active = Some(ActiveStep {
                        kind,
                        purpose,
                        step,
                        progress: Progress::default(),
                        tail: VecDeque::new(),
                        last_line: String::new(),
                    });
                }
                Err(e) => self.fail_steps(e),
            }
            return;
        }
        // The queue ran out.
        if self.step_note.is_none() {
            self.step_note = Some((true, "Done.".to_owned()));
        }
        self.scan_cache(ctx);
        if std::mem::take(&mut self.play_after_steps) {
            self.start_game(ctx);
        }
    }

    fn fail_steps(&mut self, why: String) {
        self.queue.clear();
        self.play_after_steps = false;
        self.step_note = Some((false, why));
    }

    fn poll_step(&mut self, ctx: &egui::Context) {
        let Some(active) = &mut self.active else {
            return;
        };
        let mut ended = None;
        while let Ok(event) = active.step.events.try_recv() {
            match event {
                Event::Line(line) => {
                    active.progress.feed(&line, active.step.started.elapsed());
                    // (The machine's lines are for the bar, not for reading.)
                    if !line.trim_start().starts_with("PROGRESS ") {
                        active.last_line.clone_from(&line);
                        active.tail.push_back(line);
                        if active.tail.len() > TAIL_LINES {
                            active.tail.pop_front();
                        }
                    }
                }
                Event::Ended(code) => ended = Some(code),
            }
        }
        let Some(code) = ended else { return };
        let Some(active) = self.active.take() else {
            return;
        };
        let took = progress::clock(active.step.started.elapsed());
        self.last_tail = active.tail;
        if active.step.was_cancelled() {
            let play = std::mem::take(&mut self.skip_to_play);
            self.fail_steps(format!(
                "{} was stopped after {took}. What was done so far is kept; it goes on from there next time.",
                active.kind.title()
            ));
            self.scan_cache(ctx);
            if play {
                self.start_game(ctx);
            }
            return;
        }
        if code != Some(0) {
            let log = active.step.log.as_ref().map_or(String::new(), |l| {
                format!(" The full output is in {}.", l.display())
            });
            let said = if active.last_line.is_empty() {
                String::new()
            } else {
                format!(" It said: {}", active.last_line)
            };
            let play = self.play_after_steps && active.purpose == Purpose::BeforePlay;
            self.fail_steps(format!(
                "{} did not finish ({}).{said}{log}",
                active.kind.title(),
                runner::exit_words(code),
            ));
            self.scan_cache(ctx);
            // (Compiling ahead is a help, not a condition: the game starts anyway.)
            if play {
                self.start_game(ctx);
            }
            return;
        }
        if let Some(build) = self.victory_info().map(|v| v.build.clone()) {
            let p = &mut self.settings.prepared;
            match active.kind {
                StepKind::Shaders => {
                    p.shaders_build = build;
                    p.shaders_when = now_secs();
                    p.possibly_stale = false;
                }
                StepKind::Warm(_) => {
                    p.pipelines_build = build;
                    p.pipelines_when = now_secs();
                }
            }
        }
        self.step_note = Some((
            true,
            format!(
                "{} finished in {took}: {} {}.",
                active.kind.title(),
                progress::grouped(active.progress.done),
                active.kind.unit()
            ),
        ));
        self.start_next_step(ctx);
    }

    /// Play, as `scripts/play.ps1` runs a session: the pipelines compiled
    /// ahead (by the setting), then the game; what is compiled after it is
    /// queued when it ends.
    fn play(&mut self, ctx: &egui::Context, shaders_first: bool) {
        if self.game.is_some() {
            return;
        }
        if self.only_after_play()
            && let Some(active) = &self.active
        {
            // The warm-up after the last session gives way (it goes on next time).
            active.step.cancel();
            self.skip_to_play = false;
            return self.play_when_idle(shaders_first);
        }
        if self.busy() {
            return;
        }
        let mut steps = Vec::new();
        if shaders_first {
            steps.push((StepKind::Shaders, Purpose::BeforePlay));
        }
        if let (Some(what), _) = runner::warm_around(self.settings.warm) {
            steps.push((StepKind::Warm(what), Purpose::BeforePlay));
        }
        if steps.is_empty() {
            return self.start_game(ctx);
        }
        self.queue_steps(ctx, &steps);
        self.play_after_steps = self.busy();
        if !self.busy() && self.step_note.as_ref().is_none_or(|(ok, _)| *ok) {
            self.start_game(ctx);
        }
    }

    /// (Play pressed while the after-session warm-up ends: once it has.)
    fn play_when_idle(&mut self, shaders_first: bool) {
        self.pending_play = Some(shaders_first);
    }

    fn start_game(&mut self, ctx: &egui::Context) {
        let _ = ctx;
        if self.game.is_some() {
            return;
        }
        self.crash = None;
        if let Some(problem) = settings::game_folder_problem(&self.settings.game_dir) {
            self.game_note = Some((false, problem));
            return;
        }
        let Some(victory) = self.victory_info().cloned() else {
            self.game_note = Some((
                false,
                "victory.exe is not available, so the game cannot be started.".to_owned(),
            ));
            return;
        };
        self.others_looked = None;
        self.look_for_others();
        if let Some(why) = self.others_running() {
            self.game_note = Some((false, why));
            return;
        }
        for dir in [self.settings.cache_path(), self.settings.data_path()] {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.game_note = Some((false, format!("Could not create {}: {e}", dir.display())));
                return;
            }
        }
        if self.settings.smooth_motion
            && let Err(e) = runner::refresh_smooth_motion_copy(&victory.path)
        {
            self.game_note = Some((false, e));
            return;
        }
        let spec = runner::play_spec(&self.settings, &victory.path, &paths::home());
        match runner::start_game(&spec, &paths::logs_dir()) {
            Ok(game) => {
                self.game_note = None;
                self.game = Some(game);
            }
            Err(e) => self.game_note = Some((false, e)),
        }
    }

    /// The game has ended: say how, and compile the rest for next time.
    fn game_ended(&mut self, ctx: &egui::Context, ended: runner::Ended) {
        let Some(game) = self.game.take() else {
            return;
        };
        if !ended.ok {
            self.crash = Some((game.log.clone(), runner::log_tail(&game.log, 12)));
            crate::log(&format!("game: {}", ended.text));
        }
        self.game_note = Some((ended.ok, ended.text));
        self.scan_cache(ctx);
        if let (_, Some(what)) = runner::warm_around(self.settings.warm) {
            self.queue_steps(ctx, &[(StepKind::Warm(what), Purpose::AfterPlay)]);
        }
    }

    fn handle(&mut self, ctx: &egui::Context, msg: Msg) {
        match msg {
            Msg::Victory(v) => self.victory = Some(v),
            Msg::Sys(s) => self.sys = Some(s),
            Msg::Cache(c) => {
                self.cache = Some(c);
                self.cache_scanning = false;
            }
            // (An answer for a channel the user has since left is dropped.)
            Msg::UpdateChecked(_, channel) if channel != self.settings.updates.channel => {}
            Msg::UpdateChecked(found, channel) => {
                let own = update::Own::current(self.settings.updates.installed_from);
                self.update = match found {
                    Ok(Found::Nothing) => UpdateState::NoReleases,
                    Ok(Found::Release(r)) => match update::decide(channel, &r, own) {
                        Offer::Newer => UpdateState::Available(*r, false),
                        Offer::BackToStable => UpdateState::Available(*r, true),
                        Offer::UpToDate(words) => UpdateState::UpToDate(words),
                    },
                    Err(e) => UpdateState::Failed(e),
                };
            }
            Msg::UpdateStaged(staged) => {
                self.update = match staged {
                    Ok(tag) => UpdateState::Ready(tag),
                    Err(e) => UpdateState::Failed(e),
                };
            }
            Msg::Installed(result) => {
                self.install_busy = false;
                self.installed = install::status(&self.targets);
                self.install_note = Some(result);
            }
        }
        let _ = ctx;
    }

    /// The hooks for scripts: press a button at start, save a picture.
    fn automation(&mut self, ctx: &egui::Context) {
        if !self.auto_done && self.victory.is_some() {
            self.auto_done = true;
            match self.args.auto.as_deref() {
                Some("prepare") => self.queue_steps(
                    ctx,
                    &[
                        (StepKind::Shaders, Purpose::Prepare),
                        (StepKind::Warm(WarmWhat::Used), Purpose::Prepare),
                    ],
                ),
                Some("shaders") => self.queue_steps(ctx, &[(StepKind::Shaders, Purpose::Prepare)]),
                Some("warm-all") => {
                    self.queue_steps(ctx, &[(StepKind::Warm(WarmWhat::All), Purpose::Prepare)])
                }
                Some("check-update" | "update") => self.check_update(ctx),
                Some("play") => self.play(ctx, false),
                _ => {}
            }
        }
        // `--auto update`: the whole way, as if Update and Restart were pressed.
        if self.args.auto.as_deref() == Some("update") {
            match &self.update {
                UpdateState::Available(release, _) if self.running_installed => {
                    let release = release.clone();
                    self.download_update(ctx, release);
                }
                UpdateState::Ready(_)
                    if self.running_installed
                        && self.args.updated.is_none()
                        && !self.shot_asked =>
                {
                    // (Once: `shot_asked` doubles as "already leaving".)
                    self.shot_asked = true;
                    if std::env::current_exe()
                        .and_then(|me| std::process::Command::new(me).spawn())
                        .is_ok()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                _ => {}
            }
        }
        // (Tests: a launcher that restarted itself has no one to close it.)
        if let Some(secs) = std::env::var("VICTORY_LAUNCHER_EXIT_AFTER")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
        {
            if self.started.elapsed().as_secs_f32() >= secs {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let Some(path) = self.args.screenshot.clone() else {
            return;
        };
        if self.shot.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.shot_asked && self.started.elapsed().as_secs_f32() >= self.args.after {
            self.shot_asked = true;
            let (done, wake) = (self.shot.clone(), ctx.clone());
            ctx.request_screenshot(move |image| {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let [w, h] = image.size;
                let saved = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
                    .is_some_and(|picture| picture.save(&path).is_ok());
                if !saved {
                    crate::log(&format!("screenshot: could not write {}", path.display()));
                }
                done.store(true, Ordering::Relaxed);
                wake.request_repaint();
            });
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn error_text(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(ui.visuals().error_fg_color));
}

fn warn_text(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(ui.visuals().warn_fg_color));
}

fn good_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(110, 200, 120)
    } else {
        Color32::from_rgb(20, 120, 40)
    }
}

fn note(ui: &mut egui::Ui, (good, text): &(bool, String)) {
    if *good {
        ui.label(RichText::new(text).color(good_color(ui)));
    } else {
        error_text(ui, text);
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).weak().size(12.0));
}

/// A line with a tick or a cross in front.
fn check_line(ui: &mut egui::Ui, ok: bool, text: &str) {
    ui.horizontal_wrapped(|ui| {
        let (mark, color) = if ok {
            ("OK", good_color(ui))
        } else {
            ("!!", ui.visuals().warn_fg_color)
        };
        ui.label(RichText::new(mark).strong().monospace().color(color));
        ui.label(text);
    });
}

/// A folder: a text box, Browse, and (if `default` is given) Default.
fn folder_row(ui: &mut egui::Ui, value: &mut String, title: &str, default: Option<&Path>) {
    ui.horizontal(|ui| {
        let buttons = if default.is_some() { 190.0 } else { 100.0 };
        let mut edit = egui::TextEdit::singleline(value)
            .desired_width((ui.available_width() - buttons).max(120.0));
        if let Some(default) = default {
            edit = edit.hint_text(default.display().to_string());
        }
        ui.add(edit);
        if ui.button("Browse...").clicked() {
            let start = if value.trim().is_empty() {
                default.map(|d| d.display().to_string()).unwrap_or_default()
            } else {
                value.clone()
            };
            if let Some(dir) = win::pick_folder(title, &start) {
                *value = dir.display().to_string();
            }
        }
        if default.is_some()
            && ui
                .add_enabled(!value.trim().is_empty(), egui::Button::new("Use default"))
                .clicked()
        {
            value.clear();
        }
    });
}

fn choice<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    all: &[T],
    label: impl Fn(T) -> &'static str,
) {
    egui::ComboBox::from_id_salt(id)
        .width(COMBO_WIDTH)
        .selected_text(label(*value))
        .show_ui(ui, |ui| {
            for &option in all {
                ui.selectable_value(value, option, label(option));
            }
        });
}

/// Release notes (Markdown) as lines to read: headings marked, the
/// emphasis, code marks and link targets taken out.
fn readable_notes(notes: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for line in notes.lines() {
        let line = line.trim_end();
        let heading = line.starts_with('#');
        let mut text = line
            .trim_start_matches('#')
            .trim()
            .replace("**", "")
            .replace('`', "");
        // `[words](https://...)` -> `words`
        while let (Some(open), Some(mid)) = (text.find('['), text.find("](")) {
            let Some(close) = text[mid..].find(')').map(|c| mid + c) else {
                break;
            };
            if open > mid {
                break;
            }
            text = format!(
                "{}{}{}",
                &text[..open],
                &text[open + 1..mid],
                &text[close + 1..]
            );
        }
        if let Some(rest) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
            text = format!("\u{2022} {rest}");
        }
        if text.is_empty() && out.last().is_none_or(|(_, l)| l.is_empty()) {
            continue;
        }
        out.push((heading, text));
    }
    while out.last().is_some_and(|(_, l)| l.is_empty()) {
        out.pop();
    }
    out
}

/// The width of the drop-down lists.
const COMBO_WIDTH: f32 = 430.0;

/// `Half the picture: 540p (recommended)`, `720p`.
fn resolution_label(r: Resolution, console: Console) -> String {
    match r {
        Resolution::Half => format!(
            "Half the picture: {}p (recommended)",
            console.output_height() / 2
        ),
        other => other.label().to_owned(),
    }
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong());
        ui.add_space(2.0);
        body(ui);
    });
}

fn tail_view(ui: &mut egui::Ui, id: &str, lines: &VecDeque<String>, height: f32) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(6.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in lines {
                        ui.label(RichText::new(line).monospace().small());
                    }
                });
        });
}

impl App {
    fn logo(&mut self, ctx: &egui::Context) -> (egui::TextureHandle, bool) {
        if let Some(logo) = &self.logo {
            return logo.clone();
        }
        let (picture, own) = logo_picture();
        let scale = 256.0 / picture.width().max(picture.height()).max(1) as f32;
        let (w, h) = (
            (picture.width() as f32 * scale).round().max(1.0) as u32,
            (picture.height() as f32 * scale).round().max(1.0) as u32,
        );
        let rgba = image::imageops::resize(&picture, w, h, image::imageops::FilterType::Lanczos3);
        let image =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
        let logo = (
            ctx.load_texture("logo", image, egui::TextureOptions::LINEAR),
            own,
        );
        self.logo = Some(logo.clone());
        logo
    }

    /// The logo: the built-in as it is (it is a tile itself), somebody's
    /// own on a white tile.
    fn logo_tile(&mut self, ui: &mut egui::Ui, side: f32) {
        let (logo, own) = self.logo(ui.ctx());
        let picture = egui::Image::new(&logo).fit_to_exact_size(egui::vec2(side, side));
        if own {
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(side * 0.16)
                .inner_margin(side * 0.06)
                .show(ui, |ui| ui.add(picture));
        } else {
            ui.add(picture);
        }
    }

    /// Takes a picture the user picked as their logo: copied to the
    /// settings folder, shown at once, and made into the shortcuts' icon.
    fn choose_logo(&mut self, from: &Path) {
        let to = paths::home().join(paths::USER_LOGO);
        let taken = std::fs::read(from)
            .map_err(|e| format!("Could not read {}: {e}", from.display()))
            .and_then(|png| {
                install::ico_from_png(&png)
                    .map(|_| png)
                    .map_err(|e| format!("{} is {e}.", from.display()))
            })
            .and_then(|png| {
                let _ = std::fs::create_dir_all(paths::home());
                std::fs::write(&to, png)
                    .map_err(|e| format!("Could not write {}: {e}", to.display()))
            });
        self.logo_note = Some(match taken {
            Ok(()) => (
                true,
                format!("Your logo is in {}. It stays on this PC.", to.display()),
            ),
            Err(e) => (false, e),
        });
        self.refresh_logo();
    }

    /// After the user's logo came or went: the window's picture and, if
    /// the program is installed, the icon its shortcuts wear.
    fn refresh_logo(&mut self) {
        self.logo = None;
        if self.installed.launcher
            && let Err(e) =
                install::write_icon(&self.targets.program_dir, paths::user_logo().as_deref())
        {
            self.logo_note = Some((false, e));
        }
    }

    /// The running step: its bar, its numbers and its buttons; with
    /// `output`, the lines it printed below. Returns (Cancel, Skip and play
    /// now) as pressed.
    fn step_view(&self, ui: &mut egui::Ui, output: bool) -> (bool, bool) {
        let Some(active) = &self.active else {
            return (false, false);
        };
        let (mut cancel, mut skip) = (false, false);
        let p = &active.progress;
        let elapsed = active.step.started.elapsed();
        let after = match self.queue.len() {
            0 => String::new(),
            1 => " (then one more step)".to_owned(),
            n => format!(" (then {n} more steps)"),
        };
        let why = match active.purpose {
            Purpose::Prepare => "",
            Purpose::BeforePlay => " before the game starts",
            Purpose::AfterPlay => " for next time",
        };
        ui.label(RichText::new(format!("{}{why}{after}", active.kind.title())).strong());
        match p.fraction() {
            Some(fraction) => {
                let total = p.total.unwrap_or(0);
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!(
                            "{} of {} {}  ({:.0}%)",
                            progress::grouped(p.done),
                            progress::grouped(total),
                            active.kind.unit(),
                            fraction * 100.0
                        ))
                        .desired_height(22.0),
                );
            }
            None => {
                // No total is known: a moving bar and the count.
                let sweep = (elapsed.as_secs_f32() * 0.35).fract();
                let text = if p.done > 0 {
                    format!(
                        "{} {} so far",
                        progress::grouped(p.done),
                        active.kind.unit()
                    )
                } else {
                    "Starting...".to_owned()
                };
                ui.add(
                    egui::ProgressBar::new(sweep)
                        .text(text)
                        .animate(true)
                        .desired_height(22.0),
                );
            }
        }
        ui.horizontal_wrapped(|ui| {
            let mut parts = vec![format!("Elapsed {}", progress::clock(elapsed))];
            if let Some(rate) = p.rate() {
                parts.push(format!(
                    "{} {} a second",
                    progress::grouped(rate.round() as u64),
                    active.kind.unit()
                ));
            }
            match p.eta() {
                Some(eta) if !p.finished => {
                    parts.push(format!("about {} left", progress::clock(eta)))
                }
                None if p.total.is_some_and(|t| t > p.done) => {
                    parts.push("estimating the time left...".to_owned())
                }
                _ => {}
            }
            ui.label(parts.join("   |   "));
        });
        ui.horizontal(|ui| {
            let stop = match active.purpose {
                Purpose::AfterPlay => "Stop (it goes on next time)",
                _ => "Cancel",
            };
            cancel = ui
                .button(stop)
                .on_hover_text("What is done so far is kept; it goes on from there next time.")
                .clicked();
            if active.purpose == Purpose::BeforePlay && self.play_after_steps {
                skip = ui
                    .button("Skip and play now")
                    .on_hover_text("The game starts at once; what is not compiled yet is compiled while it runs (it may stutter).")
                    .clicked();
            }
            ui.label(
                RichText::new("Runs at low priority, so the PC stays usable.")
                    .weak()
                    .small(),
            );
        });
        if output {
            if p.total.is_none() && !active.last_line.is_empty() {
                ui.label(RichText::new(&active.last_line).monospace().small());
            }
            let height = (ui.available_height() - 16.0).clamp(80.0, 260.0);
            tail_view(ui, "tail", &active.tail, height);
        }
        (cancel, skip)
    }

    /// Acts on what [`Self::step_view`] returned.
    fn step_buttons(&mut self, (cancel, skip): (bool, bool)) {
        let Some(active) = &self.active else {
            return;
        };
        if skip {
            self.skip_to_play = true;
            active.step.cancel();
        } else if cancel {
            self.play_after_steps = false;
            active.step.cancel();
        }
    }

    /// Opens a file or folder in Explorer; a failure is said on the Play page.
    fn open(&mut self, target: &Path) {
        if let Some(dir) = target.parent().filter(|_| !target.exists()) {
            let _ = std::fs::create_dir_all(dir);
        }
        if target == paths::logs_dir() {
            let _ = std::fs::create_dir_all(target);
        }
        if let Err(e) = win::open_in_shell(target) {
            self.game_note = Some((false, e));
        }
    }

    fn page_play(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let game_problem = settings::game_folder_problem(&self.settings.game_dir);
        let fresh = self.shader_freshness();
        let others = self.others_running();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            self.logo_tile(ui, 120.0);
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                let running = self.game.is_some();
                let starting = self.busy() && !self.only_after_play();
                let can_play = game_problem.is_none()
                    && self.victory_info().is_some()
                    && others.is_none()
                    && !running
                    && !starting
                    && self.pending_play.is_none();
                let label = if running {
                    "Running"
                } else if starting && self.play_after_steps {
                    "Starting..."
                } else {
                    "Play"
                };
                let mut button = egui::Button::new(
                    RichText::new(label)
                        .size(34.0)
                        .strong()
                        .color(Color32::WHITE),
                );
                if can_play {
                    button = button.fill(Color32::from_rgb(28, 132, 72));
                }
                let response = ui
                    .add_enabled_ui(can_play, |ui| ui.add_sized([260.0, 76.0], button))
                    .inner;
                if can_play && !self.focused {
                    // (Enter or Space plays: the button has the keyboard focus.)
                    self.focused = true;
                    response.request_focus();
                }
                if response.clicked() {
                    if matches!(fresh, Freshness::Ready | Freshness::Unknown) {
                        self.play(&ctx, false);
                    } else {
                        self.confirm = Some(Confirm::PrepareBeforePlay);
                    }
                }
                ui.add_space(4.0);
                if let Some(game) = &self.game {
                    ui.label(format!(
                        "The game is running (started {} ago). Close it from its own window.",
                        progress::clock(game.started.elapsed())
                    ));
                } else if starting {
                    ui.label("Getting ready; the game starts by itself when this is done.");
                } else {
                    ui.label(settings::describe(&self.settings));
                    hint(
                        ui,
                        &format!(
                            "{} settings. Change them on the Settings tab.",
                            self.settings.preset.label()
                        ),
                    );
                }
            });
        });
        ui.add_space(10.0);
        if self.active.is_some()
            && self
                .active
                .as_ref()
                .is_some_and(|a| a.purpose != Purpose::Prepare)
        {
            let pressed = egui::Frame::group(ui.style())
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.step_view(ui, false)
                })
                .inner;
            self.step_buttons(pressed);
            ui.add_space(6.0);
        } else if self.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Shaders are being prepared (Shaders tab); Play is available when that is done.");
            });
        }
        if let Some(n) = &self.game_note {
            note(ui, n);
        }
        if self.game.is_none()
            && let Some((ok, text)) = &self.step_note
            && !ok
            && self.active.is_none()
        {
            warn_text(ui, text);
        }
        let mut open = None;
        if let Some((log, tail)) = &self.crash {
            if !tail.is_empty() {
                let lines: VecDeque<String> = tail.iter().cloned().collect();
                tail_view(ui, "crash-tail", &lines, 150.0);
            }
            ui.horizontal(|ui| {
                if ui.button("Open this log").clicked() {
                    open = Some(log.clone());
                }
                if ui.button("Open the logs folder").clicked() {
                    open = Some(paths::logs_dir());
                }
                hint(
                    ui,
                    "Try the Safe preset (Settings) if it happens again, and keep the log for a report.",
                );
            });
        }
        if let Some(target) = open {
            self.open(&target);
        }
        if let Some(why) = &others {
            let said = |n: &Option<(bool, String)>| n.as_ref().is_some_and(|(_, t)| t == why);
            if !said(&self.game_note) && !said(&self.step_note) {
                warn_text(ui, why);
            }
        }
        ui.add_space(6.0);
        section(ui, "Ready to play?", |ui| {
            match &game_problem {
                None => check_line(
                    ui,
                    true,
                    &format!("Game folder: {}", settings::folder(&self.settings.game_dir)),
                ),
                Some(problem) => {
                    check_line(ui, false, problem);
                    if ui.button("Choose the game folder").clicked() {
                        self.tab = Tab::Setup;
                    }
                }
            }
            match &self.victory {
                None => check_line(ui, false, "Looking at victory.exe..."),
                Some(Ok(v)) => check_line(ui, true, &format!("Program: {}", v.version)),
                Some(Err(e)) => check_line(ui, false, e),
            }
            let (ok, text) = match &fresh {
                Freshness::Unknown => (false, "Shaders: not known yet.".to_owned()),
                Freshness::Never => (false, "Shaders have not been prepared yet. The game runs without, but stutters badly while it translates them as it goes.".to_owned()),
                Freshness::OtherBuild(old) => (false, format!("Shaders were prepared with another version ({old}) and should be prepared again.")),
                Freshness::AfterUpdate => (false, "The program was updated since the shaders were prepared; they should be prepared again.".to_owned()),
                Freshness::Ready => (true, format!("Shaders prepared for this version ({}).", ago(self.settings.prepared.shaders_when))),
            };
            check_line(ui, ok, &text);
            if !ok
                && fresh != Freshness::Unknown
                && game_problem.is_none()
                && !self.busy()
                && self.game.is_none()
                && ui.button("Prepare shaders now").clicked()
            {
                self.tab = Tab::Shaders;
                self.queue_steps(
                    &ctx,
                    &[
                        (StepKind::Shaders, Purpose::Prepare),
                        (StepKind::Warm(WarmWhat::Used), Purpose::Prepare),
                    ],
                );
            }
            let warm = match self.settings.warm {
                WarmMode::Used => {
                    "Pipelines: those earlier sessions used are compiled before each start, the rest after the game closes."
                }
                WarmMode::All => {
                    "Pipelines: every one the game has created is compiled before each start."
                }
                WarmMode::Off => "Pipelines: not compiled ahead (Settings); the game may stutter.",
            };
            check_line(ui, self.settings.warm != WarmMode::Off, warm);
            if let Some(sys) = &self.sys
                && sys.commit > 0
                && sys.commit < WANTED_COMMIT
            {
                check_line(
                    ui,
                    false,
                    &format!(
                        "Memory: {} of RAM plus page file. The game needs about 64 GB; it may close by itself (see Setup).",
                        progress::bytes(sys.commit)
                    ),
                );
            }
        });
        ui.add_space(8.0);
        let mut open = None;
        ui.horizontal(|ui| {
            if self.crash.is_none() && ui.button("Open the logs folder").clicked() {
                open = Some(paths::logs_dir());
            }
            if let Some(game) = &self.game {
                ui.label(
                    RichText::new(format!("This session's log: {}", game.log.display()))
                        .weak()
                        .small(),
                );
            }
        });
        if let Some(target) = open {
            self.open(&target);
        }
    }

    fn page_setup(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Setup");
        if !self.settings.setup_done {
            ui.label(format!(
                "Welcome to {DISPLAY_NAME}. Choose the folder of your own copy of the game below and press Done; everything else can stay as it is."
            ));
        }
        let mut done = false;
        section(ui, "1. Your copy of the game", |ui| {
            hint(ui, help::GAME_DIR);
            folder_row(
                ui,
                &mut self.settings.game_dir,
                "Choose the folder of your copy of the game",
                None,
            );
            match settings::game_folder_problem(&self.settings.game_dir) {
                None => check_line(
                    ui,
                    true,
                    "This folder has the game's program and its shader archive.",
                ),
                Some(problem) => {
                    check_line(ui, false, &problem);
                    let typed = settings::folder(&self.settings.game_dir);
                    if problem.contains("It seems to be in")
                        && let Some(found) = settings::game_folder_below(Path::new(typed))
                        && ui.button(format!("Use {}", found.display())).clicked()
                    {
                        self.settings.game_dir = found.display().to_string();
                    }
                }
            }
        });
        if !self.settings.setup_done
            && settings::game_folder_problem(&self.settings.game_dir).is_none()
        {
            ui.add_space(4.0);
            done |= ui
                .add_sized(
                    [200.0, 34.0],
                    egui::Button::new(RichText::new("Done, on to Play").size(17.0).strong()),
                )
                .clicked();
        }
        section(ui, "2. This PC", |ui| {
            match &self.sys {
                None => {
                    ui.label("Looking...");
                }
                Some(sys) => {
                    egui::Grid::new("sys")
                        .num_columns(2)
                        .spacing([24.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Graphics card");
                            match &sys.adapter {
                                Some(a) => ui.label(format!(
                                    "{} ({}), {} of video memory",
                                    a.name,
                                    a.vendor(),
                                    progress::bytes(a.vram_bytes)
                                )),
                                None => ui.label("none found that can run Direct3D 12"),
                            };
                            ui.end_row();
                            ui.label("Memory (RAM)");
                            ui.label(progress::bytes(sys.ram));
                            ui.end_row();
                            ui.label("Page file");
                            ui.label(format!(
                                "{} now, so {} can be used in all",
                                progress::bytes(sys.commit.saturating_sub(sys.ram)),
                                progress::bytes(sys.commit)
                            ));
                            ui.end_row();
                            ui.label("Free on the cache drive");
                            ui.label(sys.free_disk.map_or("unknown".to_owned(), progress::bytes));
                            ui.end_row();
                        });
                    if sys.commit > 0 && sys.commit < WANTED_COMMIT {
                        warn_text(
                            ui,
                            &format!(
                                "RAM and page file together are {}. The game needs about 64 GB, or Windows closes it when memory runs out. \
                                 Make the page file larger: Windows Settings > System > About > Advanced system settings > Performance > \
                                 Advanced > Virtual memory, and set it so that RAM plus page file reach 64 GB or more.",
                                progress::bytes(sys.commit)
                            ),
                        );
                    }
                    if sys.free_disk.is_some_and(|free| free < 30 << 30) {
                        warn_text(
                            ui,
                            "Less than 30 GB is free on the cache drive. Shaders and pipelines need room; choose another cache folder below or free some space.",
                        );
                    }
                }
            }
            if ui.small_button("Look again").clicked() {
                self.inspect_system(&ctx);
            }
        });
        section(ui, "3. Where VI keeps its own files", |ui| {
            let home = paths::home();
            ui.label("Cache");
            hint(ui, help::CACHE_DIR);
            folder_row(
                ui,
                &mut self.settings.cache_dir,
                "Choose a folder for the cache",
                Some(&home.join("cache")),
            );
            ui.add_space(4.0);
            ui.label("Saves");
            hint(ui, help::DATA_DIR);
            folder_row(
                ui,
                &mut self.settings.data_dir,
                "Choose a folder for your saves",
                Some(&home.join("data")),
            );
            let game = self.settings.game_dir.trim().to_owned();
            for (what, dir) in [
                ("cache", self.settings.cache_path()),
                ("saves", self.settings.data_path()),
            ] {
                if !game.is_empty() && paths::is_within(&dir, Path::new(&game)) {
                    warn_text(
                        ui,
                        &format!(
                            "The {what} folder is inside the game folder. Choose a folder elsewhere: VI never writes into your copy of the game."
                        ),
                    );
                }
            }
        });
        ui.add_space(8.0);
        section(ui, "4. Your own logo (optional)", |ui| {
            hint(
                ui,
                "A PNG picture of yours, shown in this window and on the Desktop and Start-menu shortcuts. It is kept on this PC only.",
            );
            let own = paths::home().join(paths::USER_LOGO);
            ui.horizontal(|ui| {
                if ui.button("Choose logo...").clicked()
                    && let Some(picked) = win::pick_png("Choose a PNG picture for your logo")
                {
                    self.choose_logo(&picked);
                }
                if own.is_file() && ui.button("Use the built-in logo").clicked() {
                    self.logo_note = match std::fs::remove_file(&own) {
                        Ok(()) => None,
                        Err(e) => Some((false, format!("Could not delete {}: {e}", own.display()))),
                    };
                    self.refresh_logo();
                }
            });
            if let Some(n) = &self.logo_note {
                note(ui, n);
            }
        });
        ui.add_space(8.0);
        let ready = settings::game_folder_problem(&self.settings.game_dir).is_none();
        done |= ui
            .add_enabled_ui(ready, |ui| {
                ui.add_sized(
                    [200.0, 34.0],
                    egui::Button::new(RichText::new("Done, on to Play").size(17.0)),
                )
            })
            .inner
            .clicked();
        if done {
            self.settings.setup_done = true;
            self.tab = Tab::Play;
            self.scan_cache(&ctx);
        }
    }

    fn page_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        hint(
            ui,
            &format!(
                "Saved as you change them, in {}. They apply from the next start of the game.",
                paths::settings_file().display()
            ),
        );
        let adapters = self
            .sys
            .as_ref()
            .map(|s| s.adapters.clone())
            .unwrap_or_default();
        let nvidia = self
            .sys
            .as_ref()
            .and_then(|s| s.adapter.as_ref())
            .is_some_and(|a| a.vendor_id == 0x10DE);
        let defaults = Settings::default();
        let s = &mut self.settings;
        section(ui, "Game", |ui| {
            ui.label("Game folder");
            hint(ui, help::GAME_DIR);
            folder_row(
                ui,
                &mut s.game_dir,
                "Choose the folder of your copy of the game",
                None,
            );
            if let Some(problem) = settings::game_folder_problem(&s.game_dir) {
                warn_text(ui, &problem);
            }
            ui.add_space(4.0);
            ui.label("Preset");
            hint(ui, help::PRESET);
            ui.horizontal(|ui| {
                for &preset in Preset::ALL {
                    ui.radio_value(&mut s.preset, preset, preset.label());
                }
            });
        });
        section(ui, "Picture and speed", |ui| {
            ui.label("Console");
            hint(ui, help::CONSOLE);
            choice(ui, "console", &mut s.console, Console::ALL, Console::label);
            ui.add_space(4.0);
            ui.label("World drawn at");
            hint(ui, help::RESOLUTION);
            let console = s.console;
            let fitting: Vec<Resolution> = Resolution::ALL
                .iter()
                .copied()
                .filter(|r| r.fits(console))
                .collect();
            egui::ComboBox::from_id_salt("resolution")
                .width(COMBO_WIDTH)
                .selected_text(resolution_label(s.resolution, console))
                .show_ui(ui, |ui| {
                    for option in fitting {
                        ui.selectable_value(
                            &mut s.resolution,
                            option,
                            resolution_label(option, console),
                        );
                    }
                });
            if !s.resolution.fits(console) {
                warn_text(
                    ui,
                    &format!(
                        "{} cannot be drawn at {} here, so half the picture ({}p) is used.",
                        console.short(),
                        s.resolution.label(),
                        s.resolution.drawn_height(console)
                    ),
                );
            }
            ui.add_space(4.0);
            ui.checkbox(&mut s.ray_tracing, "Ray tracing");
            hint(ui, help::RAY_TRACING);
            ui.add_space(4.0);
            ui.label("Ambient light");
            hint(ui, help::AMBIENT);
            ui.add_enabled(
                s.preset != Preset::Custom,
                egui::Slider::new(&mut s.ambient, 0.0..=1.5).step_by(0.05),
            );
            if s.preset == Preset::Custom {
                hint(
                    ui,
                    "With the Custom preset, set VICTORY_AMBIENT under Advanced instead.",
                );
            }
            if adapters.len() > 1 || !s.adapter.trim().is_empty() {
                ui.add_space(4.0);
                ui.label("Graphics card");
                hint(ui, help::ADAPTER);
                let shown = if s.adapter.trim().is_empty() {
                    "The system's first".to_owned()
                } else {
                    s.adapter.clone()
                };
                egui::ComboBox::from_id_salt("adapter")
                    .width(COMBO_WIDTH)
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.adapter, String::new(), "The system's first");
                        for a in &adapters {
                            ui.selectable_value(&mut s.adapter, a.name.clone(), &a.name);
                        }
                    });
            }
            if nvidia || s.smooth_motion {
                ui.add_space(4.0);
                ui.checkbox(&mut s.smooth_motion, "NVIDIA Smooth Motion");
                hint(ui, help::SMOOTH_MOTION);
            }
            ui.add_space(4.0);
            let changed = s.console != defaults.console
                || s.resolution != defaults.resolution
                || s.ray_tracing != defaults.ray_tracing
                || (s.ambient - defaults.ambient).abs() > 0.001
                || s.preset != defaults.preset;
            if ui
                .add_enabled(
                    changed,
                    egui::Button::new("Back to the recommended settings"),
                )
                .clicked()
            {
                s.console = defaults.console;
                s.resolution = defaults.resolution;
                s.ray_tracing = defaults.ray_tracing;
                s.ambient = defaults.ambient;
                s.preset = defaults.preset;
            }
        });
        section(ui, "Compiling pipelines ahead", |ui| {
            hint(ui, help::WARM);
            for &mode in WarmMode::ALL {
                ui.radio_value(&mut s.warm, mode, mode.label());
            }
        });
        section(ui, "Upscaling (experimental)", |ui| {
            hint(
                ui,
                "Still being built. With a vendor's upscaler the quality mode sets the size the world is drawn at, instead of the choice above.",
            );
            ui.label("Upscaler");
            hint(ui, help::UPSCALER);
            choice(
                ui,
                "upscaler",
                &mut s.upscaler,
                Upscaler::ALL,
                Upscaler::label,
            );
            if s.upscaler != Upscaler::Game {
                ui.add_space(4.0);
                ui.label("Quality");
                hint(ui, help::QUALITY);
                choice(ui, "quality", &mut s.quality, Quality::ALL, Quality::label);
            }
            ui.add_space(4.0);
            ui.label("Frame generation");
            hint(ui, help::FRAMEGEN);
            choice(
                ui,
                "framegen",
                &mut s.frame_generation,
                FrameGen::ALL,
                FrameGen::label,
            );
            let dlss = s.upscaler == Upscaler::Dlss || s.frame_generation == FrameGen::Dlss;
            let fsr = s.upscaler == Upscaler::Fsr || s.frame_generation == FrameGen::Fsr;
            if dlss {
                ui.add_space(4.0);
                ui.label("NVIDIA DLSS (Streamline) folder");
                hint(ui, help::VENDOR_DIR);
                folder_row(
                    ui,
                    &mut s.dlss_dir,
                    "Choose the folder with NVIDIA's DLSS files",
                    None,
                );
                if settings::folder(&s.dlss_dir).is_empty() {
                    warn_text(
                        ui,
                        "DLSS needs this folder; without it the game's own upscaler runs.",
                    );
                }
            }
            if fsr {
                ui.add_space(4.0);
                ui.label("AMD FidelityFX folder");
                hint(ui, help::VENDOR_DIR);
                folder_row(
                    ui,
                    &mut s.fidelityfx_dir,
                    "Choose the folder with AMD's FidelityFX files",
                    None,
                );
            }
        });
        section(ui, "Advanced", |ui| {
            hint(ui, help::EXTRA_ENV);
            ui.add(
                egui::TextEdit::multiline(&mut s.extra_env)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("VICTORY_EXAMPLE=1"),
            );
            let (_, bad) = settings::parse_extra_env(&s.extra_env);
            for line in bad {
                warn_text(ui, &format!("Not NAME=value, so it is skipped: {line}"));
            }
            ui.collapsing("What the game is started with", |ui| {
                for (name, value) in settings::build_env(s) {
                    let mut line = format!("{name}={value}");
                    if line.chars().count() > 140 {
                        line = format!(
                            "{}... ({} characters)",
                            line.chars().take(120).collect::<String>(),
                            line.len()
                        );
                    }
                    ui.label(RichText::new(line).monospace().small());
                }
            });
        });
    }

    fn page_shaders(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Shaders and pipelines");
        hint(
            ui,
            "The game's shaders are translated once for your graphics card, and the driver builds a pipeline from each. \
             Doing both ahead takes a while and makes play smooth. Redo it after an update of VI or of the graphics driver.",
        );
        let cache_dir = self.settings.cache_path();
        section(ui, "What is prepared", |ui| {
            let current = self.victory_info().map(|v| v.build.clone());
            let p = self.settings.prepared.clone();
            egui::Grid::new("caches")
                .num_columns(3)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Shader cache").strong());
                    match &self.cache {
                        Some(c) => ui.label(format!(
                            "{} shaders, {}",
                            progress::grouped(c.shader_files),
                            progress::bytes(c.shader_bytes)
                        )),
                        None => ui.label("counting..."),
                    };
                    ui.label(
                        match freshness(&p.shaders_build, p.possibly_stale, current.as_deref()) {
                            Freshness::Unknown => "".to_owned(),
                            Freshness::Never => "not prepared by this launcher yet".to_owned(),
                            Freshness::OtherBuild(old) => {
                                format!("made for another version: {old}")
                            }
                            Freshness::AfterUpdate => {
                                "made before the last update: prepare again".to_owned()
                            }
                            Freshness::Ready => {
                                format!("made for this version, {}", ago(p.shaders_when))
                            }
                        },
                    );
                    ui.end_row();
                    ui.label(RichText::new("Pipelines").strong());
                    match &self.cache {
                        Some(c) => ui.label(format!(
                            "{} on disk; {} used in play, {} compiled{}",
                            progress::bytes(c.pipeline_bytes),
                            progress::grouped(c.used),
                            progress::grouped(c.warmed),
                            if c.bad > 0 {
                                format!(", {} the driver could not build", c.bad)
                            } else {
                                String::new()
                            }
                        )),
                        None => ui.label("counting..."),
                    };
                    ui.label(
                        match freshness(&p.pipelines_build, p.possibly_stale, current.as_deref()) {
                            Freshness::Unknown => "".to_owned(),
                            Freshness::Never => "not compiled by this launcher yet".to_owned(),
                            Freshness::OtherBuild(old) => {
                                format!("compiled for another version: {old}")
                            }
                            Freshness::AfterUpdate => {
                                "compiled before the last update: they are compiled again"
                                    .to_owned()
                            }
                            Freshness::Ready => {
                                format!("compiled for this version, {}", ago(p.pipelines_when))
                            }
                        },
                    );
                    ui.end_row();
                });
            hint(ui, &format!("In {}", cache_dir.display()));
        });
        ui.add_space(8.0);
        let idle = !self.busy() && self.game.is_none();
        let others = self.others_running();
        let can_run = idle
            && others.is_none()
            && self.victory_info().is_some()
            && settings::game_folder_problem(&self.settings.game_dir).is_none();
        let mut queue: Option<Vec<(StepKind, Purpose)>> = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    can_run,
                    egui::Button::new(RichText::new("Prepare everything").strong())
                        .min_size(egui::vec2(160.0, 30.0)),
                )
                .on_hover_text("Translates the shaders, then compiles the pipelines earlier sessions used.")
                .clicked()
            {
                queue = Some(vec![
                    (StepKind::Shaders, Purpose::Prepare),
                    (StepKind::Warm(WarmWhat::Used), Purpose::Prepare),
                ]);
            }
            if ui
                .add_enabled(can_run, egui::Button::new("Prepare shaders"))
                .clicked()
            {
                queue = Some(vec![(StepKind::Shaders, Purpose::Prepare)]);
            }
            if ui
                .add_enabled(can_run, egui::Button::new("Compile used pipelines"))
                .on_hover_text("Those earlier sessions drew with: quick, and what matters most.")
                .clicked()
            {
                queue = Some(vec![(StepKind::Warm(WarmWhat::Used), Purpose::Prepare)]);
            }
            if ui
                .add_enabled(can_run, egui::Button::new("Compile all pipelines"))
                .on_hover_text("Every pipeline the game has created: long the first time (tens of thousands), then only new ones.")
                .clicked()
            {
                queue = Some(vec![(StepKind::Warm(WarmWhat::All), Purpose::Prepare)]);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(idle, egui::Button::new("Clear shader cache..."))
                .clicked()
            {
                self.confirm = Some(Confirm::ClearShaders);
            }
            if ui
                .add_enabled(idle, egui::Button::new("Clear pipeline cache..."))
                .clicked()
            {
                self.confirm = Some(Confirm::ClearPipelines { forget: false });
            }
            if ui
                .add_enabled(!self.cache_scanning, egui::Button::new("Refresh"))
                .clicked()
            {
                self.scan_cache(&ctx);
            }
        });
        if let Some(steps) = queue {
            self.queue_steps(&ctx, &steps);
        }
        if self.game.is_some() {
            hint(
                ui,
                "The game is running: preparing and clearing wait until it is closed.",
            );
        } else if let Some(why) = &others {
            warn_text(ui, why);
        } else if let Some(problem) = settings::game_folder_problem(&self.settings.game_dir) {
            warn_text(ui, &problem);
        } else if let Some(Err(e)) = &self.victory {
            error_text(ui, e);
        }
        ui.add_space(8.0);
        if self.active.is_some() {
            let pressed = self.step_view(ui, true);
            self.step_buttons(pressed);
        } else {
            if let Some(n) = &self.step_note {
                note(ui, n);
                if !n.0 && ui.button("Open the logs folder").clicked() {
                    self.open(&paths::logs_dir());
                }
            }
            if !self.last_tail.is_empty() {
                ui.add_space(4.0);
                hint(ui, "Output of the last step:");
                let height = (ui.available_height() - 16.0).clamp(80.0, 200.0);
                tail_view(ui, "last-tail", &self.last_tail, height);
            }
        }
    }

    fn page_updates(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Updates");
        ui.label(format!(
            "This is {}.",
            update::describe_build(
                update::current_version(),
                update::current_commit(),
                update::current_date()
            )
        ));
        hint(ui, NOTICE);
        let before = self.settings.updates.channel;
        let downloading = matches!(self.update, UpdateState::Downloading(..));
        let feed = self.feed().describe();
        let s = &mut self.settings.updates;
        section(ui, "Where updates come from", |ui| {
            ui.label("Update channel");
            hint(ui, help::CHANNEL);
            ui.add_enabled_ui(!downloading, |ui| {
                choice(ui, "channel", &mut s.channel, Channel::ALL, Channel::label)
            });
            ui.add_space(4.0);
            ui.checkbox(
                &mut s.check_on_start,
                "Look for a new version when the launcher starts",
            );
            hint(ui, help::CHECK_UPDATES);
            hint(
                ui,
                &format!("Last looked: {}. Asked: {feed}", ago(s.last_check)),
            );
        });
        if self.settings.updates.channel != before {
            // What was found (or staged) is of the other channel.
            let _ = std::fs::remove_file(paths::updates_dir().join("pending.txt"));
            self.check_update(&ctx);
        }
        ui.add_space(8.0);
        let checking = matches!(
            self.update,
            UpdateState::Checking | UpdateState::Downloading(..)
        );
        if ui
            .add_enabled_ui(!checking, |ui| {
                ui.add_sized([160.0, 30.0], egui::Button::new("Check for updates"))
            })
            .inner
            .clicked()
        {
            self.check_update(&ctx);
        }
        ui.add_space(8.0);
        let mut start_download = None;
        let mut go_back = None;
        let mut restart = false;
        match &self.update {
            UpdateState::Idle => {}
            UpdateState::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Asking for the newest release...");
                });
            }
            UpdateState::UpToDate(words) => {
                ui.label(RichText::new(words).color(good_color(ui)));
            }
            UpdateState::NoReleases => {
                ui.label(match self.settings.updates.channel {
                    Channel::Stable => "No release has been published yet. Nothing to update.",
                    Channel::Nightly => {
                        "No latest build has been published yet. Nothing to update."
                    }
                });
            }
            UpdateState::Failed(e) => error_text(ui, e),
            UpdateState::Available(release, back) => {
                let name = update::release_name(release);
                if *back {
                    ui.label(
                        RichText::new(format!("The stable release {name} can be installed."))
                            .strong(),
                    );
                    ui.label("You are on a latest build. Going back to the stable release may give you an older program than you have now.");
                } else {
                    ui.label(RichText::new(format!("New: {name} is available.")).strong());
                }
                match &release.asset {
                    Some(asset) => hint(
                        ui,
                        &format!("{} ({})", asset.name, progress::bytes(asset.size)),
                    ),
                    None => warn_text(
                        ui,
                        "This release has no Windows download attached, so it cannot be installed from here.",
                    ),
                }
                if !self.running_installed {
                    warn_text(
                        ui,
                        "Updates are put into the installed copy. Install VI first (Install tab), then update from there.",
                    );
                } else if release.asset.is_some()
                    && ui
                        .add_sized(
                            [220.0, 32.0],
                            egui::Button::new(
                                RichText::new(if *back {
                                    "Go back to the stable release"
                                } else {
                                    "Update"
                                })
                                .strong(),
                            ),
                        )
                        .clicked()
                {
                    if *back {
                        go_back = Some(release.clone());
                    } else {
                        start_download = Some(release.clone());
                    }
                }
                ui.add_space(4.0);
                ui.label("What is new:");
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("notes")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            if release.notes.trim().is_empty() {
                                ui.label("(The release has no notes.)");
                            }
                            for (heading, line) in readable_notes(&release.notes) {
                                if heading {
                                    ui.label(RichText::new(line).strong());
                                } else {
                                    ui.label(line);
                                }
                            }
                        });
                });
            }
            UpdateState::Downloading(release, transfer) => {
                let (got, total) = (
                    transfer.received.load(Ordering::Relaxed),
                    transfer.total.load(Ordering::Relaxed),
                );
                ui.label(format!("Downloading {}...", update::release_name(release)));
                let bar = if total > 0 {
                    egui::ProgressBar::new(got as f32 / total as f32).text(format!(
                        "{} / {}",
                        progress::bytes(got),
                        progress::bytes(total)
                    ))
                } else {
                    egui::ProgressBar::new(0.0)
                        .text(progress::bytes(got))
                        .animate(true)
                };
                ui.add(bar.desired_height(22.0));
                if ui.button("Cancel").clicked() {
                    transfer.cancel.store(true, Ordering::Relaxed);
                }
            }
            UpdateState::Ready(tag) => {
                ui.label(
                    RichText::new(format!("{tag} is downloaded and checked."))
                        .color(good_color(ui)),
                );
                if self.running_installed {
                    ui.label("It is put in place when the launcher starts next. Your settings, saves and caches stay.");
                    let blocked = self.game.is_some() || self.busy();
                    if ui
                        .add_enabled_ui(!blocked, |ui| {
                            ui.add_sized(
                                [200.0, 32.0],
                                egui::Button::new(RichText::new("Restart now to finish").strong()),
                            )
                        })
                        .inner
                        .clicked()
                    {
                        restart = true;
                    }
                    if blocked {
                        hint(ui, "Close the game (or let the shader step finish) first.");
                    }
                } else {
                    warn_text(
                        ui,
                        "It will be put in place the next time the installed VI is started.",
                    );
                }
            }
        }
        if let Some(release) = start_download {
            self.download_update(&ctx, release);
        }
        if let Some(release) = go_back {
            self.confirm = Some(Confirm::BackToStable(Box::new(release)));
        }
        if restart {
            match std::env::current_exe().and_then(|me| std::process::Command::new(me).spawn()) {
                Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => {
                    self.update = UpdateState::Failed(format!(
                        "Could not restart the launcher: {e}. Close it and start it again by hand."
                    ))
                }
            }
        }
    }

    fn page_install(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Install");
        let t = self.targets.clone();
        if self.running_installed {
            ui.label(
                RichText::new(format!(
                    "{DISPLAY_NAME} is installed and this is the installed copy."
                ))
                .color(good_color(ui)),
            );
        } else if self.installed.complete() {
            ui.label(format!(
                "{DISPLAY_NAME} {} is installed. This window is another copy, started from {}.",
                self.installed.version.clone().unwrap_or_default(),
                paths::exe_dir().display()
            ));
        } else {
            ui.label(format!(
                "Install {DISPLAY_NAME} for this Windows user, like any other game. No administrator rights are asked for. \
                 Your copy of the game is not copied, moved or changed."
            ));
        }
        section(ui, "What an install puts where", |ui| {
            let state = |present: bool| if present { "there" } else { "not there yet" };
            let rows = [
                (
                    "Program files",
                    t.program_dir.display().to_string(),
                    state(self.installed.launcher && self.installed.victory).to_owned(),
                ),
                (
                    "Desktop shortcut",
                    install::desktop_shortcut(&t).display().to_string(),
                    state(self.installed.desktop_shortcut).to_owned(),
                ),
                (
                    "Start menu",
                    install::start_menu_shortcut(&t).display().to_string(),
                    state(self.installed.start_menu_shortcut).to_owned(),
                ),
                (
                    "Installed apps entry",
                    format!("{DISPLAY_NAME} (for this Windows user only)"),
                    self.installed
                        .version
                        .as_ref()
                        .map_or("not there yet".to_owned(), |v| {
                            format!("there, version {v}")
                        }),
                ),
                (
                    "Settings, saves, cache",
                    paths::home().display().to_string(),
                    "kept through updates and reinstalls".to_owned(),
                ),
            ];
            for (what, place, how) in rows {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{what}:")).strong());
                    ui.label(place);
                    ui.label(RichText::new(format!("({how})")).weak());
                });
            }
            if t.trial {
                hint(ui, "A trial install: VICTORY_INSTALL_DIR_OVERRIDE is set.");
            }
        });
        ui.add_space(8.0);
        let victory = self.victory_path();
        ui.horizontal(|ui| {
            if !self.running_installed {
                let label = if self.installed.any() {
                    "Install again (repair)"
                } else {
                    "Install"
                };
                let button = egui::Button::new(RichText::new(label).size(18.0).strong())
                    .min_size(egui::vec2(180.0, 40.0));
                if ui
                    .add_enabled(
                        !self.install_busy && (victory.is_some() || self.carries_victory),
                        button,
                    )
                    .clicked()
                    && let Ok(me) = std::env::current_exe()
                {
                    let victory = victory.clone();
                    self.install_busy = true;
                    self.install_note = None;
                    let t = t.clone();
                    self.spawn(&ctx, move || {
                        win::com_init();
                        Msg::Installed(install::install_with_logo(
                            &t,
                            &me,
                            victory.as_deref(),
                            paths::user_logo().as_deref(),
                        ))
                    });
                }
            }
            if self.installed.any()
                && ui
                    .add_enabled(
                        !self.install_busy && self.game.is_none() && !self.busy(),
                        egui::Button::new("Uninstall..."),
                    )
                    .clicked()
            {
                match crate::relay_uninstall(&Args::default()) {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => self.install_note = Some(Err(e)),
                }
            }
            if self.install_busy {
                ui.spinner();
                ui.label("Copying...");
            }
        });
        if victory.is_none() && !self.carries_victory && !self.running_installed {
            error_text(
                ui,
                &format!(
                    "victory.exe was not found beside the launcher ({}). Unpack the whole download into one folder and start the launcher from there.",
                    paths::exe_dir().join(paths::VICTORY_EXE).display()
                ),
            );
        }
        let mut open_installed = false;
        match &self.install_note {
            Some(Ok(done)) => {
                ui.add_space(6.0);
                for line in done {
                    check_line(ui, true, line);
                }
                ui.label(RichText::new(format!("Installed. From now on, start {DISPLAY_NAME} from the Desktop or the Start menu.")).color(good_color(ui)));
                open_installed = ui
                    .button(format!("Open the installed {DISPLAY_NAME} now"))
                    .clicked();
            }
            Some(Err(e)) => error_text(ui, e),
            None => {}
        }
        if open_installed {
            let exe = t.program_dir.join(paths::LAUNCHER_EXE);
            match std::process::Command::new(&exe)
                .current_dir(&t.program_dir)
                .spawn()
            {
                Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => {
                    self.install_note = Some(Err(format!("Could not start {}: {e}", exe.display())))
                }
            }
        }
    }

    fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &mut self.confirm else {
            return;
        };
        let cache = self.settings.cache_path();
        // (What was chosen: 0 nothing yet, 1 the first button, 2 the second, 3 cancel.)
        let mut chosen = 0;
        let modal = egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            ui.set_width(460.0);
            match confirm {
                Confirm::ClearShaders => {
                    ui.heading("Clear the shader cache?");
                    ui.label(format!("This deletes {} and nothing else.", cache.join(caches::SHADERS_DIR).display()));
                    ui.label("The shaders have to be prepared again afterwards (several minutes).");
                }
                Confirm::ClearPipelines { forget } => {
                    ui.heading("Clear the pipeline cache?");
                    ui.label(format!("This deletes the built pipelines in {} and nothing else.", cache.join(caches::PIPELINES_DIR).display()));
                    ui.label("Compiling ahead builds them again.");
                    ui.checkbox(forget, "Also forget which pipelines the game uses");
                    hint(ui, "Then there is nothing to compile ahead until you have played again. Leave this off unless told otherwise.");
                }
                Confirm::BackToStable(release) => {
                    ui.heading("Go back to the stable release?");
                    ui.label(format!(
                        "This replaces the latest build you have with the stable release {}. It may be older: fixes made since are not in it.",
                        release.tag
                    ));
                    ui.label("Your settings, saves and caches stay; shaders may need preparing again.");
                }
                Confirm::PrepareBeforePlay => {
                    ui.heading("Prepare shaders first?");
                    ui.label("The shaders have not been prepared for this version. Preparing takes several minutes, once; without it the game stutters badly while it translates them during play.");
                    hint(ui, "Progress shows on the Play page; it can be stopped there and goes on next time.");
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| match confirm {
                Confirm::PrepareBeforePlay => {
                    if ui.button(RichText::new("Prepare, then play").strong()).clicked() {
                        chosen = 1;
                    }
                    if ui.button("Play without preparing").clicked() {
                        chosen = 2;
                    }
                    if ui.button("Cancel").clicked() {
                        chosen = 3;
                    }
                }
                Confirm::BackToStable(_) => {
                    if ui.button("Go back to stable").clicked() {
                        chosen = 1;
                    }
                    if ui.button(RichText::new("Stay on the latest build").strong()).clicked() {
                        chosen = 3;
                    }
                }
                _ => {
                    if ui.button("Delete").clicked() {
                        chosen = 1;
                    }
                    if ui.button(RichText::new("Keep").strong()).clicked() {
                        chosen = 3;
                    }
                }
            });
        });
        // (Escape, or a click beside it: as its last button.)
        if chosen == 0 && modal.should_close() {
            chosen = 3;
        }
        if chosen == 0 {
            return;
        }
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        if chosen == 3 {
            return;
        }
        let game_dir = self.settings.game_dir.clone();
        let program = self.targets.program_dir.clone();
        match confirm {
            Confirm::ClearShaders => {
                self.step_note = Some(match caches::clear_shaders(&cache, &game_dir, &program) {
                    Ok(bytes) => {
                        self.settings.prepared.shaders_build.clear();
                        (
                            true,
                            format!("Shader cache cleared ({} freed).", progress::bytes(bytes)),
                        )
                    }
                    Err(e) => (false, e),
                });
                self.last_tail.clear();
                self.scan_cache(ctx);
            }
            Confirm::ClearPipelines { forget } => {
                self.step_note = Some(
                    match caches::clear_pipelines(&cache, &game_dir, &program, forget) {
                        Ok(bytes) => {
                            self.settings.prepared.pipelines_build.clear();
                            (
                                true,
                                format!(
                                    "Pipeline cache cleared ({} freed).",
                                    progress::bytes(bytes)
                                ),
                            )
                        }
                        Err(e) => (false, e),
                    },
                );
                self.last_tail.clear();
                self.scan_cache(ctx);
            }
            Confirm::PrepareBeforePlay if chosen == 1 => self.play(ctx, true),
            Confirm::PrepareBeforePlay => self.play(ctx, false),
            Confirm::BackToStable(release) => self.download_update(ctx, *release),
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(ctx, msg);
        }
        self.poll_step(ctx);
        if let Some(game) = &mut self.game
            && let Some(ended) = game.ended()
        {
            self.game_ended(ctx, ended);
        }
        if !self.busy()
            && let Some(shaders_first) = self.pending_play.take()
        {
            self.play(ctx, shaders_first);
        }
        self.look_for_others();
        if !self.others.is_empty() {
            // (Looked at again until it has gone.)
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        if self.settings != self.saved {
            match self.settings.save(&paths::settings_file()) {
                Ok(()) => self.saved = self.settings.clone(),
                Err(e) => self.notice = Some(e),
            }
        }
        self.automation(ctx);
        // Clocks, the game's end and the download's bar are looked at, not told.
        if self.game.is_some()
            || self.active.is_some()
            || matches!(self.update, UpdateState::Downloading(..))
        {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(DISPLAY_NAME).size(20.0).strong());
                ui.add_space(12.0);
                for (tab, name) in Tab::ALL {
                    let mut text = RichText::new(name).size(15.0);
                    if tab == Tab::Updates
                        && matches!(
                            self.update,
                            UpdateState::Available(..) | UpdateState::Ready(_)
                        )
                    {
                        text = text.color(ui.visuals().warn_fg_color);
                    }
                    ui.selectable_value(&mut self.tab, tab, text);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(active) = &self.active {
                        let text = match active.progress.fraction() {
                            Some(f) => format!("{} {:.0}%", active.kind.title(), f * 100.0),
                            None => format!("{}...", active.kind.title()),
                        };
                        ui.label(RichText::new(text).small());
                        ui.spinner();
                    } else if self.game.is_some() {
                        ui.label(RichText::new("The game is running").small());
                    }
                });
            });
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let build = update::describe_build(
                    update::current_version(),
                    update::current_commit(),
                    update::current_date(),
                );
                let place = if self.running_installed {
                    "installed"
                } else if self.carries_victory {
                    "setup program"
                } else {
                    "not the installed copy"
                };
                ui.label(RichText::new(format!("{build}, {place}")).small().weak())
                    .on_hover_text(format!("Commit {}", update::current_commit()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = if self.game.is_some() {
                        "The game is running".to_owned()
                    } else {
                        settings::describe(&self.settings)
                    };
                    ui.label(RichText::new(text).small().weak());
                });
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(text) = self.notice.clone() {
                ui.horizontal_wrapped(|ui| {
                    warn_text(ui, &text);
                    if ui.small_button("OK").clicked() {
                        self.notice = None;
                    }
                });
                ui.separator();
            }
            egui::ScrollArea::vertical()
                .id_salt("page")
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::Play => self.page_play(ui),
                    Tab::Setup => self.page_setup(ui),
                    Tab::Settings => self.page_settings(ui),
                    Tab::Shaders => self.page_shaders(ui),
                    Tab::Updates => self.page_updates(ui),
                    Tab::Install => self.page_install(ui),
                });
        });
        self.confirm_window(&ctx);
    }
}

/// The small window that removes the program (run from a temporary copy).
struct UninstallApp {
    targets: InstallTargets,
    settings: Settings,
    remove_data: bool,
    done: Option<Arc<Mutex<Option<Vec<String>>>>>,
    started: Instant,
    shot: Arc<AtomicBool>,
    shot_asked: bool,
}

impl eframe::App for UninstallApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // (Tests: `VICTORY_LAUNCHER_SHOT=<png>` saves a picture of this
        // window after a moment and closes it without uninstalling.)
        let Some(path) = std::env::var_os("VICTORY_LAUNCHER_SHOT").map(PathBuf::from) else {
            return;
        };
        if self.shot.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.shot_asked && self.started.elapsed().as_secs_f32() >= 1.5 {
            self.shot_asked = true;
            let (done, wake) = (self.shot.clone(), ctx.clone());
            ctx.request_screenshot(move |image| {
                let [w, h] = image.size;
                if let Some(picture) =
                    image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
                {
                    let _ = picture.save(&path);
                }
                done.store(true, Ordering::Relaxed);
                wake.request_repaint();
            });
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(format!("Uninstall {DISPLAY_NAME}"));
            ui.add_space(6.0);
            let result = self.done.as_ref().map(|d| d.lock().unwrap_or_else(|e| e.into_inner()).clone());
            match result {
                None => {
                    ui.label(format!(
                        "This removes the program from {}, its shortcuts and its entry in the list of installed apps.",
                        self.targets.program_dir.display()
                    ));
                    ui.label("Your copy of the game is not touched.");
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.remove_data, "Also delete my saved games, caches and settings");
                    hint(ui, &format!("Off: everything in {} stays, for a later reinstall.", paths::home().display()));
                    if self.remove_data {
                        warn_text(ui, "Your saved games will be deleted and cannot be brought back.");
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Uninstall").clicked() {
                            let slot = Arc::new(Mutex::new(None));
                            self.done = Some(slot.clone());
                            let (targets, settings, remove_data, wake) = (self.targets.clone(), self.settings.clone(), self.remove_data, ctx.clone());
                            std::thread::spawn(move || {
                                win::com_init();
                                let home = paths::home();
                                let notes = install::uninstall(&targets, remove_data.then_some((&settings, home.as_path())));
                                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(notes);
                                wake.request_repaint();
                            });
                        }
                        if ui.button(RichText::new("Cancel").strong()).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
                Some(None) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Removing...");
                    });
                }
                Some(Some(notes)) => {
                    if notes.is_empty() {
                        ui.label(RichText::new(format!("{DISPLAY_NAME} was removed.")).color(good_color(ui)));
                    } else {
                        ui.label(format!("{DISPLAY_NAME} was removed, with these remarks:"));
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            for line in &notes {
                                warn_text(ui, line);
                            }
                        });
                    }
                    ui.add_space(12.0);
                    if ui.button("Close").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        });
    }
}

pub fn run_uninstall() {
    let targets = paths::install_targets();
    let (settings, _) = Settings::load(&paths::settings_file());
    let viewport = egui::ViewportBuilder::default()
        .with_title(format!("Uninstall {DISPLAY_NAME}"))
        .with_inner_size([520.0, 300.0])
        .with_resizable(false);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let app = UninstallApp {
        targets,
        settings,
        remove_data: false,
        done: None,
        started: Instant::now(),
        shot: Arc::new(AtomicBool::new(false)),
        shot_asked: false,
    };
    let _ = eframe::run_native(
        &format!("Uninstall {DISPLAY_NAME}"),
        options,
        Box::new(move |_| Ok(Box::new(app))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaders_are_fresh_only_for_the_build_that_made_them() {
        assert_eq!(freshness("", false, None), Freshness::Unknown);
        assert_eq!(
            freshness("", false, Some("victory 0.1.0 (abc)")),
            Freshness::Never
        );
        assert_eq!(
            freshness("victory 0.1.0 (abc)", false, Some("victory 0.1.0 (abc)")),
            Freshness::Ready
        );
        assert_eq!(
            freshness("victory 0.1.0 (abc)", true, Some("victory 0.1.0 (abc)")),
            Freshness::AfterUpdate
        );
        assert_eq!(
            freshness("victory 0.1.0 (abc)", false, Some("victory 0.1.0 (def)")),
            Freshness::OtherBuild("victory 0.1.0 (abc)".to_owned())
        );
    }

    #[test]
    fn release_notes_are_made_readable() {
        let notes = "> VI is unofficial.\n\n## VI 0.1.0 \u{2014} early preview\n\n\n- **`VI-Setup.exe`** one file. See [the README](https://example.org/r).\n";
        assert_eq!(
            readable_notes(notes),
            [
                (false, "> VI is unofficial.".to_owned()),
                (false, String::new()),
                (true, "VI 0.1.0 \u{2014} early preview".to_owned()),
                (false, String::new()),
                (
                    false,
                    "\u{2022} VI-Setup.exe one file. See the README.".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn tabs_by_name() {
        assert_eq!(Tab::from_word("shaders"), Some(Tab::Shaders));
        assert_eq!(Tab::from_word("Play"), Some(Tab::Play));
        assert_eq!(Tab::from_word("nope"), None);
    }
}
