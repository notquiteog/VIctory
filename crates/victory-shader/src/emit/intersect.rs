//! The console's ray-tracing node test, `image_bvh_intersect_ray` and
//! `image_bvh64_intersect_ray`, as inline DXIL: the host has no instruction
//! that tests one ray against one node of the title's own structures.
//!
//! [`crate::bvh`] says what the instruction computes and is the reference:
//! the code made here does the same float operations in the same order
//! (DXIL float operations carry no fast-math flags, so the host may not
//! reorder or fuse them), and the tests in `intersect_tests.rs` run it on
//! the card against the reference.
//!
//! The title's nodes are in title memory, named by address: they are read
//! through the host's copies of title memory ([`Lower::memory`]), one look
//! in its directory per node (two for a 128-byte box node).
//!
//! Shape of the code, under the instruction's lane guard:
//!
//! ```text
//! kind = pointer & 7
//! box (4, 5):  4 child pointers; 24 box floats (kind 5) or 24 halves widened (kind 4);
//!              four slab tests; the sorting network
//! triangle (0-3): the id dword, three vertices at the bytes it names; the watertight test
//! other (6, 7): a triangle miss
//! ```
//!
//! Each lane reads only the dwords its node's kind needs: 28 for a 32-bit
//! box node, 16 for a 16-bit one, 10 for a triangle.

use super::{Attr, Bin, Fields, Inst, Lower, Operand, Reg, Term, V, Why, flags, pred};
use crate::bvh;
use crate::resource::BvhNodes;

#[cfg(test)]
thread_local! {
    /// The tests' stand-in for title memory ([`Lower::bvh_memory`]): a
    /// heap slot holding a raw buffer, and the title address of its first
    /// byte. (The card tests have no host to keep copies of title memory
    /// and their directory.)
    pub(in crate::emit) static TEST_MEMORY: std::cell::Cell<Option<(u32, u64)>> =
        const { std::cell::Cell::new(None) };
    /// The tests' way to translate as with `VICTORY_BVH_NO_HIT=1`
    /// ([`no_hit`]).
    pub(in crate::emit) static TEST_NO_HIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The tests' choice for [`node_once`].
    pub(in crate::emit) static TEST_NODE_ONCE: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
    /// The tests' choice for [`super::super::bvh_walk_guard`].
    pub(in crate::emit) static TEST_WALK_GUARD: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

/// Whether a node test answers "no hit" and reads nothing
/// (`VICTORY_BVH_NO_HIT=1`; off by default; never with
/// the structure builder on, which has the structures built).
///
/// The title's structure builder does not run in the layer unless asked
/// for, and the title's ray-traced passes run all the same: each node test
/// then reads whatever bytes lie where a structure would be, in a walk the
/// title bounds at 65,535 steps a wave. What comes back is not the scene:
/// it differs from launch to launch, and how long a pass runs depends on
/// those bytes. With the switch a box node enters no child and any other
/// node is missed, as in a scene with nothing in it: every ray leaves at
/// its first test, shadow rays find no occluder, and the answer is the same
/// in every run. (The idea is fork pull request #994's, which asks a host
/// word at run time; here it is a translation, since the builder's state
/// does not change during a run of ours.)
fn no_hit() -> bool {
    #[cfg(test)]
    if TEST_NO_HIT.get() {
        return true;
    }
    std::env::var_os("VICTORY_BVH_NO_HIT").is_some_and(|v| v == "1")
        && !crate::bvh_builder_on()
}

/// How a node test reads its node (`VICTORY_BVH_NODE_ONCE`):
/// - `1`: the node's first 64 bytes at once, before it looks at the node's
///   kind. Every kind's dwords are in those 64 bytes but a 32-bit box node's
///   last twelve, and they lie in one page of title memory.
/// - `2`: each kind in its own branch, a triangle node's 64 bytes at once
///   there and its vertices picked from them.
/// - `0` (the default): each kind its own dwords in its own branch. A wave
///   whose lanes are at nodes of several kinds waits for memory once for
///   each kind, and a triangle's vertices are read only after the dword
///   that says which they are: three waits for memory one after another
///   (the directory, that dword, the vertices) where the hardware has one.
fn node_once() -> u8 {
    #[cfg(test)]
    if let Some(mode) = TEST_NODE_ONCE.get() {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_BVH_NODE_ONCE").ok().as_deref().map(str::trim) {
        Some("1") => 1,
        Some("2") => 2,
        _ => 0,
    })
}

/// Where the instruction's node is.
#[derive(Clone, Copy)]
enum NodeAt {
    /// At this byte of a raw buffer of the host.
    Buffer { handle: V, byte: V },
    /// In title memory: the raw view of the host copy holding the node's
    /// first 64 bytes and the node's byte in it ([`Lower::bvh_memory`]),
    /// the node's address, for the second half of a 128-byte node, and
    /// whether there is a copy.
    Memory { handle: V, byte: V, low: V, high: V, mapped: V },
}

