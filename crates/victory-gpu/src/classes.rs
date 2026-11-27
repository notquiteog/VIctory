//! Memory classes (`VICTORY_PORT_CLASSES`, off by default; the porting
//! plan's P3): a host copy in the card's memory that only shaders write
//! becomes the only home of its bytes (class G). It is not compared, fetched
//! or landed after its flights, and not refreshed before them: the line's
//! copy-back and refresh work for it stops.
//!
//! - `=learn`: the copies that would be classed G are logged, nothing
//!   changes.
//! - `=use`: they are classed G.
//!
//! How a copy becomes G, learned while the title runs:
//!
//! - it lives in the card's memory, is a title buffer's copy (not a window),
//!   not in place, and its title memory is write-watched;
//! - shaders were declared to write it at least [`age`] frames ago
//!   (`VICTORY_PORT_CLASSES_AGE`, frames; 300 by default), and nothing of it
//!   was sent up since then: the title did not write it.
//!
//! The correctness net, as the plan's 4.1: a class is only ever widened. The
//! title memory of a G copy is made no-access, all of it, once its flights in
//! the air have landed. The first touch of it by anything (the title reading
//! or writing, or code of ours) widens the copy for good: its protection
//! comes back, and its whole span is fetched and landed behind the next
//! submission, as a range declared and not compared. A touch by the title
//! waits for that landing (at most [`WAIT_MS`]); a touch by our own code goes
//! on at once (it may be the code that lands). A copy let go while G gets its
//! protection back.
//!
//! shortcut: by copy, not by allocation key, and learned again each run (no
//! profile is kept). The no-access pages are whole pages: a neighbour's bytes
//! that share the first or last page widen the copy when touched.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::shadow::Shadow;

const PAGE: usize = 4096;

/// The longest a title touch waits for its copy to land.
const WAIT_MS: u64 = 250;

/// What the switch asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Off,
    Learn,
    Use,
}

fn parse(value: Option<&str>) -> Mode {
    match value.map(str::trim) {
        Some("learn") => Mode::Learn,
        Some("use" | "1") => Mode::Use,
        _ => Mode::Off,
    }
}

pub(crate) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let mode = parse(std::env::var("VICTORY_PORT_CLASSES").ok().as_deref());
        // The landing that widens a copy is the relay's.
        if mode == Mode::Use && !crate::host::relay() {
            tracing::warn!("VICTORY_PORT_CLASSES=use needs the relay (VICTORY_RELAY): off");
            return Mode::Off;
        }
        if mode == Mode::Use && !install() {
            tracing::warn!("VICTORY_PORT_CLASSES=use: no exception handler: off");
            return Mode::Off;
        }
        mode
    })
}

/// Frames a copy is watched before it is classed.
fn age() -> u64 {
    static AGE: OnceLock<u64> = OnceLock::new();
    *AGE.get_or_init(|| std::env::var("VICTORY_PORT_CLASSES_AGE").ok().and_then(|v| v.parse().ok()).unwrap_or(300))
}

/// Emulated, as before.
pub(crate) const X: u8 = 0;
/// Classed G; its flights in the air land, then its pages are closed.
pub(crate) const CLOSING: u8 = 1;
/// G: the card's copy is the only home of its bytes.
pub(crate) const G: u8 = 2;
/// Touched: widened, its whole span owed to title memory.
pub(crate) const OWED: u8 = 3;
/// Emulated for good.
pub(crate) const WIDE: u8 = 4;

/// A copy's class and what it is learned from. In [`Shadow`].
#[derive(Default)]
pub(crate) struct Class {
    state: AtomicU8,
    /// The frame shaders were first declared to write it, plus one (0: never).
    written: AtomicU64,
    /// The last frame something of it was sent up, plus one (0: never).
    up: AtomicU64,
    /// With [`CLOSING`]: the submission number it was classed at. With
    /// [`OWED`]: the one whose plan carries its whole span (0: none yet).
    at: AtomicU64,
    /// The whole pages of its title memory that are closed, as title
    /// addresses: from, to. Bytes of the copy in the pages at either end,
    /// which other memory may share, are followed as before.
    inner: [AtomicU64; 2],
}

