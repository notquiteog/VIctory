//! Structures' compacted sizes, given to the title before the card has them
//! (`VICTORY_BVH_SIZES_BOUND`; on with the structure builder, `0`: not).
//!
//! The title builds its dynamic ray-tracing structures with the builder of
//! `XboxBvhBuilder.dll` (`BuildRaytracingAccelerationStructureBatched2X`),
//! which asks the card, with each build, for the structure's compacted size:
//! a program of one thread (`GetBVHCompactedSize`, root constants: the
//! structure's address and mode 1) stores it into a slot of a small title
//! buffer. A frame or two later the title reads the slot on the CPU, without
//! a fence, takes memory of that size and has the structure copied there
//! (`CopyRaytracingAccelerationStructure`, compacting). On the console the
//! card is done by then. Here the lists are written down by the title's
//! threads and translated by ours (`VICTORY_THIN`), sent by a thread of ours
//! up to a present later (`crate::queue::behind`), run, and what the card
//! stored lands in title memory when the flight is settled: often after the
//! title has read the slot. It read what the slot held before, the size of
//! the structure asked for there last time, and a bigger structure copied
//! into memory taken for a smaller one wrote over its neighbours (run
//! light-11: 53 of 230 compacting copies; waiting for the asking submission,
//! `VICTORY_BVH_SIZES_WAIT=1`, waits on our thread, not on the title's, and
//! did not help).
//!
//! What the card does not have yet the CPU cannot have either: the
//! structure, header and all, is built on the card and stays there. What the
//! CPU can have is a bound: the structure's size before compacting is at most
//! what the builder's own `GetRaytracingAccelerationStructurePrebuildInfo`
//! said (`ResultDataMaxSizeInBytes`), and compacting never makes it bigger.
//! So:
//!
//! 1. the title's calls of that function are seen (`victory-hle`'s
//!    `modules::bvh` wraps the title's imports of it): what its inputs were
//!    (counts, formats, flags: what the answer depends on, [`key`]) and the
//!    answer, kept by that key ([`prebuilt`]);
//! 2. at a batched build, before the builder records anything, the build
//!    descriptions the title passes are looked through for each structure's
//!    destination and inputs ([`batch`], [`scan`]; the layout of the batch's
//!    own description is not public, the build description in it is
//!    `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC`), and a structure
//!    whose inputs have an answer is given it as its bound;
//! 3. when the builder's size program is translated, the slot is given that
//!    bound in title memory at once ([`asked`]); the card's exact size
//!    replaces it when it lands. A read before then takes memory enough.
//! 4. An earlier asking of the same slot that lands after this one was given
//!    its bound (a flight still in the air) would put the old structure's size
//!    back: what lands is followed by a look ([`landed`], a bottom-of-pipe
//!    write of the asking list's), and the latest bound goes back in.
//!
//! What is not known is left as it was: a structure whose inputs were not
//! found, or were not asked about, gets no bound.
//!
//! shortcut: the memory a bound takes is the structure's size before
//! compacting: no smaller than before the title compacts, but bigger than it
//! needs until the card's size has landed.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

/// Bytes of `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS` and of a
/// `D3D12_RAYTRACING_GEOMETRY_DESC`.
const INPUTS: usize = 24;
const GEOMETRY: usize = 56;
/// A build description: the destination, then the inputs.
const RECORD: usize = 8 + INPUTS;
/// The most geometries of a structure looked at.
const GEOMETRIES: u32 = 4096;
/// The most structures of a batch looked at, and the bytes looked through
/// for each (the batch's own description is not known).
const ENTRIES: u32 = 4096;
const STRIDE: usize = 256;
/// For so many frames after its batch a structure's bound is used.
const RECENT: u64 = 8;
/// Answers kept, at most.
const KEPT: usize = 1 << 16;

/// Reads title (or the title's CPU) memory: whether it was all there.
pub(crate) type Read<'a> = &'a dyn Fn(u64, &mut [u8]) -> bool;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// A pointer of the title's CPU into its own memory.
fn pointer(p: u64) -> bool {
    (0x1_0000..1 << 47).contains(&p) && p % 8 == 0
}