/// The ray operands, as floats.
struct Ray {
    extent: V,
    origin: [V; 3],
    direction: [V; 3],
    inverse: [V; 3],
}

/// LOCAL (from our wt5): `VICTORY_BVH_MISS=1` (off by default): the node test
/// misses every node. While the structure builder is off the structures are
/// never built, and a walk of what lies in their memory is not the console's.
fn bvh_miss() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_MISS").is_some_and(|v| v == "1"))
}

impl<'a> Lower<'a> {
    /// `image_bvh_intersect_ray` and `image_bvh64_intersect_ray`.
    pub(super) fn bvh(&mut self, inst: &Inst, b: usize, i: usize) -> Result<(), Why> {
        let Fields::Image {
            data,
            addr,
            rsrc,
            dmask,
            a16,
            d16,
            tfe,
            lwe,
            ..
        } = &inst.fields
        else {
            unreachable!()
        };
        if *a16 || *d16 || *tfe || *lwe || *dmask != 0xf {
            return Err("a ray-tracing node test with 16-bit operands or a partial result");
        }
        let wide = inst.name() == "image_bvh64_intersect_ray";
        // Operand VGPRs in order: the node pointer (two dwords in the
        // 64-bit form), the ray's extent, origin, direction and inverse
        // direction.
        let mut regs = Vec::new();
        for a in addr {
            if let Operand::Vector { reg, dwords } = *a {
                regs.extend((0..dwords).map(|k| reg + k));
            }
        }
        if regs.len() != if wide { 12 } else { 11 } {
            return Err("a ray-tracing node test's operands");
        }
        let Operand::Vector { reg: dst, .. } = *data else {
            return Err("a ray-tracing node test's result");
        };
        let site = (b, i);
        let nodes = self
            .x
            .res
            .bvh
            .iter()
            .find(|n| n.0 == site)
            .map(|n| n.1)
            .ok_or("an untracked ray-tracing structure")?;
        let (sort, barycentrics) = match nodes {
            BvhNodes::Resource => (true, true),
            BvhNodes::Memory { word1, word3 } => (
                word1 & bvh::BOX_SORT != 0,
                word3 & bvh::RETURN_BARYCENTRICS != 0,
            ),
        };
        if !barycentrics {
            // Mode 0 returns a triangle id and a hit status in the last two
            // dwords; no program of the title asks for it.
            return Err("a ray-tracing structure that returns triangle ids");
        }
        let guard = self.guard(b, i)?;
        let mut x = Vec::with_capacity(regs.len());
        for &r in &regs {
            let v = inst.read(Reg::V(r)).ok_or("an unread operand")?;
            x.push(self.i32_of(v)?);
        }
        let pointer = x[0];
        let pointer_high = wide.then(|| x[1]);
        let f: Vec<V> = x[1 + usize::from(wide)..]
            .iter()
            .map(|&v| self.as_float(v))
            .collect();
        let ray = Ray {
            extent: f[0],
            origin: [f[1], f[2], f[3]],
            direction: [f[4], f[5], f[6]],
            inverse: [f[7], f[8], f[9]],
        };
        let c = self.cur;
        if no_hit() {
            // No structures: a box enters no child, anything else is
            // missed ([`no_hit`]). Nothing is read, and nothing asked of
            // the directory of title memory.
            let _ = &ray;
            let (k4, k6) = (self.s.m.i32(4), self.s.m.i32(6));
            let pair = self.s.m.bin(c, Bin::And, pointer, k6);
            let is_box = self.s.m.cmp(c, pred::EQ, pair, k4);
            let none = self.s.m.i32(bvh::NONE);
            for (k, miss) in bvh::MISS.into_iter().enumerate() {
                let miss = self.s.m.i32(miss.to_bits());
                let r = self.s.m.select(c, is_box, none, miss);
                self.write(inst, Reg::V(dst + k as u8), r, guard);
            }
            return Ok(());
        }
        let at = match nodes {
            BvhNodes::Resource => {
                let src = self
                    .descriptors
                    .get(&(site, false))
                    .copied()
                    .ok_or("an untracked ray-tracing structure")?;
                let handle = self.handle(site, src, false, (11, 0))?;
                // (pointer >> 3) << 6 from the buffer's start.
                let (kind_bits, k3) = (self.s.m.i32(!7), self.s.m.i32(3));
                let byte = self.s.m.bin(c, Bin::And, pointer, kind_bits);
                let byte = self.s.m.bin(c, Bin::Shl, byte, k3);
                NodeAt::Buffer { handle, byte }
            }
            BvhNodes::Memory { .. } => {
                let Operand::Scalar { code, .. } = *rsrc else {
                    return Err("a ray-tracing structure descriptor not in SGPRs");
                };
                let mut words = [pointer; 2];
                for (k, w) in words.iter_mut().enumerate() {
                    let v = inst
                        .read(Reg::S(code + k as u8))
                        .ok_or("an unread operand")?;
                    *w = self.i32_of(v)?;
                }
                let (low, high) = self.bvh_address(words[0], words[1], pointer, pointer_high);
                // One look for the node's copy, before the branches: a
                // node is 64 bytes on 64, so its first half lies in one
                // 64 KiB page whatever its kind.
                let (handle, byte, mapped) = self.bvh_memory(low, high, guard);
                NodeAt::Memory {
                    handle,
                    byte,
                    low,
                    high,
                    mapped,
                }
            }
        };
        let zero = self.s.m.i32(0);
        let guarding = super::super::bvh_walk_guard() > 0;
        let gate = self.begin_guarded(guard);
        let head = self.cur;
        let (k4, k6, k7) = (self.s.m.i32(4), self.s.m.i32(6), self.s.m.i32(7));
        let kind = self.s.m.bin(head, Bin::And, pointer, k7);
        let pair = self.s.m.bin(head, Bin::And, pointer, k6);
        let is_box = self.s.m.cmp(head, pred::EQ, pair, k4);
        let is_triangle = self.s.m.cmp(head, pred::IULT, kind, k4);
        // The first 64 bytes, for every kind ([`node_once`]).
        let first: Option<Vec<V>> = (node_once() == 1 && !bvh_miss()).then(|| self.bvh_node_words(at, None, 0, 16));
        let head = self.cur;
        let (box_block, leaf_block) = (self.s.m.new_block(), self.s.m.new_block());
        let (triangle_block, join) = (self.s.m.new_block(), self.s.m.new_block());
        self.s
            .m
            .terminate(head, Term::CondBr(is_box, box_block, leaf_block));
        self.s
            .m
            .terminate(leaf_block, Term::CondBr(is_triangle, triangle_block, join));
        self.cur = box_block;
        // LOCAL (from our wt5): with `VICTORY_BVH_MISS=1` every node is missed
        // without being read: a box returns no child and a triangle the
        // triangle miss, so a walk ends at its first node.
        let mut box_cuts = None;
        let boxes = if bvh_miss() {
            [self.s.m.i32(bvh::NONE); 4]
        } else {
            let (ids, cut) = self.bvh_boxes(at, kind, &ray, sort, first.as_deref(), (pointer, pointer_high));
            box_cuts = cut;
            ids
        };
        let box_end = self.cur;
        self.s.m.terminate(box_end, Term::Br(join));
        self.cur = triangle_block;
        let triangle = if bvh_miss() {
            bvh::MISS.map(|f| self.s.m.i32(f.to_bits()))
        } else {
            let first = first.or_else(|| (node_once() == 2).then(|| self.bvh_node_words(at, None, 0, 16)));
            let hit = self.bvh_triangle(at, kind, &ray, first.as_deref());
            // A triangle on a page without a copy is missed, not tested with
            // zeros ([`super::super::bvh_walk_guard`]).
            match at {
                NodeAt::Memory { mapped, .. } if guarding => {
                    let c = self.cur;
                    let miss = bvh::MISS.map(|f| self.s.m.i32(f.to_bits()));
                    std::array::from_fn(|k| self.s.m.select(c, mapped, hit[k], miss[k]))
                }
                _ => hit,
            }
        };
        let triangle_end = self.cur;
        self.s.m.terminate(triangle_end, Term::Br(join));
        self.cur = join;
        // Kinds 6 and 7 (an instance, a procedural primitive): not known
        // what the console returns; the title's programs do not read it.
        let other = bvh::MISS.map(|f| self.s.m.i32(f.to_bits()));
        let i32t = self.s.t.i32;
        let mut results = Vec::with_capacity(4);
        for k in 0..4 {
            let phi = self.s.m.phi(join, i32t);
            self.s.m.set_phi(
                phi,
                vec![
                    (boxes[k], box_end),
                    (triangle[k], triangle_end),
                    (other[k], leaf_block),
                ],
            );
            results.push((phi, zero));
        }
        // (Counted where nodes are in title memory, whose programs have the
        // faults buffer already; the title's are.)
        if guarding && matches!(at, NodeAt::Memory { .. }) {
            // What the guard cut here: children of a box, a triangle missed.
            let cut = self.s.m.phi(join, i32t);
            let triangle_cut = match at {
                NodeAt::Memory { mapped, .. } => {
                    let (one, yes) = (self.s.m.i32(1), self.s.m.bool(true));
                    let gone = self.s.m.bin(triangle_end, Bin::Xor, mapped, yes);
                    self.s.m.select(triangle_end, gone, one, zero)
                }
                NodeAt::Buffer { .. } => zero,
            };
            self.s.m.set_phi(
                cut,
                vec![(box_cuts.unwrap_or(zero), box_end), (triangle_cut, triangle_end), (zero, leaf_block)],
            );
            let some = self.s.m.cmp(join, pred::NE, cut, zero);
            self.count_walk(super::super::FAULTS_GUARD_CUTS, cut, some);
        }
        let results = self.end_guarded(gate, &results);
        for (k, &r) in results.iter().enumerate() {
            self.write(inst, Reg::V(dst + k as u8), r, guard);
        }
        Ok(())
    }

