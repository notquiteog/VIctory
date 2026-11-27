//! The census of what the CPU does with memory shaders write
//! (`VICTORY_READ_PROBE=1`, off by default; the porting plan's P3, step 1).
//!
//! After a landing put new bytes of a host copy into a page of title memory,
//! that page is made no-access ([`arm`]). The first touch of it faults here:
//! the page gets its protection back at once and the touch goes on as if
//! nothing happened, and it is counted for the host copy the bytes came
//! through, by who touched it (code of ours, or anything else: the title and
//! the system on its behalf) and how (a read or a write). Pages are armed
//! again at the next landing that brings them something new.
//!
//! What the counts say per host copy, over a run: memory the GPU writes that
//! nobody but us ever touches is the plan's class G (GPU-only: it need not be
//! fetched, compared or landed); memory the title reads is class C.
//!
//! shortcut: a sample, not every page: at most [`MOST`] pages are armed at a
//! time, and a page another mechanism protects again (the write guards) is
//! looked at no more until it is armed again. A touch through another view of
//! the same memory, or by the kernel on the title's behalf (a system call
//! reading it), is not seen: the counts can only be low. The kernel's touch
//! fails the call instead (no fault is raised in user mode): the probe is for
//! a learning run, not for play.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const PAGE: usize = 4096;

/// The most pages armed at once.
const MOST: usize = 2048;

/// The most pages armed by one landing.
const EACH: usize = 128;

pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_READ_PROBE") && install())
}

/// A page armed: its protection before, and the host copy (its start and
/// length) the bytes came through.
#[derive(Clone, Copy)]
struct Armed {
    old: u32,
    copy: (u64, u64),
}

/// Per host copy: pages armed; touches by the title (reads, writes); by us.
#[derive(Default, Clone, Copy)]
struct Seen {
    armed: u64,
    title_reads: u64,
    title_writes: u64,
    ours: u64,
}

struct Probe {
    armed: HashMap<usize, Armed>,
    seen: HashMap<(u64, u64), Seen>,
    /// Since the run began, for the end of it.
    ever: HashMap<(u64, u64), Seen>,
    /// Where in our code our touches were, since the run began: how many
    /// at each address.
    sites: HashMap<usize, u64>,
    /// The 64 KiB blocks a page was ever armed in ([`repaired`]).
    blocks: std::collections::HashSet<usize>,
}

/// The write guards (`crate::watch`) change a 64 KiB block's protection at
/// once and take the first page's old protection for the whole block's: an
/// armed first page makes the block no-access, and they leave it so (memory
/// not theirs to guard). A touch of a page of such a block, no-access and not
/// armed, opens the page (read-write: what the guards would have left) and
/// goes on, counted.
const BLOCK: usize = 64 << 10;

static PROBE: Mutex<Option<Probe>> = Mutex::new(None);

fn probe() -> std::sync::MutexGuard<'static, Option<Probe>> {
    PROBE.lock().unwrap_or_else(|e| e.into_inner())
}

static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Our own code: the module this is in.
fn ours() -> (usize, usize) {
    static OURS: OnceLock<(usize, usize)> = OnceLock::new();
    *OURS.get_or_init(|| {
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
        // optional header holds the size of the image (offset 56 of it).
        unsafe {
            let pe = base + (*((base + 0x3c) as *const u32)) as usize;
            let size = *((pe + 24 + 56) as *const u32) as usize;
            (base, base + size)
        }
    })
}