/// A structure's address: 256-byte aligned, title GPU memory.
fn structure(p: u64) -> bool {
    (1 << 32..1 << 40).contains(&p) && p % 256 == 0
}

/// What a prebuild answer depends on, of the inputs `inputs` (the bytes of a
/// `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS`): kind, flags,
/// count, layout, and of each geometry its kind, flags and counts and formats
/// (all of it for a kind not known). Not where anything is. `None` if these
/// are not inputs, or what they point at is not there.
pub(crate) fn key(inputs: &[u8], read: Read) -> Option<u64> {
    let (kind, flags, count, layout, at) =
        (u32_at(inputs, 0), u32_at(inputs, 4), u32_at(inputs, 8), u32_at(inputs, 12), u64_at(inputs, 16));
    // Top level 0, bottom level 1; an array, or an array of pointers.
    if kind > 1 || layout > 1 || count == 0 || count > 1 << 20 {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, flags, count, layout).hash(&mut h);
    // A top level's answer is its instances' count.
    if kind == 0 {
        return Some(h.finish());
    }
    if count > GEOMETRIES || !pointer(at) {
        return None;
    }
    for i in 0..u64::from(count) {
        let desc = if layout == 0 {
            at + i * GEOMETRY as u64
        } else {
            let mut p = [0u8; 8];
            if !read(at + i * 8, &mut p) || !pointer(u64::from_le_bytes(p)) {
                return None;
            }
            u64::from_le_bytes(p)
        };
        let mut g = [0u8; GEOMETRY];
        if !read(desc, &mut g) {
            return None;
        }
        let kind = u32_at(&g, 0);
        (kind, u32_at(&g, 4)).hash(&mut h);
        match kind {
            // Triangles: a transform or not, index and vertex formats and
            // counts.
            0 => (u64_at(&g, 8) != 0, u32_at(&g, 16), u32_at(&g, 20), u32_at(&g, 24), u32_at(&g, 28)).hash(&mut h),
            // Boxes: their count.
            1 => u64_at(&g, 8).hash(&mut h),
            _ => g[8..].hash(&mut h),
        }
    }
    Some(h.finish())
}

/// A build description at the start of `bytes`: its destination and the
/// bound its inputs were answered, if they were.
fn record(bytes: &[u8], read: Read, answers: &HashMap<u64, (u64, u64)>) -> Option<(u64, u64)> {
    let dest = u64_at(bytes, 0);
    if !structure(dest) {
        return None;
    }
    let inputs = &bytes[8..RECORD];
    // (Cheap tests first: most places are not inputs.)
    if u32_at(inputs, 0) > 1 || u32_at(inputs, 12) > 1 || u32_at(inputs, 8) == 0 {
        return None;
    }
    let key = key(inputs, read)?;
    answers.get(&key).map(|&(bound, _)| (dest, bound))
}

/// The structures of a batch of `count` builds described at `descs`, and
/// their bounds: build descriptions in what is there, or in what its words
/// point at (the batch's description may hold one or point at one). The
/// biggest bound where a structure is named twice.
pub(crate) fn scan(descs: u64, count: u32, read: Read, answers: &HashMap<u64, (u64, u64)>) -> Vec<(u64, u64)> {
    let mut found: Vec<(u64, u64)> = Vec::new();
    if answers.is_empty() || !pointer(descs) || count == 0 {
        return found;
    }
    let len = count.min(ENTRIES) as usize * STRIDE;
    let mut bytes = vec![0u8; len + RECORD];
    // Page by page; what is not there reads as zeros, and names nothing.
    let mut from = 0;
    while from < len {
        let to = (((descs + from as u64) / 4096 + 1) * 4096 - descs).min(len as u64) as usize;
        let piece = &mut bytes[from..to];
        if !read(descs + from as u64, piece) {
            piece.fill(0);
        }
        from = to;
    }
    let add =|found: &mut Vec<(u64, u64)>, (dest, bound): (u64, u64)| match found.iter_mut().find(|f| f.0 == dest) {
        Some(f) => f.1 = f.1.max(bound),
        None => found.push((dest, bound)),
    };
    for at in (0..len).step_by(8) {
        if let Some(r) = record(&bytes[at..at + RECORD], read, answers) {
            add(&mut found, r);
        }
    }
    if found.len() >= count as usize {
        return found;
    }
    let mut seen = std::collections::HashSet::new();
    for at in (0..len).step_by(8) {
        let p = u64_at(&bytes, at);
        if !pointer(p) || structure(p) || !seen.insert(p) {
            continue;
        }
        let mut there = [0u8; 64];
        if !read(p, &mut there) {
            continue;
        }
        for at in (0..=there.len() - RECORD).step_by(8) {
            if let Some(r) = record(&there[at..at + RECORD], read, answers) {
                add(&mut found, r);
            }
        }
    }
    found
}

