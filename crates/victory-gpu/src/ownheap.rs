//! The process's allocator: Rust's blocks in heaps of our own, one per few
//! threads (`VICTORY_OWN_HEAP=<n>`, 1 to 16 heaps; 8 when unset;
//! `VICTORY_OWN_HEAP=0`: the process heap, as Rust's `System` allocator does).
//!
//! Why: Rust's `System` allocator on Windows takes every block from the
//! process heap, the one the graphics runtime, the driver and every other
//! thread of the process use too. In a profile of the submitting thread
//! while walking (run heapprof1, 59.7 ms a frame), 15.7 ms a frame were
//! inside `RtlAllocateHeap`, `RtlReAllocateHeap` and `RtlFreeHeap`, 4.8 ms
//! of them waiting for the heap's lock (a critical section): the
//! submission's refreshes, the title's bytes put under flights' writes
//! (`Shadow::merge_under_writes`, the runs of `word_runs`), the plans of
//! card steps, each allocating short-lived vectors while eight recording
//! threads and the driver allocate beside them.
//!
//! What it does: each thread takes one of `n` private heaps
//! (`HeapCreate`, serialized, low-fragmentation front end) by the order it
//! first allocates in; a block carries the number of its heap in the 16
//! bytes in front of it, so it is freed or grown in that heap from any
//! thread. Blocks aligned to more than 16 bytes stay with `System`, as
//! before (the alignment is known again when they are freed). The choice
//! is made at the first allocation, from the environment read without
//! allocating, and never changes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

use windows_sys::Win32::System::Memory::{GetProcessHeap, HEAP_ZERO_MEMORY, HeapAlloc, HeapCreate, HeapFree, HeapReAlloc};

/// Bytes in front of each block: the number of its heap (16 keep the
/// block aligned as `HeapAlloc`'s are).
const HEADER: usize = 16;
/// The most heaps.
const MOST: usize = 16;

/// 0: not read yet; 1: off; 2 + n: `n` heaps.
static STATE: AtomicU32 = AtomicU32::new(0);
/// The heaps, made as threads first need them (0: not yet).
static HEAPS: [AtomicUsize; MOST] = [const { AtomicUsize::new(0) }; MOST];
/// Threads that took a heap so far.
static THREADS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// This thread's heap: its number plus one (0: none taken yet).
    static MINE: Cell<u8> = const { Cell::new(0) };
}

/// The number of heaps from `VICTORY_OWN_HEAP` (0: off), read with no
/// allocation.
fn asked() -> usize {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetEnvironmentVariableW(name: *const u16, buffer: *mut u16, size: u32) -> u32;
    }
    let name: [u16; 17] = {
        let mut n = [0u16; 17];
        for (d, s) in n.iter_mut().zip("VICTORY_OWN_HEAP".encode_utf16()) {
            *d = s;
        }
        n
    };
    let mut value = [0u16; 8];
    // SAFETY: a terminated name and a buffer of the length given.
    let len = unsafe { GetEnvironmentVariableW(name.as_ptr(), value.as_mut_ptr(), value.len() as u32) } as usize;
    // unset: 8 heaps, the measured default (about 8 ms a frame less on the
    // submitting thread, at the same list calls); `VICTORY_OWN_HEAP=0`: the process heap
    if len == 0 {
        return 8;
    }
    if len >= value.len() {
        return 0;
    }
    let mut n = 0usize;
    for &c in &value[..len] {
        match c {
            0x30..=0x39 => n = n * 10 + usize::from(c - 0x30),
            _ => return 0,
        }
    }
    n.min(MOST)
}

/// How many heaps of our own (0: the process heap).
fn heaps() -> usize {
    match STATE.load(Relaxed) {
        0 => {
            let n = asked();
            STATE.store(if n == 0 { 1 } else { 2 + n as u32 }, Relaxed);
            n
        }
        1 => 0,
        s => (s - 2) as usize,
    }
}

/// Heap `i`, made if it is not yet.
fn heap(i: usize) -> *mut c_void {
    let at = HEAPS[i].load(Relaxed);
    if at != 0 {
        return at as *mut c_void;
    }
    // SAFETY: a growable private heap of the default kind.
    let made = (unsafe { HeapCreate(0, 0, 0) }) as usize;
    // (If it cannot be made: the process heap, which is a heap like any.)
    // SAFETY: plain call.
    let made = if made == 0 { (unsafe { GetProcessHeap() }) as usize } else { made };
    match HEAPS[i].compare_exchange(0, made, Relaxed, Relaxed) {
        Ok(_) => made as *mut c_void,
        // Another thread made it first; ours is left unused (once at most
        // per heap).
        Err(there) => there as *mut c_void,
    }
}

/// This thread's heap number, taken the first time.
fn mine(n: usize) -> usize {
    let i = MINE.try_with(Cell::get).unwrap_or(0);
    if i != 0 {
        return usize::from(i - 1);
    }
    let i = THREADS.fetch_add(1, Relaxed) % n;
    let _ = MINE.try_with(|m| m.set(i as u8 + 1));
    i
}

/// Whether a block of `layout` is ours (when [`heaps`] is not 0).
fn ours(layout: Layout) -> bool {
    layout.align() <= HEADER
}