impl Class {
    pub(crate) fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    /// Whether the copy's declared writes are not followed: the card's copy
    /// is the only home of what shaders write.
    pub(crate) fn skips(&self) -> bool {
        matches!(self.state(), CLOSING | G)
    }

    /// Whether title memory is not looked at for the copy: closed.
    pub(crate) fn closed(&self) -> bool {
        self.state() == G
    }

    /// The parts of `[from, to)` (title addresses) outside the closed pages:
    /// all of it unless the copy [`Class::skips`]. Empty parts are `(x, x)`.
    pub(crate) fn outside(&self, from: usize, to: usize) -> [(usize, usize); 2] {
        if !self.skips() {
            return [(from, to), (to, to)];
        }
        let (a, b) = (self.inner[0].load(Ordering::Relaxed) as usize, self.inner[1].load(Ordering::Relaxed) as usize);
        cut(from, to, a, b)
    }

    /// Shaders of a submission are declared to write the copy.
    pub(crate) fn writing(&self) {
        if self.written.load(Ordering::Relaxed) == 0 {
            self.written.store(crate::frame::current() + 1, Ordering::Relaxed);
        }
    }

    /// Something of the copy was sent up.
    pub(crate) fn sent_up(&self) {
        self.up.store(crate::frame::current() + 1, Ordering::Relaxed);
    }
}

/// `[from, to)` outside `[a, b)`: below it and above it.
fn cut(from: usize, to: usize, a: usize, b: usize) -> [(usize, usize); 2] {
    let below = (from, to.min(a).max(from));
    let above = (from.max(b).min(to), to);
    [below, above]
}

/// The whole pages of `[start, start + len)`, if there are [`LEAST_PAGES`]
/// or more: first, end.
fn interior(start: usize, len: usize) -> Option<(usize, usize)> {
    let (first, end) = (start.div_ceil(PAGE), (start + len) / PAGE);
    (end >= first + LEAST_PAGES).then_some((first, end))
}

/// The fewest whole pages a copy closes: smaller copies share their pages
/// with other memory, mostly.
const LEAST_PAGES: usize = 16;

/// Whether a copy may be classed G now, `frame` being the current frame:
/// written long enough ago, nothing sent up since.
fn due(written: u64, up: u64, frame: u64, age: u64) -> bool {
    written != 0 && written + age <= frame + 1 && (up == 0 || up < written || up + age <= frame + 1)
}

/// A copy's entry: for the handler (by address) and for the line.
struct Entry {
    /// The copy, its class; `class` lives while `copy` does, and the entry is
    /// taken out before the copy goes ([`gone`]).
    copy: Weak<Shadow>,
    class: *const Class,
    /// Its title pages: first, end.
    pages: (usize, usize),
    /// The whole span, as title addresses.
    span: (usize, usize),
}

// SAFETY: `class` is read only under the table's lock, and an entry leaves
// the table before its copy is dropped.
unsafe impl Send for Entry {}

#[derive(Default)]
struct Table {
    /// By copy id.
    copies: HashMap<u64, Entry>,
    /// Closed pages: first page, (end page, copy id).
    closed: BTreeMap<usize, (usize, u64)>,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

fn table() -> std::sync::MutexGuard<'static, Option<Table>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// For the frame-rate line.
static STATS: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];
mod stat {
    /// Copies classed G.
    pub const CLASSED: usize = 0;
    /// Copies widened: by the title, by us.
    pub const TITLE: usize = 1;
    pub const OURS: usize = 2;
    /// Title touches that waited, the ms waited, and those that timed out.
    pub const WAITS: usize = 3;
    pub const WAITED_MS: usize = 4;
    pub const TIMED_OUT: usize = 5;
    /// Copies that could not be closed (memory not plain read-write).
    pub const REFUSED: usize = 6;
    /// Owed spans landed.
    pub const LANDED: usize = 7;
    /// Closed copies found open (another protection put on them).
    pub const REOPENED: usize = 8;
    /// The longest single title wait, ms.
    pub const LONGEST: usize = 9;
}

/// The longest title wait of the run, ms, and the title waits of the run.
static LONGEST_EVER: AtomicU64 = AtomicU64::new(0);
static WAITS_EVER: AtomicU64 = AtomicU64::new(0);