    /// Title memory at `low`/`high` for a node's bytes: the raw view of the
    /// host copy holding the address and the byte in that view
    /// ([`Lower::memory`]: a page the host has no copy of reads zeros and
    /// is left for the host to fetch, for lanes that run).
    fn bvh_memory(&mut self, low: V, high: V, guard: Option<V>) -> (V, V, V) {
        #[cfg(test)]
        if let Some((slot, base)) = TEST_MEMORY.get() {
            // The buffer at heap slot `slot` holds the bytes from title
            // address `base` on; any other address reads past its end,
            // which is zeros.
            let c = self.cur;
            let slot = self.s.m.i32(slot);
            let handle = self.s.heap_handle(c, slot, false, false, (11, 0));
            let (base_low, base_high) =
                (self.s.m.i32(base as u32), self.s.m.i32((base >> 32) as u32));
            let at = self.s.m.bin(c, Bin::Sub, low, base_low);
            let in_low = self.s.m.cmp(c, pred::IUGE, low, base_low);
            let in_high = self.s.m.cmp(c, pred::EQ, high, base_high);
            let inside = self.s.m.bin(c, Bin::And, in_low, in_high);
            let outside = self.s.m.i32(0xffff_ff00);
            let at = self.s.m.select(c, inside, at, outside);
            return (handle, at, inside);
        }
        let (handle, byte) = self.memory(low, high, guard, false);
        let mapped = self.page_there.expect("memory says");
        (handle, byte, mapped)
    }