/// New bytes of host copy `copy` (start, length) landed in the page at
/// `page` (an address in it) of title memory: it is armed, if there is room.
pub(crate) fn arm(pages: &[usize], copy: (u64, u64)) {
    use windows_sys::Win32::System::Memory::{PAGE_NOACCESS, PAGE_READWRITE, VirtualProtect};
    // (Asked before the probe's lock: the classes' handler holds theirs
    // and asks for this one.)
    let pages: Vec<usize> = pages.iter().copied().filter(|&p| !crate::classes::closed_page(p)).collect();
    if pages.is_empty() {
        return;
    }
    let mut probe = probe();
    let probe = probe.get_or_insert_with(|| Probe { armed: HashMap::new(), seen: HashMap::new(), ever: HashMap::new(), sites: HashMap::new(), blocks: Default::default() });
    let mut n = 0;
    for &at in &pages {
        let page = at / PAGE * PAGE;
        if n == EACH || probe.armed.len() >= MOST {
            STATS[2].fetch_add(1, Ordering::Relaxed);
            break;
        }
        // (Not a block's first page: the guards take its protection for
        // the block's, [`BLOCK`].)
        if probe.armed.contains_key(&page) || page % BLOCK == 0 {
            continue;
        }
        // In the table before the protection changes, under the lock the
        // handler takes: a touch meanwhile waits and finds it.
        probe.armed.insert(page, Armed { old: PAGE_READWRITE, copy });
        let mut old = 0;
        // SAFETY: a page of title memory; only its protection changes.
        let done = unsafe { VirtualProtect(page as *const _, PAGE, PAGE_NOACCESS, &mut old) } != 0;
        if !done {
            probe.armed.remove(&page);
            continue;
        }
        // (Read-only is the write guards' own: theirs to undo.)
        if old != PAGE_READWRITE {
            // Something else of its own (a guard page, write-copy): left so.
            let mut was = 0;
            // SAFETY: as above, back as it was.
            unsafe { VirtualProtect(page as *const _, PAGE, old, &mut was) };
            probe.armed.remove(&page);
            continue;
        }
        probe.armed.insert(page, Armed { old, copy });
        probe.blocks.insert(page / BLOCK);
        probe.seen.entry(copy).or_default().armed += 1;
        probe.ever.entry(copy).or_default().armed += 1;
        n += 1;
    }
    STATS[0].fetch_add(n as u64, Ordering::Relaxed);
}

/// What the probe saw of host copy `copy` (start, length) since the run
/// began: pages armed, touches by the title. `None` before it saw any.
pub(crate) fn seen_ever(copy: (u64, u64)) -> Option<(u64, u64)> {
    let probe = probe();
    let s = probe.as_ref()?.ever.get(&copy)?;
    Some((s.armed, s.title_reads + s.title_writes))
}

/// The pages armed in `[from, to)` back as they were, for another hand to
/// protect them (the memory classes).
pub(crate) fn disarm(from: usize, to: usize) {
    use windows_sys::Win32::System::Memory::VirtualProtect;
    let mut probe = probe();
    let Some(probe) = probe.as_mut() else {
        return;
    };
    let pages: Vec<usize> = probe.armed.keys().copied().filter(|&p| from <= p && p < to).collect();
    for page in pages {
        if let Some(armed) = probe.armed.remove(&page) {
            let mut was = 0;
            // SAFETY: a page we armed, back as it was.
            unsafe { VirtualProtect(page as *const _, PAGE, armed.old, &mut was) };
        }
    }
}