/// At `Host::card_before`, for the copies of a submission (numbered
/// `sent_as`) whose shader writes are declared: those due are classed.
pub(crate) fn consider<'a>(copies: impl Iterator<Item = &'a Arc<Shadow>>, sent_as: u64) {
    let mode = mode();
    if mode == Mode::Off {
        return;
    }
    let frame = crate::frame::current();
    let age = age();
    for copy in copies {
        let Some(class) = copy.class() else { continue };
        if class.state() != X
            || !due(class.written.load(Ordering::Relaxed), class.up.load(Ordering::Relaxed), frame, age)
        {
            continue;
        }
        let (start, len) = copy.span();
        let (start, len) = (start as usize, len as usize);
        let Some(pages) = interior(start, len) else {
            class.state.store(WIDE, Ordering::Release);
            continue;
        };
        if mode == Mode::Learn {
            // Logged once.
            class.state.store(WIDE, Ordering::Release);
            STATS[stat::CLASSED].fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                "memory classes (VICTORY_PORT_CLASSES=learn): {start:#x}+{len:#x} would be G (written since frame {}, never sent up since)",
                class.written.load(Ordering::Relaxed).saturating_sub(1)
            );
            continue;
        }
        let mut table = table();
        let table = table.get_or_insert_with(Table::default);
        class.at.store(sent_as, Ordering::Relaxed);
        class.inner[0].store((pages.0 * PAGE) as u64, Ordering::Relaxed);
        class.inner[1].store((pages.1 * PAGE) as u64, Ordering::Relaxed);
        class.state.store(CLOSING, Ordering::Release);
        table.copies.insert(
            copy.id(),
            Entry { copy: Arc::downgrade(copy), class: class as *const Class, pages, span: (start, start + len) },
        );
        STATS[stat::CLASSED].fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "memory classes (VICTORY_PORT_CLASSES=use): {start:#x}+{len:#x} is G (written since frame {})",
            class.written.load(Ordering::Relaxed).saturating_sub(1)
        );
    }
}

/// At `Host::card_before`, numbered `sent_as`: the copies owed to title
/// memory, each with its whole span as a range declared, for the plan. Each
/// is given once.
pub(crate) fn owed(sent_as: u64) -> Vec<(Arc<Shadow>, Vec<(usize, usize)>)> {
    if mode() != Mode::Use {
        return Vec::new();
    }
    let table = table();
    let Some(table) = table.as_ref() else {
        return Vec::new();
    };
    let mut owed = Vec::new();
    for entry in table.copies.values() {
        // SAFETY: the entry is in the table: its copy lives.
        let class = unsafe { &*entry.class };
        if class.state() == OWED && class.at.load(Ordering::Relaxed) == 0 {
            if let Some(copy) = entry.copy.upgrade() {
                class.at.store(sent_as, Ordering::Relaxed);
                owed.push((copy, vec![entry.span]));
            }
        }
    }
    owed
}

/// On the relay's thread, after the plans of submissions up to `upto` have
/// landed: copies classed before then close their pages, and copies owed
/// whose span was in one are emulated for good.
pub(crate) fn landed(upto: u64) {
    if mode() != Mode::Use {
        return;
    }
    let mut table = table();
    let Some(table) = table.as_mut() else {
        return;
    };
    let mut done = Vec::new();
    for (&id, entry) in &table.copies {
        // SAFETY: as in `owed`.
        let class = unsafe { &*entry.class };
        let at = class.at.load(Ordering::Relaxed);
        match class.state() {
            CLOSING if at <= upto => {
                // (Pages another closed copy shares stay that copy's.)
                let (first, end) = entry.pages;
                let shared = table.closed.range(..end).next_back().is_some_and(|(_, &(e, _))| e > first);
                // (View memory the write guards protect is theirs.)
                let guarded = crate::watch::guarded(first * PAGE, end * PAGE);
                if !shared && !guarded && close(entry.pages) {
                    table.closed.insert(entry.pages.0, (entry.pages.1, id));
                    class.state.store(G, Ordering::Release);
                } else {
                    STATS[stat::REFUSED].fetch_add(1, Ordering::Relaxed);
                    class.state.store(WIDE, Ordering::Release);
                    done.push(id);
                }
            }
            // Still closed, all of it: else something opened it, and
            // the copy is widened as by a touch.
            G if !still_closed(entry.pages) => {
                table.closed.remove(&entry.pages.0);
                class.at.store(0, Ordering::Relaxed);
                class.state.store(OWED, Ordering::Release);
                STATS[stat::REOPENED].fetch_add(1, Ordering::Relaxed);
            }
            OWED if at != 0 && at <= upto => {
                class.state.store(WIDE, Ordering::Release);
                STATS[stat::LANDED].fetch_add(1, Ordering::Relaxed);
                done.push(id);
            }
            _ => {}
        }
    }
    for id in done {
        table.copies.remove(&id);
    }
}