    /// The second 64 bytes of a 128-byte node: in a page, and in a copy, of
    /// their own maybe. Goes on in another block.
    fn bvh_second_half(&mut self, at: NodeAt) -> NodeAt {
        match at {
            NodeAt::Buffer { handle, byte } => {
                let size = self.s.m.i32(64);
                let byte = self.s.m.bin(self.cur, Bin::Add, byte, size);
                NodeAt::Buffer { handle, byte }
            }
            NodeAt::Memory { low, high, .. } => {
                let (low, high) = self.address_plus(low, high, 64);
                // Every lane in this block runs the instruction.
                let (handle, byte, mapped) = self.bvh_memory(low, high, None);
                NodeAt::Memory {
                    handle,
                    byte,
                    low,
                    high,
                    mapped,
                }
            }
        }
    }

    /// `count` dwords of 64 bytes of a node from their byte `base` + `offset`
    /// (`base` computed, `offset` fixed), read four at a time.
    fn bvh_node_words(&mut self, at: NodeAt, base: Option<V>, offset: u32, count: u32) -> Vec<V> {
        let (undef, align) = (self.s.m.undef(self.s.t.i32), self.s.m.i32(4));
        let (handle, start_byte) = match at {
            NodeAt::Buffer { handle, byte } | NodeAt::Memory { handle, byte, .. } => (handle, byte),
        };
        let mut out = Vec::with_capacity(count as usize);
        for start in (0..count).step_by(4) {
            let n = (count - start).min(4);
            let node_byte = offset + 4 * start;
            let mut byte = start_byte;
            let c = self.cur;
            if let Some(b) = base {
                byte = self.s.m.bin(c, Bin::Add, byte, b);
            }
            let k = self.s.m.i32(node_byte);
            let byte = self.s.m.bin(c, Bin::Add, byte, k);
            let mask = self.s.m.i8((1u8 << n) - 1);
            let r = self.s.op(
                c,
                "rawBufferLoad.f32",
                Attr::ReadOnly,
                self.s.t.res_ret,
                139,
                &[handle, byte, undef, mask, align],
            );
            for k in 0..n {
                let f = self.s.m.extract(c, r, k);
                out.push(self.as_int(f));
            }
        }
        out
    }

    /// The node's title address, [`bvh::node_address`]: the descriptor's
    /// base (dword 0 and the low byte of dword 1, in 256-byte units) plus
    /// `(pointer >> 3) << 6`.
    fn bvh_address(&mut self, word0: V, word1: V, pointer: V, pointer_high: Option<V>) -> (V, V) {
        let c = self.cur;
        let i64t = self.s.m.int(64);
        let (zero, byte) = (self.s.m.i32(0), self.s.m.i32(0xff));
        let base_high = self.s.m.bin(c, Bin::And, word1, byte);
        let base = self.join64(word0, base_high);
        let k8 = self.s.m.int_const(i64t, 8);
        let base = self.s.m.bin(c, Bin::Shl, base, k8);
        let p = self.join64(pointer, pointer_high.unwrap_or(zero));
        let (k3, k6) = (self.s.m.int_const(i64t, 3), self.s.m.int_const(i64t, 6));
        let p = self.s.m.bin(c, Bin::LShr, p, k3);
        let p = self.s.m.bin(c, Bin::Shl, p, k6);
        let a = self.s.m.bin(c, Bin::Add, base, p);
        self.s.flags |= flags::INT64;
        self.split64(a)
    }