/// A slot sizes are asked into: its latest asking, and the bound it was
/// given (none: its structure had none).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Slot {
    asking: u32,
    bound: Option<u64>,
}

#[derive(Default)]
struct State {
    /// By [`key`]: the biggest answer, and the frame it was last given in.
    answers: HashMap<u64, (u64, u64)>,
    /// By structure: its bound, and the frame of its batch.
    built: HashMap<u64, (u64, u64)>,
    slots: HashMap<u64, Slot>,
    stats: Stats,
}

/// Since the last report.
#[derive(Default)]
struct Stats {
    from: Option<std::time::Instant>,
    answers: u64,
    batches: u64,
    found: u64,
    bound: u64,
    unbound: u64,
    undone: u64,
}

impl State {
    fn answered(&mut self, key: u64, bound: u64, frame: u64) {
        if self.answers.len() >= KEPT && !self.answers.contains_key(&key) {
            // The older half goes.
            let mut frames: Vec<u64> = self.answers.values().map(|a| a.1).collect();
            frames.sort_unstable();
            let cut = frames[frames.len() / 2];
            self.answers.retain(|_, a| a.1 > cut);
        }
        let a = self.answers.entry(key).or_insert((0, frame));
        *a = (a.0.max(bound), frame);
        self.stats.answers += 1;
    }

    /// A batch's structures and bounds: each build replaces what the last
    /// one at that address had.
    fn batch(&mut self, found: &[(u64, u64)], frame: u64) {
        self.built.retain(|_, b| b.1 + RECENT >= frame);
        for &(dest, bound) in found {
            self.built.insert(dest, (bound, frame));
        }
        self.stats.batches += 1;
        self.stats.found += found.len() as u64;
    }

    /// `structure`'s compacted size is asked into `slot`: the asking's number
    /// and the bound to put there, if there is one.
    fn ask(&mut self, structure: u64, slot: u64, frame: u64) -> (u32, Option<u64>) {
        let bound = self.built.get(&structure).filter(|b| b.1 + RECENT >= frame).map(|b| b.0);
        let s = self.slots.entry(slot).or_insert(Slot { asking: 0, bound: None });
        s.asking = s.asking.wrapping_add(1);
        s.bound = bound;
        if bound.is_some() {
            self.stats.bound += 1;
        } else {
            self.stats.unbound += 1;
        }
        (s.asking, bound)
    }

    /// What the asking `asking` of `slot` stored has landed: what to put
    /// back there, if a later asking gave it a bound.
    fn landed(&mut self, slot: u64, asking: u32) -> Option<u64> {
        let s = self.slots.get(&slot)?;
        let back = (s.asking != asking).then_some(s.bound).flatten();
        if back.is_some() {
            self.stats.undone += 1;
        }
        back
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

/// Whether bounds are given: with the structure builder
/// (`VICTORY_BVH_BUILDER=1`), unless `VICTORY_BVH_SIZES_BOUND=0`.
pub fn wanted() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        victory_shader::bvh_builder_on()
            && std::env::var_os("VICTORY_BVH_SIZES_BOUND").is_none_or(|v| v != "0")
    })
}

fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_TRACE").is_some_and(|v| v == "1"))
}

#[cfg(windows)]
fn title_read(at: u64, bytes: &mut [u8]) -> bool {
    crate::host::read_into(at as *const u8, bytes)
}

/// The builder's `GetRaytracingAccelerationStructurePrebuildInfo` answered
/// `bound` (its `ResultDataMaxSizeInBytes`) for the inputs at `inputs`.
#[cfg(windows)]
pub fn prebuilt(inputs: u64, bound: u64) {
    if !wanted() || !pointer(inputs) || bound == 0 {
        return;
    }
    let mut bytes = [0u8; INPUTS];
    if !title_read(inputs, &mut bytes) {
        return;
    }
    if let Some(key) = key(&bytes, &title_read) {
        let frame = crate::frame::current();
        with(|s| s.answered(key, bound, frame));
    }
}

/// The title asks the builder for a batch of `count` builds described at
/// `descs` (before the builder records them).
#[cfg(windows)]
pub fn batch(descs: u64, count: u32) {
    if !wanted() {
        return;
    }
    let frame = crate::frame::current();
    with(|s| {
        let found = scan(descs, count, &title_read, &s.answers);
        s.batch(&found, frame);
        if trace() && s.stats.batches <= 64 {
            tracing::info!("bvh trace: frame {frame}: a batch of {count} builds, {} structures with bounds {found:x?}", found.len());
        }
    });
}

/// When the builder's program of a structure's size is translated: its
/// structure, and the slot it stores into. The slot is given the structure's
/// bound in title memory, and `list`, once it has run, looks whether a later
/// asking's bound must go back ([`landed`]). The bound given, if any.
#[cfg(windows)]
pub(crate) fn asked(list: &crate::host::HostList, structure: u64, slot: u64) -> Option<u64> {
    if !wanted() || slot % 8 != 0 {
        return None;
    }
    let frame = crate::frame::current();
    let (asking, bound) = with(|s| {
        let r = s.ask(structure, slot, frame);
        s.report();
        r
    });
    let bound = bound?;
    put(slot, bound);
    list.defer_write(slot as *mut u8, crate::queue::Written::SizeLanded(asking), 8);
    Some(bound)
}

/// What the asking `asking` of `slot` stored has landed (a bottom-of-pipe
/// write of its list's, after the landing).
#[cfg(windows)]
pub(crate) fn landed(slot: u64, asking: u32) {
    if let Some(bound) = with(|s| s.landed(slot, asking)) {
        put(slot, bound);
    }
}

/// The bound of the latest asking into `slot`, for the trace.
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn bound_at(slot: u64) -> Option<u64> {
    with(|s| s.slots.get(&slot).and_then(|s| s.bound))
}

#[cfg(windows)]
fn put(slot: u64, bound: u64) {
    let bytes = bound.to_le_bytes();
    crate::host::copy_mapped(slot as *mut u8, bytes.as_ptr(), 8, true);
    crate::watch::wrote(slot as usize, 8);
}