/// The allocator: [`System`] unless `VICTORY_OWN_HEAP` asks for heaps.
pub(crate) struct OwnHeap;

/// `n` heaps of our own (0: [`System`]).
struct Sharded(usize);

// SAFETY: as `Sharded`'s, with the number of heaps fixed for the process.
unsafe impl GlobalAlloc for OwnHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: passed on.
        unsafe { Sharded(heaps()).alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: passed on.
        unsafe { Sharded(heaps()).alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, at: *mut u8, layout: Layout) {
        // SAFETY: passed on; the number never changes, so the block is
        // freed by the kind of allocator that made it.
        unsafe { Sharded(heaps()).dealloc(at, layout) }
    }

    unsafe fn realloc(&self, at: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: passed on.
        unsafe { Sharded(heaps()).realloc(at, layout, size) }
    }
}

impl Sharded {
    unsafe fn take(&self, layout: Layout, flags: u32) -> *mut u8 {
        let n = self.0;
        if n == 0 || !ours(layout) {
            // SAFETY: the caller's contract, passed on.
            return unsafe { if flags != 0 { System.alloc_zeroed(layout) } else { System.alloc(layout) } };
        }
        let i = mine(n);
        // SAFETY: a heap of ours; the size cannot overflow for a valid
        // layout (at most isize::MAX).
        let base = unsafe { HeapAlloc(heap(i), flags, layout.size() + HEADER) }.cast::<u8>();
        if base.is_null() {
            return base;
        }
        // SAFETY: the block holds the header and the size asked.
        unsafe {
            base.cast::<usize>().write(i);
            base.add(HEADER)
        }
    }
}

// SAFETY: every block is a heap block of at least the size asked, aligned
// to 16 (`HeapAlloc`'s) plus the 16-byte header, or `System`'s; each is
// freed and grown where it was made.
unsafe impl GlobalAlloc for Sharded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: passed on.
        unsafe { self.take(layout, 0) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: passed on.
        unsafe { self.take(layout, HEAP_ZERO_MEMORY) }
    }

    unsafe fn dealloc(&self, at: *mut u8, layout: Layout) {
        if self.0 == 0 || !ours(layout) {
            // SAFETY: the caller's contract, passed on.
            return unsafe { System.dealloc(at, layout) };
        }
        // SAFETY: a block of ours: the header is in front of it.
        unsafe {
            let base = at.sub(HEADER);
            let i = base.cast::<usize>().read();
            HeapFree(heap(i), 0, base.cast());
        }
    }

    unsafe fn realloc(&self, at: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if self.0 == 0 || !ours(layout) {
            // SAFETY: the caller's contract, passed on.
            return unsafe { System.realloc(at, layout, size) };
        }
        // SAFETY: a block of ours; the header moves with the block.
        unsafe {
            let base = at.sub(HEADER);
            let i = base.cast::<usize>().read();
            let moved = HeapReAlloc(heap(i), 0, base.cast(), size + HEADER).cast::<u8>();
            if moved.is_null() { moved } else { moved.add(HEADER) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_of_our_heaps_keep_their_bytes_and_alignment_and_free_anywhere() {
        // Four heaps directly (the process's allocator is decided once).
        let a = Sharded(4);
        let mut blocks = Vec::new();
        for (k, size) in [1usize, 15, 16, 17, 4096, 70_000, 1 << 20].into_iter().enumerate() {
            let layout = Layout::from_size_align(size, 16).unwrap();
            // SAFETY: a valid layout; the block is ours until freed below.
            let p = unsafe { a.alloc(layout) };
            assert!(!p.is_null());
            assert_eq!(p as usize % 16, 0);
            // SAFETY: inside the block.
            unsafe { std::ptr::write_bytes(p, k as u8 + 1, size) };
            blocks.push((p, layout, k as u8 + 1));
        }
        // Grown, the bytes stay; freed on another thread.
        let (p, layout, v) = blocks[4];
        // SAFETY: a block of ours of `layout`.
        let q = unsafe { a.realloc(p, layout, 100_000) };
        // SAFETY: the first 4096 bytes are the old ones.
        assert!(unsafe { std::slice::from_raw_parts(q, 4096) }.iter().all(|&b| b == v));
        blocks[4] = (q, Layout::from_size_align(100_000, 16).unwrap(), v);
        let zeroed = Layout::from_size_align(300, 8).unwrap();
        // SAFETY: valid layout.
        let z = unsafe { a.alloc_zeroed(zeroed) };
        // SAFETY: inside the block.
        assert!(unsafe { std::slice::from_raw_parts(z, 300) }.iter().all(|&b| b == 0));
        // A block aligned to more than 16 goes to `System`.
        let wide = Layout::from_size_align(256, 64).unwrap();
        // SAFETY: valid layout.
        let w = unsafe { a.alloc(wide) };
        assert_eq!(w as usize % 64, 0);
        let sent: Vec<(usize, Layout)> = blocks.iter().map(|&(p, l, _)| (p as usize, l)).chain([(z as usize, zeroed), (w as usize, wide)]).collect();
        std::thread::spawn(move || {
            for (p, l) in sent {
                // SAFETY: each block once, with its layout.
                unsafe { Sharded(4).dealloc(p as *mut u8, l) };
            }
        })
        .join()
        .unwrap();
    }
}