    /// The box test ([`bvh::boxes`]): four result dwords. Starts in the
    /// current block and leaves `self.cur` at the block it ends in.
    /// `pre`: the node's first sixteen dwords, read already.
    /// `parent`: the node's pointer (and its high dword, of the 64-bit
    /// form), for [`super::super::bvh_walk_guard`]; with it on, also the
    /// children it kept the walk from (a count).
    fn bvh_boxes(
        &mut self,
        at: NodeAt,
        kind: V,
        ray: &Ray,
        sort: bool,
        pre: Option<&[V]>,
        parent: (V, Option<V>),
    ) -> ([V; 4], Option<V>) {
        let head = self.cur;
        let children = match pre {
            Some(w) => w[..4].to_vec(),
            None => self.bvh_node_words(at, None, 0, 4),
        };
        let k5 = self.s.m.i32(5);
        let wide = self.s.m.cmp(head, pred::EQ, kind, k5);
        let (wide_block, half_block) = (self.s.m.new_block(), self.s.m.new_block());
        let join = self.s.m.new_block();
        self.s
            .m
            .terminate(head, Term::CondBr(wide, wide_block, half_block));
        // 32-bit floats: 24 dwords from byte 16, the first 12 in the node's
        // first 64 bytes. In title memory the second 12 lie in the same 64 KiB
        // page, and so in the same copy, for 1,023 of 1,024 aligned nodes: only
        // a node that starts at 0xFFC0 in its page looks its second half up
        // again (DexNom/VIctory#21).
        self.cur = wide_block;
        let k16 = self.s.m.i32(16);
        let words = match pre {
            Some(w) => w[4..].to_vec(),
            None => self.bvh_node_words(at, None, 16, 12),
        };
        let first: Vec<V> = words.iter().map(|&w| self.as_float(w)).collect();
        let floats = |l: &mut Self, at: NodeAt| -> (Vec<V>, usize) {
            let words = l.bvh_node_words(at, None, 0, 12);
            let mut all = first.clone();
            all.extend(words.iter().map(|&w| l.as_float(w)));
            // (The look for the second half may go on in another block.)
            let end = l.cur;
            l.s.m.terminate(end, Term::Br(join));
            (all, end)
        };
        // Where the wide floats come from: (floats, block, whether the second
        // half's page is there).
        let wide: Vec<(Vec<V>, usize, Option<V>)> = match at {
            NodeAt::Buffer { .. } => {
                let second = self.bvh_second_half(at);
                let (all, end) = floats(self, second);
                vec![(all, end, None)]
            }
            NodeAt::Memory { handle, byte, low, mapped, .. } => {
                let (k64k, last) = (self.s.m.i32(0xFFFF), self.s.m.i32(0xFFC0));
                let within = self.s.m.bin(self.cur, Bin::And, low, k64k);
                let crosses = self.s.m.cmp(self.cur, pred::IUGE, within, last);
                let (cross_block, same_block) = (self.s.m.new_block(), self.s.m.new_block());
                self.s.m.terminate(self.cur, Term::CondBr(crosses, cross_block, same_block));
                self.cur = same_block;
                let k64 = self.s.m.i32(64);
                let byte = self.s.m.bin(same_block, Bin::Add, byte, k64);
                let (same, same_end) = floats(self, NodeAt::Buffer { handle, byte });
                self.cur = cross_block;
                let second = self.bvh_second_half(at);
                let NodeAt::Memory { mapped: second_mapped, .. } = second else {
                    unreachable!("the second half of a node in title memory is in title memory")
                };
                let (cross, cross_end) = floats(self, second);
                vec![(same, same_end, Some(mapped)), (cross, cross_end, Some(second_mapped))]
            }
        };
        // 16-bit floats: 24 halves in 12 dwords from byte 16, the low half
        // of a dword first.
        self.cur = half_block;
        let words = match pre {
            Some(w) => w[4..].to_vec(),
            None => self.bvh_node_words(at, None, 16, 12),
        };
        let f32t = self.s.t.f32;
        let mut halves = Vec::with_capacity(24);
        for n in 0..24 {
            let mut h = words[n / 2];
            if n % 2 == 1 {
                h = self.s.m.bin(half_block, Bin::LShr, h, k16);
            }
            // The conversion reads the low 16 bits.
            halves.push(self.s.op(
                half_block,
                "legacyF16ToF32",
                Attr::ReadNone,
                f32t,
                131,
                &[h],
            ));
        }
        self.s.m.terminate(half_block, Term::Br(join));
        self.cur = join;
        // A page that is not there reads zeros, which are a child and a box:
        // no child of it is hit. (The last two boxes of a 128-byte node are in
        // its second half.)
        let there = match at {
            NodeAt::Memory { mapped: first, .. } => {
                let i1t = self.s.m.type_of(first);
                let mapped = self.s.m.phi(join, i1t);
                let mut from: Vec<(V, usize)> = wide
                    .iter()
                    .map(|(_, end, there)| (there.expect("in title memory"), *end))
                    .collect();
                from.push((first, half_block));
                self.s.m.set_phi(mapped, from);
                let both = self.s.m.bin(join, Bin::And, first, mapped);
                Some([first, first, both, both])
            }
            NodeAt::Buffer { .. } => None,
        };
        let mut coords = Vec::with_capacity(24);
        for n in 0..24 {
            let phi = self.s.m.phi(join, f32t);
            let mut from: Vec<(V, usize)> =
                wide.iter().map(|(floats, end, _)| (floats[n], *end)).collect();
            from.push((halves[n], half_block));
            self.s.m.set_phi(phi, from);
            coords.push(phi);
        }
        // The inverse direction held finite ([`bvh::boxes`]).
        let (most, least) = (
            self.s.m.f32(f32::MAX.to_bits()),
            self.s.m.f32((-f32::MAX).to_bits()),
        );
        let inverse = ray.inverse.map(|x| {
            let x = self.binary(35, x, least);
            self.binary(36, x, most)
        });
        let c = join;
        let (none, infinity, fzero) = (
            self.s.m.i32(bvh::NONE),
            self.s.m.f32(f32::INFINITY.to_bits()),
            self.s.m.f32(0),
        );
        let mut ids = [none; 4];
        let mut distances = [infinity; 4];
        // The guard ([`super::super::bvh_walk_guard`]): a child at node 0, at
        // the box itself or (2) before it is not entered. Pointers compared
        // without their kind bits; in the 64-bit form only while the box's
        // pointer has no high bits (children are 32-bit).
        let level = super::super::bvh_walk_guard();
        let walk_guard = (level > 0).then(|| {
            let not_kind = self.s.m.i32(!7);
            let here = self.s.m.bin(c, Bin::And, parent.0, not_kind);
            let low = match parent.1 {
                Some(high) => {
                    let z = self.s.m.i32(0);
                    self.s.m.cmp(c, pred::EQ, high, z)
                }
                None => self.s.m.bool(true),
            };
            (not_kind, here, low)
        });
        let mut cuts = walk_guard.map(|_| self.s.m.i32(0));
        for n in 0..4 {
            let bx = &coords[6 * n..6 * n + 6];
            let (mut near, mut far) = ([fzero; 3], [fzero; 3]);
            for a in 0..3 {
                let low = self.s.m.bin(c, Bin::Sub, bx[a], ray.origin[a]);
                let low = self.s.m.bin(c, Bin::Mul, low, inverse[a]);
                let high = self.s.m.bin(c, Bin::Sub, bx[3 + a], ray.origin[a]);
                let high = self.s.m.bin(c, Bin::Mul, high, inverse[a]);
                near[a] = self.binary(36, low, high);
                far[a] = self.binary(35, low, high);
            }
            let enter = self.binary(35, near[0], near[1]);
            let enter = self.binary(35, enter, near[2]);
            let leave = self.binary(36, far[0], far[1]);
            let leave = self.binary(36, leave, far[2]);
            let from = self.binary(35, enter, fzero);
            // Ordered compares: a NaN anywhere is a miss.
            let through = self.s.m.cmp(c, pred::OGE, leave, from);
            let before = self.s.m.cmp(c, pred::OLT, enter, ray.extent);
            let off = self.is_nan_f32(bx[0]);
            let on = self.s.m.bool(true);
            let on = self.s.m.bin(c, Bin::Xor, off, on);
            let hit = self.s.m.bin(c, Bin::And, through, before);
            let hit = self.s.m.bin(c, Bin::And, hit, on);
            let child = self.s.m.cmp(c, pred::NE, children[n], none);
            let hit = self.s.m.bin(c, Bin::And, hit, child);
            let hit = match there {
                Some(there) => self.s.m.bin(c, Bin::And, hit, there[n]),
                None => hit,
            };
            let hit = match walk_guard {
                Some((not_kind, here, low)) => {
                    let to = self.s.m.bin(c, Bin::And, children[n], not_kind);
                    let z = self.s.m.i32(0);
                    let at_zero = self.s.m.cmp(c, pred::EQ, to, z);
                    let back = self.s.m.cmp(c, if level >= 2 { pred::IULE } else { pred::EQ }, to, here);
                    let back = self.s.m.bin(c, Bin::And, back, low);
                    let bad = self.s.m.bin(c, Bin::Or, at_zero, back);
                    let cut = self.s.m.bin(c, Bin::And, hit, bad);
                    let one = self.s.m.i32(1);
                    let add = self.s.m.select(c, cut, one, z);
                    cuts = cuts.map(|sum| self.s.m.bin(c, Bin::Add, sum, add));
                    let yes = self.s.m.bool(true);
                    let good = self.s.m.bin(c, Bin::Xor, bad, yes);
                    self.s.m.bin(c, Bin::And, hit, good)
                }
                None => hit,
            };
            ids[n] = self.s.m.select(c, hit, children[n], none);
            distances[n] = self.s.m.select(c, hit, enter, infinity);
        }
        if sort {
            for (a, b) in [(0, 1), (2, 3), (0, 2), (1, 3), (1, 2)] {
                let swap = self.s.m.cmp(c, pred::OLT, distances[b], distances[a]);
                let (da, db) = (distances[a], distances[b]);
                distances[a] = self.s.m.select(c, swap, db, da);
                distances[b] = self.s.m.select(c, swap, da, db);
                let (ia, ib) = (ids[a], ids[b]);
                ids[a] = self.s.m.select(c, swap, ib, ia);
                ids[b] = self.s.m.select(c, swap, ia, ib);
            }
        }
        (ids, cuts)
    }