/// A copy goes: if its pages are closed they open.
pub(crate) fn gone(copy: &Shadow) {
    if mode() != Mode::Use {
        return;
    }
    let mut table = table();
    let Some(table) = table.as_mut() else {
        return;
    };
    if let Some(entry) = table.copies.remove(&copy.id()) {
        if table.closed.get(&entry.pages.0).is_some_and(|&(_, id)| id == copy.id()) {
            table.closed.remove(&entry.pages.0);
            open(entry.pages);
        }
    }
}

/// The pages `[first, end)` no-access, if all are committed plain
/// read-write; else left as they are.
fn close((first, end): (usize, usize)) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_NOACCESS, PAGE_READWRITE, VirtualProtect, VirtualQuery,
    };
    let (from, to) = (first * PAGE, end * PAGE);
    let mut at = from;
    while at < to {
        // SAFETY: asks about an address; the answer is written to ours.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if n == 0 || info.State != MEM_COMMIT || info.Protect != PAGE_READWRITE {
            return false;
        }
        at = info.BaseAddress as usize + info.RegionSize;
    }
    let mut old = 0;
    // SAFETY: title memory found plain read-write; only its protection changes.
    unsafe { VirtualProtect(from as *const _, to - from, PAGE_NOACCESS, &mut old) != 0 }
}

/// Whether the pages `[first, end)` are all no-access still.
fn still_closed((first, end): (usize, usize)) -> bool {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, PAGE_NOACCESS, VirtualQuery};
    let (mut at, to) = (first * PAGE, end * PAGE);
    while at < to {
        // SAFETY: asks about an address; the answer is written to ours.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if n == 0 || info.Protect != PAGE_NOACCESS {
            return false;
        }
        at = info.BaseAddress as usize + info.RegionSize;
    }
    true
}

/// The pages `[first, end)` read-write again.
fn open((first, end): (usize, usize)) {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let mut old = 0;
    // SAFETY: pages we closed, back as they were.
    unsafe { VirtualProtect((first * PAGE) as *const _, (end - first) * PAGE, PAGE_READWRITE, &mut old) };
}

/// The copy whose closed pages hold `page`, if any, from `closed`.
fn closed_at(closed: &BTreeMap<usize, (usize, u64)>, page: usize) -> Option<(usize, u64)> {
    closed.range(..=page).next_back().filter(|&(_, &(end, _))| page < end).map(|(&first, &(_, id))| (first, id))
}

/// Our own code: the module this is in.
fn ours(at: usize) -> bool {
    static OURS: OnceLock<(usize, usize)> = OnceLock::new();
    let (from, to) = *OURS.get_or_init(|| {
        use windows_sys::Win32::System::LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleHandleExW,
        };
        let mut module = std::ptr::null_mut();
        // SAFETY: an address in this module; the handle is not counted.
        let found = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                ours as *const u16,
                &mut module,
            )
        } != 0;
        if !found || module.is_null() {
            return (0, 0);
        }
        let base = module as usize;
        // SAFETY: a loaded image: its DOS header names its PE header, whose
        // optional header holds the size of the image.
        unsafe {
            let pe = base + (*((base + 0x3c) as *const u32)) as usize;
            (base, base + *((pe + 24 + 56) as *const u32) as usize)
        }
    });
    from <= at && at < to
}

