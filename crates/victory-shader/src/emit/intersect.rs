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

use super::{Attr, Bin, Fields, Inst, Lower, Operand, Reg, RootType, Tag, Term, V, Why, flags, pred};
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
}

/// Where the instruction's node is.
#[derive(Clone, Copy)]
enum NodeAt {
    /// At this byte of a raw buffer of the host.
    Buffer { handle: V, byte: V },
    /// In title memory: the raw view of the host copy holding the node's
    /// first 64 bytes and the node's byte in it ([`Lower::bvh_memory`]),
    /// and the node's address, for the second half of a 128-byte node.
    Memory {
        handle: V,
        byte: V,
        low: V,
        high: V,
        mapped: V,
    },
}

/// The ray operands, as floats.
struct Ray {
    extent: V,
    origin: [V; 3],
    direction: [V; 3],
    inverse: [V; 3],
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
        let gate = self.begin_guarded(guard);
        let probe = crate::emit::RAY_PROBE.get();
        if probe {
            self.ray_probe_increment(0, None);
        }
        let head = self.cur;
        let (k4, k6, k7) = (self.s.m.i32(4), self.s.m.i32(6), self.s.m.i32(7));
        let kind = self.s.m.bin(head, Bin::And, pointer, k7);
        let pair = self.s.m.bin(head, Bin::And, pointer, k6);
        let is_box = self.s.m.cmp(head, pred::EQ, pair, k4);
        let is_triangle = self.s.m.cmp(head, pred::IULT, kind, k4);
        let (box_block, leaf_block) = (self.s.m.new_block(), self.s.m.new_block());
        let (triangle_block, join) = (self.s.m.new_block(), self.s.m.new_block());
        self.s
            .m
            .terminate(head, Term::CondBr(is_box, box_block, leaf_block));
        self.s
            .m
            .terminate(leaf_block, Term::CondBr(is_triangle, triangle_block, join));
        self.cur = box_block;
        let boxes = self.bvh_boxes(at, kind, &ray, sort);
        let box_end = self.cur;
        self.s.m.terminate(box_end, Term::Br(join));
        self.cur = triangle_block;
        let triangle = self.bvh_triangle(at, kind, &ray);
        if probe {
            self.ray_probe_triangle(at, [pointer, pointer_high.unwrap_or(zero)], &ray, triangle);
        }
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
        let results = self.end_guarded(gate, &results);
        for (k, &r) in results.iter().enumerate() {
            self.write(inst, Reg::V(dst + k as u8), r, guard);
        }
        Ok(())
    }

    /// One atomic add per active wave. The first wave to report a hit
    /// elects its first active lane to record the six sample dwords.
    fn ray_probe_increment(&mut self, word: u32, sample: Option<[V; 6]>) {
        let yes = self.s.m.bool(true);
        let count = self.s.op(
            self.cur,
            "waveAllOp",
            Attr::NoUnwind,
            self.s.t.i32,
            135,
            &[yes],
        );
        let first = self.s.op(
            self.cur,
            "waveIsFirstLane",
            Attr::NoUnwind,
            self.s.t.i1,
            110,
            &[],
        );
        self.s.flags |= flags::WAVE_OPS;
        let lane = self.begin_guarded(Some(first));
        let storage = self.s.root_uav_handle(crate::emit::SLOT_FAULTS);
        let byte = self.s.m.i32(4 * (crate::emit::MEMORY_FAULTS + word));
        let add = self.s.m.i32(0);
        let undef = self.s.m.undef(self.s.t.i32);
        let previous = self.s.op(
            self.cur,
            "atomicBinOp.i32",
            Attr::NoUnwind,
            self.s.t.i32,
            78,
            &[storage, add, byte, undef, undef, count],
        );
        if let Some(fields) = sample {
            let zero = self.s.m.i32(0);
            let first = self.s.m.cmp(self.cur, pred::EQ, previous, zero);
            let elected = self.begin_guarded(Some(first));
            let mask = self.s.m.i8(1);
            let align = self.s.m.i32(4);
            for (i, field) in fields.into_iter().enumerate() {
                let byte = self
                    .s
                    .m
                    .i32(4 * (crate::emit::MEMORY_FAULTS + 2 + i as u32));
                self.s.op(
                    self.cur,
                    "rawBufferStore.i32",
                    Attr::NoUnwind,
                    self.s.t.void,
                    140,
                    &[
                        storage, byte, undef, field, undef, undef, undef, mask, align,
                    ],
                );
            }
            self.end_guarded(elected, &[]);
        }
        self.end_guarded(lane, &[]);
    }

    /// A geometric candidate exists before the guest chooses its closest
    /// hit or applies material/alpha tests. Only one lane writes the sample.
    fn ray_probe_triangle(&mut self, at: NodeAt, pointer: [V; 2], ray: &Ray, result: [V; 4]) {
        let c = self.cur;
        let numerator = self.as_float(result[0]);
        let denominator = self.as_float(result[1]);
        let distance = self.s.m.bin(c, Bin::SDiv, numerator, denominator);
        let zero = self.s.m.f32(0);
        let infinity = self.s.m.f32(f32::INFINITY.to_bits());
        let forward = self.s.m.cmp(c, pred::OGE, distance, zero);
        let finite = self.s.m.cmp(c, pred::OLT, distance, infinity);
        let within = self.s.m.cmp(c, pred::OLE, distance, ray.extent);
        let valid = self.s.m.bin(c, Bin::And, forward, finite);
        let mut valid = self.s.m.bin(c, Bin::And, valid, within);
        if let NodeAt::Memory { mapped, .. } = at {
            valid = self.s.m.bin(c, Bin::And, valid, mapped);
        }
        let hit = self.begin_guarded(Some(valid));
        let fields = [
            pointer[0],
            pointer[1],
            self.as_int(distance),
            self.as_int(ray.extent),
            result[2],
            result[3],
        ];
        self.ray_probe_increment(1, Some(fields));
        self.end_guarded(hit, &[]);
    }

    /// Title memory at `lo`/`hi`: the raw view of the host copy holding the
    /// page and the byte offset in it. A page without a copy reads zeros
    /// and drops writes (the null view at `base`), and its 64 KiB page
    /// number is left for the host, for lanes that run (`guard`).
    pub(super) fn memory(&mut self, lo: V, hi: V, guard: Option<V>) -> (V, V, V) {
        use crate::emit::shader::{
            MEMORY_FAULTS, MEMORY_PAGES, SLOT_FAULTS, SLOT_MEMORY, USER_MEMORY_BASE,
        };
        let c = self.cur;
        let (i32t, res) = (self.s.t.i32, self.s.t.res_ret);
        let k = |l: &mut Self, v: u32| l.s.m.i32(v);
        let (k16, k8) = (k(self, 16), k(self, 0xFF));
        let low = self.s.m.bin(c, Bin::LShr, lo, k16);
        let high = self.s.m.bin(c, Bin::And, hi, k8);
        let high = self.s.m.bin(c, Bin::Shl, high, k16);
        let page = self.s.m.bin(c, Bin::Or, low, high);
        // Inside the directory whatever the address (lanes that do not run
        // compute one too); its last entry is never filled.
        let last = k(self, MEMORY_PAGES - 1);
        let page = self
            .s
            .op(c, "binary.i32", Attr::ReadNone, i32t, 40, &[page, last]);
        self.s.m.tag(page, Tag::Clamp);
        let k3 = k(self, 3);
        let at = self.s.m.bin(c, Bin::Shl, page, k3);
        let directory = self.s.root_srv_handle(SLOT_MEMORY);
        let (iu, both, align) = (self.s.m.undef(i32t), self.s.m.i8(3), k(self, 4));
        let entry = self.s.op(
            c,
            "rawBufferLoad.f32",
            Attr::ReadOnly,
            res,
            139,
            &[directory, at, iu, both, align],
        );
        let (copy, start) = (self.s.m.extract(c, entry, 0), self.s.m.extract(c, entry, 1));
        let (copy, start) = (self.as_int(copy), self.as_int(start));
        // A page without a copy: its number, at a place by the number, only
        // when an active lane misses; mapped accesses skip the modulo and store.
        let zero = k(self, 0);
        // A newer command list can publish more pages after this one has
        // recorded. Only reference descriptors prepared for this particular bind.
        let count = self.host_word(RootType::Other(USER_MEMORY_BASE), 0, 1);
        let in_range = self.s.m.cmp(c, pred::IULE, copy, count);
        let copy = self.s.m.select(c, in_range, copy, zero);
        let mut missing = self.s.m.cmp(c, pred::EQ, copy, zero);
        if let Some(g) = guard {
            missing = self.s.m.bin(c, Bin::And, missing, g);
        }
        let fault_gate = self.begin_guarded(Some(missing));
        let fc = self.cur;
        let places = k(self, MEMORY_FAULTS - 1);
        let place = self.s.m.bin(fc, Bin::URem, page, places);
        let one = k(self, 1);
        let place = self.s.m.bin(fc, Bin::Add, place, one);
        let k2 = k(self, 2);
        let place = self.s.m.bin(fc, Bin::Shl, place, k2);
        let faults = self.s.root_uav_handle(SLOT_FAULTS);
        let void = self.s.t.void;
        let mask = self.s.m.i8(1);
        self.s.op(
            fc,
            "rawBufferStore.i32",
            Attr::NoUnwind,
            void,
            140,
            &[faults, place, iu, page, iu, iu, iu, mask, align],
        );
        self.end_guarded(fault_gate, &[]);
        let c = self.cur;
        let base = self.host_word(RootType::Other(USER_MEMORY_BASE), 0, 0);
        let slot = self.s.m.bin(c, Bin::Add, base, copy);
        let h = self.s.heap_handle(c, slot, false, true, (11 | 0x1000, 0));
        let k64k = k(self, 0xFFFF);
        let within = self.s.m.bin(c, Bin::And, lo, k64k);
        let byte = self.s.m.bin(c, Bin::Add, start, within);
        let mapped = self.s.m.cmp(c, pred::NE, copy, zero);
        (h, byte, mapped)
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
        self.memory(low, high, guard)
    }

    /// Reads within a single aligned 64-byte part of the node, in groups
    /// of up to four dwords.
    fn bvh_node_words(&mut self, at: NodeAt, base: Option<V>, offset: u32, count: u32) -> Vec<V> {
        let c = self.cur;
        let (undef, align) = (self.s.m.undef(self.s.t.i32), self.s.m.i32(4));
        let (handle, start_byte) = match at {
            NodeAt::Buffer { handle, byte } | NodeAt::Memory { handle, byte, .. } => (handle, byte),
        };
        let mut out = Vec::with_capacity(count as usize);
        for start in (0..count).step_by(4) {
            let n = (count - start).min(4);
            let node_byte = offset + 4 * start;
            let mut byte = start_byte;
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
    fn bvh_boxes(&mut self, at: NodeAt, kind: V, ray: &Ray, sort: bool) -> [V; 4] {
        let head = self.cur;
        let children = self.bvh_node_words(at, None, 0, 4);
        let k5 = self.s.m.i32(5);
        let wide = self.s.m.cmp(head, pred::EQ, kind, k5);
        let (wide_block, half_block) = (self.s.m.new_block(), self.s.m.new_block());
        let join = self.s.m.new_block();
        self.s
            .m
            .terminate(head, Term::CondBr(wide, wide_block, half_block));
        // 32-bit floats: 24 dwords from byte 16.
        // The first 12 dwords sit in the first 64-byte half (already resolved).
        // The second 12 dwords lie in the same 64 KiB page for 1,023 of 1,024
        // 64-byte-aligned addresses; only nodes starting at offset >= 0xFFC0
        // cross into the next page and need a second directory lookup.
        self.cur = wide_block;
        let k16 = self.s.m.i32(16);
        let words0 = self.bvh_node_words(at, None, 16, 12);
        let floats0: Vec<V> = words0.iter().map(|&w| self.as_float(w)).collect();
        enum WideJoin {
            Buffer {
                end: usize,
                floats1: Vec<V>,
            },
            Memory {
                first_mapped: V,
                same_end: usize,
                same_floats: Vec<V>,
                cross_end: usize,
                cross_floats: Vec<V>,
                cross_mapped: V,
            },
        }
        let wide_join = match at {
            NodeAt::Buffer { handle, byte } => {
                let size = self.s.m.i32(64);
                let byte64 = self.s.m.bin(wide_block, Bin::Add, byte, size);
                let words1 = self.bvh_node_words(
                    NodeAt::Buffer {
                        handle,
                        byte: byte64,
                    },
                    None,
                    0,
                    12,
                );
                let floats1: Vec<V> = words1.iter().map(|&w| self.as_float(w)).collect();
                let end = self.cur;
                self.s.m.terminate(end, Term::Br(join));
                WideJoin::Buffer { end, floats1 }
            }
            NodeAt::Memory {
                handle,
                byte,
                low,
                high,
                mapped: first_mapped,
            } => {
                let k64k = self.s.m.i32(0xFFFF);
                let boundary = self.s.m.i32(0xFFC0);
                let within = self.s.m.bin(wide_block, Bin::And, low, k64k);
                let crosses = self.s.m.cmp(wide_block, pred::IUGE, within, boundary);
                let (cross_block, same_block) = (self.s.m.new_block(), self.s.m.new_block());
                self.s
                    .m
                    .terminate(wide_block, Term::CondBr(crosses, cross_block, same_block));
                // Fast path (99.9%): second 64 bytes stay in the same 64 KiB page.
                self.cur = same_block;
                let size = self.s.m.i32(64);
                let byte64 = self.s.m.bin(same_block, Bin::Add, byte, size);
                let same_words = self.bvh_node_words(
                    NodeAt::Buffer {
                        handle,
                        byte: byte64,
                    },
                    None,
                    0,
                    12,
                );
                let same_floats: Vec<V> = same_words.iter().map(|&w| self.as_float(w)).collect();
                let same_end = self.cur;
                self.s.m.terminate(same_end, Term::Br(join));
                // Slow path (0.1%): second 64 bytes cross a 64 KiB page boundary.
                self.cur = cross_block;
                let (nlow, nhigh) = self.address_plus(low, high, 64);
                let (shandle, sbyte, cross_mapped) = self.bvh_memory(nlow, nhigh, None);
                let cross_words = self.bvh_node_words(
                    NodeAt::Buffer {
                        handle: shandle,
                        byte: sbyte,
                    },
                    None,
                    0,
                    12,
                );
                let cross_floats: Vec<V> = cross_words.iter().map(|&w| self.as_float(w)).collect();
                let cross_end = self.cur;
                self.s.m.terminate(cross_end, Term::Br(join));
                WideJoin::Memory {
                    first_mapped,
                    same_end,
                    same_floats,
                    cross_end,
                    cross_floats,
                    cross_mapped,
                }
            }
        };
        // 16-bit floats: 24 halves in 12 dwords from byte 16, the low half
        // of a dword first.
        self.cur = half_block;
        let words = self.bvh_node_words(at, None, 16, 12);
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
        let half_end = self.cur;
        self.s.m.terminate(half_end, Term::Br(join));
        self.cur = join;
        // All child IDs are in the first part. The last two Box32 bounds
        // also require the second. A zero returned by a missing page is a
        // valid child pointer or coordinate, so the slab test alone cannot
        // distinguish it from real geometry.
        let residency = match wide_join {
            WideJoin::Memory {
                first_mapped,
                same_end,
                cross_end,
                cross_mapped,
                ..
            } => {
                let mapped = self.s.m.phi(join, self.s.m.type_of(first_mapped));
                self.s.m.set_phi(
                    mapped,
                    vec![
                        (first_mapped, same_end),
                        (cross_mapped, cross_end),
                        (first_mapped, half_end),
                    ],
                );
                let both = self.s.m.bin(join, Bin::And, first_mapped, mapped);
                Some([first_mapped, first_mapped, both, both])
            }
            WideJoin::Buffer { .. } => None,
        };
        let mut coords = Vec::with_capacity(24);
        for n in 0..24 {
            let phi = self.s.m.phi(join, f32t);
            let incoming = match &wide_join {
                WideJoin::Buffer { end, floats1 } => {
                    let w = if n < 12 { floats0[n] } else { floats1[n - 12] };
                    vec![(w, *end), (halves[n], half_end)]
                }
                WideJoin::Memory {
                    same_end,
                    same_floats,
                    cross_end,
                    cross_floats,
                    ..
                } => {
                    let (sw, cw) = if n < 12 {
                        (floats0[n], floats0[n])
                    } else {
                        (same_floats[n - 12], cross_floats[n - 12])
                    };
                    vec![(sw, *same_end), (cw, *cross_end), (halves[n], half_end)]
                }
            };
            self.s.m.set_phi(phi, incoming);
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
            let child_present = self.s.m.cmp(c, pred::NE, children[n], none);
            let hit = self.s.m.bin(c, Bin::And, hit, child_present);
            let hit = match residency {
                Some(pages) => self.s.m.bin(c, Bin::And, hit, pages[n]),
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
        ids
    }

    /// The triangle test ([`bvh::triangle`] of [`bvh::triangle_vertices`]):
    /// four result dwords, made in the current block.
    fn bvh_triangle(&mut self, at: NodeAt, kind: V, ray: &Ray) -> [V; 4] {
        let c = self.cur;
        let k = |l: &mut Self, v: u32| l.s.m.i32(v);
        // The triangle's byte of the dword at byte 60: which of its
        // vertices barycentric I and J weigh.
        let id = self.bvh_node_words(at, None, 60, 1)[0];
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
            let byte = self.s.m.bin(c, Bin::Mul, vertex, k12);
            let words = self.bvh_node_words(at, Some(byte), 0, 3);
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