impl State {
    /// Every ten seconds or so, what was done.
    fn report(&mut self) {
        let now = std::time::Instant::now();
        let since = *self.stats.from.get_or_insert(now);
        if now.duration_since(since).as_secs_f64() < 10.0 {
            return;
        }
        let s = std::mem::take(&mut self.stats);
        tracing::info!(
            answers = s.answers,
            batches = s.batches,
            structures = s.found,
            asked_with_bound = s.bound,
            asked_without = s.unbound,
            put_back = s.undone,
            kept = self.answers.len(),
            "structure sizes (VICTORY_BVH_SIZES_BOUND): prebuild answers seen, batches looked through and structures found in them, \
             sizes asked with a bound and without, bounds put back over a late landing, since the last line"
        );
        self.stats.from = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Memory of the test: blocks at addresses.
    struct Memory(Vec<(u64, Vec<u8>)>);

    impl Memory {
        fn read(&self, at: u64, bytes: &mut [u8]) -> bool {
            for (start, data) in &self.0 {
                if at >= *start && at + bytes.len() as u64 <= start + data.len() as u64 {
                    let from = (at - start) as usize;
                    bytes.copy_from_slice(&data[from..from + bytes.len()]);
                    return true;
                }
            }
            false
        }
    }

    fn inputs(kind: u32, flags: u32, count: u32, layout: u32, at: u64) -> Vec<u8> {
        let mut b = Vec::new();
        for w in [kind, flags, count, layout] {
            b.extend(w.to_le_bytes());
        }
        b.extend(at.to_le_bytes());
        b
    }

    fn triangles(indices: u32, vertices: u32, buffer: u64) -> Vec<u8> {
        let mut g = vec![0u8; GEOMETRY];
        g[16..20].copy_from_slice(&2u32.to_le_bytes());
        g[20..24].copy_from_slice(&6u32.to_le_bytes());
        g[24..28].copy_from_slice(&indices.to_le_bytes());
        g[28..32].copy_from_slice(&vertices.to_le_bytes());
        g[32..40].copy_from_slice(&buffer.to_le_bytes());
        g
    }

    const GEOMS: u64 = 0x7ff0_0000;

    #[test]
    fn the_key_is_of_counts_not_of_places() {
        let a = Memory(vec![(GEOMS, [triangles(300, 120, 0x5_0000_0000), triangles(30, 12, 0x5_0001_0000)].concat())]);
        let b = Memory(vec![(GEOMS + 0x1000, [triangles(300, 120, 0x6_0000_0000), triangles(30, 12, 0x6_0001_0000)].concat())]);
        let ka = key(&inputs(1, 0x4, 2, 0, GEOMS), &|at, b| a.read(at, b));
        let kb = key(&inputs(1, 0x4, 2, 0, GEOMS + 0x1000), &|at, x| b.read(at, x));
        assert!(ka.is_some());
        assert_eq!(ka, kb);
        // Another count, other flags: another key.
        let c = Memory(vec![(GEOMS, [triangles(303, 120, 0), triangles(30, 12, 0)].concat())]);
        assert_ne!(key(&inputs(1, 0x4, 2, 0, GEOMS), &|at, b| c.read(at, b)), ka);
        assert_ne!(key(&inputs(1, 0x8, 2, 0, GEOMS), &|at, x| a.read(at, x)), ka);
        // An array of pointers to the same geometries: the same answer.
        let pointers = Memory(vec![
            (GEOMS, [triangles(300, 120, 0), triangles(30, 12, 0)].concat()),
            (0x7fe0_0000, [GEOMS.to_le_bytes(), (GEOMS + GEOMETRY as u64).to_le_bytes()].concat()),
        ]);
        let kp = key(&inputs(1, 0x4, 2, 1, 0x7fe0_0000), &|at, b| pointers.read(at, b));
        assert!(kp.is_some());
        // Not inputs, or geometries not there.
        let none = Memory(vec![]);
        assert_eq!(key(&inputs(2, 0, 1, 0, GEOMS), &|at, b| none.read(at, b)), None);
        assert_eq!(key(&inputs(1, 0, 0, 0, GEOMS), &|at, b| none.read(at, b)), None);
        assert_eq!(key(&inputs(1, 0, 1, 0, GEOMS), &|at, b| none.read(at, b)), None);
        // A top level: its instance count alone, nothing read.
        assert!(key(&inputs(0, 0, 40, 0, 0x5_0000_0000), &|at, b| none.read(at, b)).is_some());
    }

    /// Build descriptions in a batch, in place or pointed at, are found by
    /// their inputs; a structure whose inputs were not answered is not.
    #[test]
    fn a_batch_names_its_structures_and_their_bounds() {
        let geometries = [triangles(300, 120, 0), triangles(9, 3, 0)].concat();
        let geom = |i: u64| GEOMS + i * GEOMETRY as u64;
        let mem = Memory(vec![(GEOMS, geometries.clone())]);
        let read = |at: u64, b: &mut [u8]| mem.read(at, b);
        let mut answers = HashMap::new();
        answers.insert(key(&inputs(1, 0, 1, 0, geom(0)), &read).unwrap(), (0x4_0000, 1));
        // A batch entry of an unknown layout: 16 bytes of something, then a
        // build description (destination, inputs, source, scratch).
        let entry = |dest: u64, g: u64| {
            let mut e = vec![0x11u8; 16];
            e.extend(dest.to_le_bytes());
            e.extend(inputs(1, 0, 1, 0, g));
            e.extend(0u64.to_le_bytes());
            e.extend(0x9_0000_0000u64.to_le_bytes());
            e.resize(96, 0);
            e
        };
        let descs = 0x7f00_0000u64;
        // The second structure's inputs were never answered.
        let mut batch = [entry(0x3_0000_0100, geom(0)), entry(0x3_0000_0200, geom(1))].concat();
        batch.resize(2 * STRIDE, 0);
        let mem2 = Memory(vec![(GEOMS, geometries.clone()), (descs, batch)]);
        let read2 = |at: u64, b: &mut [u8]| mem2.read(at, b);
        assert_eq!(scan(descs, 2, &read2, &answers), vec![(0x3_0000_0100, 0x4_0000)]);
        // Entries that point at their descriptions.
        let at = 0x7e00_0000u64;
        let mut described = entry(0x3_0000_0300, geom(0))[16..16 + 48].to_vec();
        described.resize(64, 0);
        let mut pointing = vec![0u8; 8];
        pointing.extend(at.to_le_bytes());
        pointing.resize(STRIDE, 0);
        let mem3 = Memory(vec![(GEOMS, geometries), (descs, pointing), (at, described)]);
        let read3 = |a: u64, b: &mut [u8]| mem3.read(a, b);
        assert_eq!(scan(descs, 1, &read3, &answers), vec![(0x3_0000_0300, 0x4_0000)]);
        // Nothing answered: nothing looked at.
        assert!(scan(descs, 1, &read3, &HashMap::new()).is_empty());
    }

    #[test]
    fn an_answer_is_the_biggest_given() {
        let mut s = State::default();
        s.answered(7, 0x1000, 1);
        s.answered(7, 0x800, 2);
        assert_eq!(s.answers[&7], (0x1000, 2));
    }

    /// The slot takes the bound of its latest asking; an earlier asking that
    /// lands after it was given puts nothing over it for good.
    #[test]
    fn a_late_landing_gives_the_slot_its_bound_back() {
        let mut s = State::default();
        let (slot, a, b) = (0x2_0000_0040, 0x3_0000_0100, 0x3_0000_0200);
        s.batch(&[(a, 0x4000)], 10);
        let (first, bound) = s.ask(a, slot, 10);
        assert_eq!(bound, Some(0x4000));
        // Its own landing: the card's size stands.
        assert_eq!(s.landed(slot, first), None);
        s.batch(&[(b, 0x9000)], 12);
        let (second, bound) = s.ask(b, slot, 12);
        assert_eq!(bound, Some(0x9000));
        // The first asking lands only now: the second's bound goes back.
        assert_eq!(s.landed(slot, first), Some(0x9000));
        assert_eq!(s.landed(slot, second), None);
        // A structure without a bound: nothing is put back for it either.
        let (_, none) = s.ask(0x3_0000_0300, slot, 13);
        assert_eq!(none, None);
        assert_eq!(s.landed(slot, second), None);
        // A batch long ago gives no bound.
        assert_eq!(s.ask(b, slot, 12 + RECENT + 1).1, None);
        // Unknown slots: nothing.
        assert_eq!(s.landed(0x40, 1), None);
    }

    #[test]
    fn a_rebuild_replaces_the_bound() {
        let mut s = State::default();
        s.batch(&[(0x3_0000_0100, 0x4000)], 1);
        s.batch(&[(0x3_0000_0100, 0x2000)], 2);
        assert_eq!(s.ask(0x3_0000_0100, 0x2_0000_0000, 2).1, Some(0x2000));
    }
}