/// The handler: a touch of a closed page widens its copy.
fn install() -> bool {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn touched(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS || r.NumberParameters < 2 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let address = r.ExceptionInformation[1];
        let page = address / PAGE;
        let by_us = ours(r.ExceptionAddress as usize);
        let class = {
            let mut table = table();
            let Some(table) = table.as_mut() else {
                return EXCEPTION_CONTINUE_SEARCH;
            };
            let Some((first, id)) = closed_at(&table.closed, page) else {
                // Another thread widened it a moment ago: the touch goes again.
                return if was_closed(page) { EXCEPTION_CONTINUE_EXECUTION } else { EXCEPTION_CONTINUE_SEARCH };
            };
            let (end, _) = table.closed.remove(&first).unwrap_or_default();
            open((first, end));
            remember(first, end);
            let Some(entry) = table.copies.get(&id) else {
                return EXCEPTION_CONTINUE_EXECUTION;
            };
            // SAFETY: in the table: the copy lives.
            let class = unsafe { &*entry.class };
            class.at.store(0, Ordering::Relaxed);
            class.state.store(OWED, Ordering::Release);
            STATS[if by_us { stat::OURS } else { stat::TITLE }].fetch_add(1, Ordering::Relaxed);
            let (start, end) = entry.span;
            let at = r.ExceptionAddress as usize;
            let how = if r.ExceptionInformation[0] == 1 { "write" } else { "read" };
            // (Formatted later: no allocation here beyond the table's.)
            WIDENED.lock().unwrap_or_else(|e| e.into_inner()).push((start, end - start, address, at, by_us, how));
            class as *const Class as usize
        };
        if !by_us {
            // Until its span has landed, or long enough.
            let began = std::time::Instant::now();
            STATS[stat::WAITS].fetch_add(1, Ordering::Relaxed);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1));
                // Under the table's lock: a copy gone from it (landed, or
                // let go) no longer waits.
                let table = table();
                let Some(entry) = table.as_ref().and_then(|t| t.copies.values().find(|e| e.class as usize == class)) else {
                    break;
                };
                // SAFETY: in the table: the copy lives.
                if unsafe { &*entry.class }.state() != OWED {
                    break;
                }
                if began.elapsed().as_millis() as u64 >= WAIT_MS {
                    STATS[stat::TIMED_OUT].fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
            let ms = began.elapsed().as_millis() as u64;
            STATS[stat::WAITED_MS].fetch_add(ms, Ordering::Relaxed);
            STATS[stat::LONGEST].fetch_max(ms, Ordering::Relaxed);
            LONGEST_EVER.fetch_max(ms, Ordering::Relaxed);
            WAITS_EVER.fetch_add(1, Ordering::Relaxed);
        }
        EXCEPTION_CONTINUE_EXECUTION
    }
    // SAFETY: the handler touches only the table and protections.
    !unsafe { AddVectoredExceptionHandler(1, Some(touched)) }.is_null()
}

/// Pages opened by widening, recently: a second thread's touch of one, at
/// the same moment, goes on.
static OPENED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn remember(first: usize, end: usize) {
    let mut opened = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    if opened.len() >= 64 {
        opened.remove(0);
    }
    opened.push((first, end));
}

fn was_closed(page: usize) -> bool {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|&(first, end)| first <= page && page < end)
}