/// The handler: a touch of an armed page gives it its protection back and
/// goes on, counted.
fn install() -> bool {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    unsafe extern "system" fn touched(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS || r.NumberParameters < 2 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let page = r.ExceptionInformation[1] / PAGE * PAGE;
        // (Before the probe's lock, as in `arm`.)
        let theirs = crate::classes::closed_page(page);
        let mut probe = probe();
        let Some(probe) = probe.as_mut() else {
            return EXCEPTION_CONTINUE_SEARCH;
        };
        let Some(armed) = probe.armed.remove(&page) else {
            // Touched by two threads at once: the first gave the page its
            // protection back. If the touch would now go, it goes again;
            // anything else is not ours to handle.
            if open(page, r.ExceptionInformation[0] == 1) {
                return EXCEPTION_CONTINUE_EXECUTION;
            }
            // (Not a page the memory classes closed: theirs.)
            if probe.blocks.contains(&(page / BLOCK)) && no_access(page) && !theirs {
                let mut was = 0;
                // SAFETY: title memory the guards left no-access by our arming.
                unsafe { VirtualProtect(page as *const _, PAGE, PAGE_READWRITE, &mut was) };
                STATS[3].fetch_add(1, Ordering::Relaxed);
                return EXCEPTION_CONTINUE_EXECUTION;
            }
            return EXCEPTION_CONTINUE_SEARCH;
        };
        if !no_access(page) {
            // Protected again since by another hand (the write guards, a
            // block at once): theirs now.
            return if open(page, r.ExceptionInformation[0] == 1) {
                EXCEPTION_CONTINUE_EXECUTION
            } else {
                EXCEPTION_CONTINUE_SEARCH
            };
        }
        let mut was = 0;
        // SAFETY: the page we armed, back as it was.
        unsafe { VirtualProtect(page as *const _, PAGE, armed.old, &mut was) };
        let (from, to) = ours();
        let at = r.ExceptionAddress as usize;
        if from <= at && at < to {
            *probe.sites.entry(at).or_default() += 1;
        }
        for seen in [probe.seen.entry(armed.copy).or_default(), probe.ever.entry(armed.copy).or_default()] {
            if from <= at && at < to {
                seen.ours += 1;
            } else if r.ExceptionInformation[0] == 1 {
                seen.title_writes += 1;
            } else {
                seen.title_reads += 1;
            }
        }
        STATS[1].fetch_add(1, Ordering::Relaxed);
        EXCEPTION_CONTINUE_EXECUTION
    }
    /// Whether `page` is committed and no-access.
    fn no_access(page: usize) -> bool {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_NOACCESS, VirtualQuery};
        // SAFETY: asks about an address; the answer is written to ours.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let n = unsafe { VirtualQuery(page as *const _, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_NOACCESS
    }
    /// Whether a touch of `page` (a write, or not) would go now.
    fn open(page: usize, write: bool) -> bool {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READONLY, PAGE_READWRITE, VirtualQuery,
        };
        // SAFETY: asks about an address; the answer is written to ours.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let n = unsafe { VirtualQuery(page as *const _, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        n != 0
            && info.State == MEM_COMMIT
            && (info.Protect == PAGE_READWRITE || (!write && info.Protect == PAGE_READONLY))
    }
    // SAFETY: the handler touches only the probe's table and protections.
    !unsafe { AddVectoredExceptionHandler(1, Some(touched)) }.is_null()
}

/// For the frame-rate line: the host copies with pages armed since the last
/// one, the most first, and what touched them; every 60th call, the run so
/// far.
pub(crate) fn line(frames: f64) {
    if !on() {
        return;
    }
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let calls = CALLS.fetch_add(1, Ordering::Relaxed);
    let (seen, ever, sites, armed) = {
        let mut probe = probe();
        let Some(probe) = probe.as_mut() else {
            return;
        };
        let last = calls % 60 == 59;
        let ever = last.then(|| probe.ever.clone());
        let sites = last.then(|| probe.sites.clone());
        (std::mem::take(&mut probe.seen), ever, sites, probe.armed.len())
    };
    let s: Vec<f64> = STATS.iter().map(|s| s.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    tracing::info!(
        "read probe per frame (VICTORY_READ_PROBE): {:.1} pages armed, {:.1} touched, {:.1} landings out of room, {:.1} pages          the guards left no-access opened; {armed} armed now",
        s[0],
        s[1],
        s[2],
        s[3]
    );
    let mut seen: Vec<_> = seen.into_iter().collect();
    seen.sort_unstable_by_key(|(_, s)| std::cmp::Reverse(s.armed));
    for ((start, len), s) in seen.into_iter().take(12) {
        tracing::info!(
            "  read probe {start:#x}+{len:#x}: {} armed, title reads {}, title writes {}, ours {}",
            s.armed,
            s.title_reads,
            s.title_writes,
            s.ours
        );
    }
    if let Some(sites) = sites {
        let mut sites: Vec<_> = sites.into_iter().collect();
        sites.sort_unstable_by_key(|&(_, n)| std::cmp::Reverse(n));
        let base = ours().0;
        for (at, n) in sites.into_iter().take(16) {
            let name = name_of(at as u64);
            tracing::info!("  read probe ours at +{:#x} {name}: {n}", at - base);
        }
    }
    if let Some(ever) = ever {
        let mut ever: Vec<_> = ever.into_iter().collect();
        ever.sort_unstable_by_key(|&((start, _), _)| start);
        for ((start, len), s) in ever {
            tracing::info!(
                "  read probe so far {start:#x}+{len:#x}: {} armed, title reads {}, title writes {}, ours {}",
                s.armed,
                s.title_reads,
                s.title_writes,
                s.ours
            );
        }
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