    /// Adds `n` to dword `dword` of the faults buffer where `when`
    /// (counts of [`super::super::bvh_walk_guard`]); leaves `self.cur` at
    /// the block after.
    pub(super) fn count_walk(&mut self, dword: u32, n: V, when: V) {
        let gate = self.begin_guarded(Some(when));
        let c = self.cur;
        let h = self.s.root_uav_handle(super::super::SLOT_FAULTS);
        let i32t = self.s.t.i32;
        let iu = self.s.m.undef(i32t);
        let (add, byte) = (self.s.m.i32(0), self.s.m.i32(4 * dword));
        self.s.op(c, "atomicBinOp.i32", Attr::NoUnwind, i32t, 78, &[h, add, byte, iu, iu, n]);
        self.end_guarded(gate, &[]);
    }

    /// The triangle test ([`bvh::triangle`] of [`bvh::triangle_vertices`]):
    /// four result dwords, made in the current block.
    /// `pre`: the node's first sixteen dwords, read already.
    fn bvh_triangle(&mut self, at: NodeAt, kind: V, ray: &Ray, pre: Option<&[V]>) -> [V; 4] {
        let c = self.cur;
        let k = |l: &mut Self, v: u32| l.s.m.i32(v);
        // The triangle's byte of the dword at byte 60: which of its
        // vertices barycentric I and J weigh.
        let id = match pre {
            Some(w) => w[15],
            None => self.bvh_node_words(at, None, 60, 1)[0],
        };
        let (k2, k3, k12, k15) = (k(self, 2), k(self, 3), k(self, 12), k(self, 15));
        let shift = self.s.m.bin(c, Bin::Shl, kind, k3);
        let id = self.s.m.bin(c, Bin::LShr, id, shift);
        let i = self.s.m.bin(c, Bin::And, id, k3);
        let j = self.s.m.bin(c, Bin::LShr, id, k2);
        let j = self.s.m.bin(c, Bin::And, j, k3);
        let first = self.s.m.bin(c, Bin::Sub, k3, i);
        let first = self.s.m.bin(c, Bin::Sub, first, j);
        let first = self.s.m.bin(c, Bin::And, first, k3);
        // The node's vertices the triangle is made of, four bits each.
        let (k0, k1) = (k(self, 0), k(self, 1));
        let maps: [V; 4] = std::array::from_fn(|n| k(self, bvh::vertex_map(n as u32)));
        let (is0, is1, is2) = (
            self.s.m.cmp(c, pred::EQ, kind, k0),
            self.s.m.cmp(c, pred::EQ, kind, k1),
            self.s.m.cmp(c, pred::EQ, kind, k2),
        );
        let map = self.s.m.select(c, is2, maps[2], maps[3]);
        let map = self.s.m.select(c, is1, maps[1], map);
        let map = self.s.m.select(c, is0, maps[0], map);
        let mut p = Vec::with_capacity(3);
        for n in [first, i, j] {
            let nibble = self.s.m.bin(c, Bin::Shl, n, k2);
            let vertex = self.s.m.bin(c, Bin::LShr, map, nibble);
            let vertex = self.s.m.bin(c, Bin::And, vertex, k15);
            let words = match pre {
                // One of the node's five vertices, dwords 3v to 3v + 2: the
                // vertex numbers the maps hold are 0-4.
                Some(w) => (0..3)
                    .map(|a| {
                        let mut x = w[12 + a];
                        for v in (0..4).rev() {
                            let kv = k(self, v as u32);
                            let is = self.s.m.cmp(c, pred::EQ, vertex, kv);
                            x = self.s.m.select(c, is, w[3 * v + a], x);
                        }
                        x
                    })
                    .collect(),
                None => {
                    let byte = self.s.m.bin(c, Bin::Mul, vertex, k12);
                    self.bvh_node_words(at, Some(byte), 0, 3)
                }
            };
            // Relative to the ray's origin.
            let r: Vec<V> = (0..3)
                .map(|a| {
                    let f = self.as_float(words[a]);
                    self.s.m.bin(c, Bin::Sub, f, ray.origin[a])
                })
                .collect();
            p.push([r[0], r[1], r[2]]);
        }
        // The ray's largest axis z, and x and y after it in an order that
        // keeps the winding.
        let fzero = self.s.m.f32(0);
        let size = ray.direction.map(|d| self.unary(6, d));
        let xy = self.s.m.cmp(c, pred::OGE, size[0], size[1]);
        let xz = self.s.m.cmp(c, pred::OGE, size[0], size[2]);
        let yz = self.s.m.cmp(c, pred::OGE, size[1], size[2]);
        let z0 = self.s.m.bin(c, Bin::And, xy, xz);
        let on = self.s.m.bool(true);
        let not_xy = self.s.m.bin(c, Bin::Xor, xy, on);
        let z1 = self.s.m.bin(c, Bin::And, not_xy, yz);
        // Component (kz + n) % 3 of `q`.
        let pick = |l: &mut Self, q: [V; 3], n: usize| {
            let rest = l.s.m.select(c, z1, q[(1 + n) % 3], q[(2 + n) % 3]);
            l.s.m.select(c, z0, q[n % 3], rest)
        };
        let dz = pick(self, ray.direction, 0);
        let backwards = self.s.m.cmp(c, pred::OLT, dz, fzero);
        // (x, y, z) of `q`: the two after the largest axis, swapped for a
        // ray that runs backwards along it.
        let axes = |l: &mut Self, q: [V; 3]| {
            let (x, y) = (pick(l, q, 1), pick(l, q, 2));
            [
                l.s.m.select(c, backwards, y, x),
                l.s.m.select(c, backwards, x, y),
                pick(l, q, 0),
            ]
        };
        let d = axes(self, ray.direction);
        let sz = pick(self, ray.inverse, 0);
        let sx = self.s.m.bin(c, Bin::Mul, d[0], sz);
        let sy = self.s.m.bin(c, Bin::Mul, d[1], sz);
        let mut sheared = Vec::with_capacity(3);
        for r in p {
            let r = axes(self, r);
            let x = self.s.m.bin(c, Bin::Mul, sx, r[2]);
            let x = self.s.m.bin(c, Bin::Sub, r[0], x);
            let y = self.s.m.bin(c, Bin::Mul, sy, r[2]);
            let y = self.s.m.bin(c, Bin::Sub, r[1], y);
            let z = self.s.m.bin(c, Bin::Mul, sz, r[2]);
            sheared.push((x, y, z));
        }
        let [(ax, ay, az), (bx, by, bz), (cx, cy, cz)] = sheared[..] else {
            unreachable!()
        };
        // p * q - r * s.
        let cross = |l: &mut Self, p: V, q: V, r: V, s: V| {
            let a = l.s.m.bin(c, Bin::Mul, p, q);
            let b = l.s.m.bin(c, Bin::Mul, r, s);
            l.s.m.bin(c, Bin::Sub, a, b)
        };
        let u = cross(self, cx, by, cy, bx);
        let v = cross(self, ax, cy, ay, cx);
        let w = cross(self, bx, ay, by, ax);
        let any = |l: &mut Self, p: u8| {
            let a = l.s.m.cmp(c, p, u, fzero);
            let b = l.s.m.cmp(c, p, v, fzero);
            let d = l.s.m.cmp(c, p, w, fzero);
            let a = l.s.m.bin(c, Bin::Or, a, b);
            l.s.m.bin(c, Bin::Or, a, d)
        };
        let behind = any(self, pred::OLT);
        let before = any(self, pred::OGT);
        let outside = self.s.m.bin(c, Bin::And, behind, before);
        let inside = self.s.m.bin(c, Bin::Xor, outside, on);
        let det = self.s.m.bin(c, Bin::Add, v, w);
        let det = self.s.m.bin(c, Bin::Add, u, det);
        let t = self.s.m.bin(c, Bin::Mul, u, az);
        let t2 = self.s.m.bin(c, Bin::Mul, v, bz);
        let t = self.s.m.bin(c, Bin::Add, t, t2);
        let t2 = self.s.m.bin(c, Bin::Mul, w, cz);
        let t = self.s.m.bin(c, Bin::Add, t, t2);
        // The distance is not negative: the numerator has the
        // denominator's sign (or is 0). Ordered compares: a denominator
        // of 0 or a NaN is a miss.
        let front = self.s.m.cmp(c, pred::OGT, det, fzero);
        let front_ahead = self.s.m.cmp(c, pred::OGE, t, fzero);
        let front = self.s.m.bin(c, Bin::And, front, front_ahead);
        let back = self.s.m.cmp(c, pred::OLT, det, fzero);
        let back_ahead = self.s.m.cmp(c, pred::OLE, t, fzero);
        let back = self.s.m.bin(c, Bin::And, back, back_ahead);
        let ahead = self.s.m.bin(c, Bin::Or, front, back);
        let hit = self.s.m.bin(c, Bin::And, inside, ahead);
        let miss = bvh::MISS.map(|f| self.s.m.f32(f.to_bits()));
        let out = [t, det, v, w];
        std::array::from_fn(|n| {
            let f = self.s.m.select(c, hit, out[n], miss[n]);
            self.as_int(f)
        })
    }
}