/// Widenings not yet logged: span start, length, address touched, code
/// address, by us, how.
static WIDENED: Mutex<Vec<(usize, usize, usize, usize, bool, &'static str)>> = Mutex::new(Vec::new());

/// For the frame-rate line.
pub(crate) fn line(frames: f64) {
    if mode() == Mode::Off {
        return;
    }
    let (copies, bytes) = {
        let table = table();
        table.as_ref().map_or((0, 0), |t| {
            (t.closed.len(), t.closed.iter().map(|(&first, &(end, _))| (end - first) * PAGE).sum::<usize>())
        })
    };
    let s: Vec<u64> = STATS.iter().map(|s| s.swap(0, Ordering::Relaxed)).collect();
    tracing::info!(
        "memory classes (VICTORY_PORT_CLASSES), over {frames:.0} frames: {} classed G, {copies} closed now ({:.1} MB); widened by the title {}, \
         by us {}; title touches waited {} ({} ms, the longest {} ms, {} timed out; in the run {}, the longest {} ms); refused {};          owed spans landed {}; found open {}",
        s[stat::CLASSED],
        bytes as f64 / 1e6,
        s[stat::TITLE],
        s[stat::OURS],
        s[stat::WAITS],
        s[stat::WAITED_MS],
        s[stat::LONGEST],
        s[stat::TIMED_OUT],
        WAITS_EVER.load(Ordering::Relaxed),
        LONGEST_EVER.load(Ordering::Relaxed),
        s[stat::REFUSED],
        s[stat::LANDED],
        s[stat::REOPENED]
    );
    let widened = std::mem::take(&mut *WIDENED.lock().unwrap_or_else(|e| e.into_inner()));
    for (start, len, address, at, by_us, how) in widened {
        let name = if by_us { name_of(at as u64) } else { String::new() };
        tracing::info!(
            "  memory classes: {start:#x}+{len:#x} widened: {how} of {address:#x} by {} at {at:#x} {name}",
            if by_us { "us" } else { "the title" }
        );
    }
}

/// The function at `at` and how far in, where the profiler's symbol library
/// is built in (diagnostics builds).
fn name_of(at: u64) -> String {
    #[cfg(feature = "diagnostics")]
    {
        crate::profile::symbol_of(at).unwrap_or_default()
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        let _ = at;
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_reads_learn_and_use() {
        assert_eq!(parse(None), Mode::Off);
        assert_eq!(parse(Some("0")), Mode::Off);
        assert_eq!(parse(Some("learn")), Mode::Learn);
        assert_eq!(parse(Some("use")), Mode::Use);
    }

    #[test]
    fn a_copy_is_due_only_long_after_its_first_write_and_its_last_send() {
        // Never written by shaders: never.
        assert!(!due(0, 0, 1000, 300));
        // Written, not long enough ago.
        assert!(!due(900, 0, 1000, 300));
        // Written long ago, never sent up.
        assert!(due(600, 0, 1000, 300));
        // Sent up since, recently: not.
        assert!(!due(600, 950, 1000, 300));
        // Sent up long ago: yes.
        assert!(due(600, 650, 1000, 300));
        // Sent up before shaders first wrote it (its first upload): yes.
        assert!(due(600, 500, 1000, 300));
    }

    #[test]
    fn only_whole_pages_are_closed_and_the_ends_are_followed() {
        // Shares its first and last pages: those stay open.
        assert_eq!(interior(0x1100, 17 * PAGE), Some((2, 18)));
        // Too small once its shared pages are left out.
        assert_eq!(interior(0x1100, 16 * PAGE), None);
        assert_eq!(interior(0x1000, 16 * PAGE), Some((1, 17)));
        // A range across the closed pages: its two ends.
        assert_eq!(cut(0x1100, 0x13000, 0x2000, 0x12000), [(0x1100, 0x2000), (0x12000, 0x13000)]);
        // Inside: nothing.
        let [below, above] = cut(0x3000, 0x4000, 0x2000, 0x12000);
        assert!(below.0 >= below.1 && above.0 >= above.1);
        // Below: all of it.
        assert_eq!(cut(0x100, 0x800, 0x2000, 0x12000)[0], (0x100, 0x800));
        assert_eq!(cut(0x100, 0x800, 0x2000, 0x12000)[1].0, 0x800);
        // Above: all of it.
        assert_eq!(cut(0x13000, 0x14000, 0x2000, 0x12000)[1], (0x13000, 0x14000));
    }

    #[test]
    fn a_touched_page_finds_the_copy_that_closed_it() {
        let mut closed = BTreeMap::new();
        closed.insert(10, (20, 1));
        closed.insert(30, (31, 2));
        assert_eq!(closed_at(&closed, 9), None);
        assert_eq!(closed_at(&closed, 10), Some((10, 1)));
        assert_eq!(closed_at(&closed, 19), Some((10, 1)));
        assert_eq!(closed_at(&closed, 20), None);
        assert_eq!(closed_at(&closed, 30), Some((30, 2)));
        assert_eq!(closed_at(&closed, 31), None);
    }
}
