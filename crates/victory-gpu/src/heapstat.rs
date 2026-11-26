//! Diagnostics of the big title heaps' writes (diagnostics builds only): how
//! many of their pages the write watch finds written, where, and how alike
//! one second's written pages are to the next's. Says whether a heap's
//! written pages stay in one place (a ring, a few hot pieces) or wander.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The unit counted: 64 KiB.
const PAGE: usize = 64 << 10;
/// A piece: 2 MiB.
const PIECE: usize = 2 << 20;
/// Copies at least this big are counted.
pub(crate) const BIG: usize = 64 << 20;

struct Heap {
    start: usize,
    end: usize,
    /// 64 KiB pages written since the last line: times found.
    now: Vec<u16>,
    /// The same of the line before.
    before: Vec<u16>,
}

static HEAPS: Mutex<Vec<Heap>> = Mutex::new(Vec::new());
/// Askings into a big heap, their nanoseconds, the 4 KiB pages they found;
/// nanoseconds of putting title bytes under flights' writes there; of
/// refreshes there.
static N: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// A host copy of `[start, start + len)` in the card's memory was made.
pub(crate) fn big(start: usize, len: usize) {
    if len < BIG {
        return;
    }
    let pages = len.div_ceil(PAGE);
    let mut heaps = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
    if heaps.iter().any(|h| h.start == start) {
        return;
    }
    heaps.push(Heap { start, end: start + len, now: vec![0; pages], before: vec![0; pages] });
    drop(heaps);
    tracing::info!("heapstat: {start:#x}+{len:#x} is counted; its memory: {}", regions(start, len));
}

/// The memory's regions as Windows has them: state, type and protection,
/// with bytes, the first 12.
fn regions(start: usize, len: usize) -> String {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    let mut out = Vec::new();
    let (mut at, end) = (start, start + len);
    let mut n = 0;
    while at < end {
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        if unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) } == 0 {
            break;
        }
        let reach = (info.BaseAddress as usize + info.RegionSize).min(end);
        n += 1;
        if out.len() < 12 {
            out.push(format!(
                "{:#x}+{:#x} state {:#x} type {:#x} protect {:#x} base {:#x}",
                at,
                reach - at,
                info.State,
                info.Type,
                info.Protect,
                info.AllocationBase as usize
            ));
        }
        at = reach.max(at + 4096);
    }
    format!("{n} regions: {}", out.join("; "))
}

fn index_of(heaps: &[Heap], at: usize) -> Option<usize> {
    heaps.iter().position(|h| h.start <= at && at < h.end)
}

/// A kernel asking about `[first, last)` (bytes) found `found` (addresses
/// of 4 KiB pages) in `ns`.
pub(crate) fn asked(first: usize, last: usize, found: &[usize], ns: u64) {
    let mut heaps = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = index_of(&heaps, first).or_else(|| index_of(&heaps, last.saturating_sub(1))) else {
        return;
    };
    N[0].fetch_add(1, Relaxed);
    N[1].fetch_add(ns, Relaxed);
    let h = &mut heaps[i];
    let mut last_page = usize::MAX;
    let mut in_heap = 0;
    for &at in found {
        if at < h.start || at >= h.end {
            continue;
        }
        in_heap += 1;
        let page = (at - h.start) / PAGE;
        if page != last_page {
            h.now[page] = h.now[page].saturating_add(1);
            last_page = page;
        }
    }
    N[2].fetch_add(in_heap, Relaxed);
}

/// Title bytes were put under flights' writes in a big heap: `ns`.
pub(crate) fn merged(ns: u64) {
    N[3].fetch_add(ns, Relaxed);
}

/// A refresh of a big heap's copy: `ns`.
pub(crate) fn refreshed(ns: u64) {
    N[4].fetch_add(ns, Relaxed);
}

/// The line, per frame over `frames`.
pub(crate) fn line(frames: f64) {
    let n: Vec<u64> = N.iter().map(|a| a.swap(0, Relaxed)).collect();
    let mut heaps = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
    for h in heaps.iter_mut() {
        let pages = h.now.iter().filter(|&&c| c > 0).count();
        let again = h.now.iter().zip(&h.before).filter(|&(&a, &b)| a > 0 && b > 0).count();
        let found: u64 = h.now.iter().map(|&c| u64::from(c)).sum();
        let per = PIECE / PAGE;
        let pieces = h.now.chunks(per).filter(|c| c.iter().any(|&x| x > 0)).count();
        // Pages written in pieces where at least half the pages were.
        let dense = h.now.chunks(per).filter(|c| c.iter().filter(|&&x| x > 0).count() * 2 >= per).count();
        // The runs of written pages, and the longest.
        let (mut runs, mut longest, mut run) = (0, 0, 0);
        for &c in &h.now {
            if c > 0 {
                run += 1;
                if run == 1 {
                    runs += 1;
                }
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        tracing::info!(
            "heapstat {:#x}+{:#x} since the last line ({frames:.0} frames): {pages} pages of 64 KiB found written ({:.1} MB), \
             {again} of them written in the line before too ({:.0}%); found {found} times in all ({:.1} per frame); \
             in {pieces} pieces of 2 MiB, {dense} of them half written or more; {runs} runs, the longest {longest} pages",
            h.start,
            h.end - h.start,
            (pages * PAGE) as f64 / 1e6,
            100.0 * again as f64 / pages.max(1) as f64,
            found as f64 / frames,
        );
        std::mem::swap(&mut h.now, &mut h.before);
        h.now.iter_mut().for_each(|c| *c = 0);
    }
    drop(heaps);
    tracing::info!(
        "heapstat per frame: {:.0} kernel askings into big heaps, {:.2} ms, {:.0} pages of 4 KiB found written there; \
         {:.2} ms putting title bytes under flights' writes there; {:.2} ms refreshing their copies (all threads)",
        n[0] as f64 / frames,
        n[1] as f64 / 1e6 / frames,
        n[2] as f64 / frames,
        n[3] as f64 / 1e6 / frames,
        n[4] as f64 / 1e6 / frames,
    );
}
