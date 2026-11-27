//! Loops whose iteration count the code itself fixes (after
//! DexNom/VIctory#137).
//!
//! The translator cuts every loop short after a per-stage number of
//! iterations ([`super::lower::LOOP_LIMIT`] and the stage limits in
//! `count_iteration`). That guards against loops that would run on data the
//! host has not mirrored yet, but it also cuts genuine loops: a compute
//! loop of 8,192 iterations stopped at 4,096 and stored wrong results.
//!
//! A loop is *counted* when it is driven by one scalar register that
//! - enters the loop as a constant (`s_mov_b32` or `s_movk_i32` of an inline
//!   constant or literal) on every edge into it,
//! - is stepped once per iteration by a constant (`s_add`/`s_sub`
//!   `_u32`/`_i32` or `s_addk_i32` of the header's own value), and
//! - decides the single back edge: the latch's `s_cbranch_scc0/1` reads the
//!   SCC of an `s_cmp(k)_*` of the stepped value against a constant.
//!
//! Nothing else feeds it, so every lane and every run takes the back edge
//! the same number of times, whatever any memory holds. [`trips`] works
//! that number out by running the counter, and gives up past [`MAX_TRIPS`].
//!
//! The latch's branch must also be the loop's only way out (no `break`, no
//! `s_endpgm` inside). A loop with another exit could be meant to stop early
//! on its data. When that data hasn't been mirrored yet, it would run its
//! whole count. That is the runaway case the stage limits are for, so such
//! loops keep them.

use std::collections::{HashMap, HashSet};

use crate::gfx10::{Fields, Operand};
use crate::ir::{Exit, Program, Reg, Value};

/// The longest counted loop given its own limit: the translator's own
/// overall cap, so no loop runs longer than the default allows any loop.
pub(super) const MAX_TRIPS: u32 = super::lower::LOOP_LIMIT;

/// The value of an inline integer constant or a literal.
fn constant(op: &Operand) -> Option<u32> {
    match *op {
        Operand::Const(c @ 128..=192) => Some(u32::from(c) - 128),
        Operand::Const(c @ 193..=208) => Some((-(i32::from(c) - 192)) as u32),
        Operand::Literal(x) => Some(x),
        _ => None,
    }
}

/// How the counter moves once per iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Add(u32),
    Sub(u32),
}

/// `s_cmp(k)_<cc>_<ty>` with the counter on the left (`swap`: on the right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Compare {
    cc: &'static str,
    signed: bool,
    other: u32,
    swap: bool,
}

impl Compare {
    fn holds(&self, x: u32) -> bool {
        let (a, b) = if self.swap {
            (self.other, x)
        } else {
            (x, self.other)
        };
        let o = if self.signed {
            (a as i32).cmp(&(b as i32))
        } else {
            a.cmp(&b)
        };
        use std::cmp::Ordering::*;
        match self.cc {
            "eq" => o == Equal,
            "lg" => o != Equal,
            "gt" => o == Greater,
            "ge" => o != Less,
            "lt" => o == Less,
            "le" => o != Greater,
            _ => false,
        }
    }
}

/// Back edges taken per entry, for every loop header `trips` can bound.
/// `rpo` is each block's reverse-postorder index (a pred at or after its
/// header is a back edge, as in `lower`).
pub(super) fn trips(p: &Program, rpo: &[usize]) -> HashMap<usize, u32> {
    let mut def: HashMap<Value, (usize, usize)> = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            for &(_, v) in &inst.writes {
                def.insert(v, (b, i));
            }
        }
    }
    let mut out = HashMap::new();
    for h in 0..p.blocks.len() {
        if rpo[h] == usize::MAX {
            continue;
        }
        if let Some(n) = header_trips(p, rpo, &def, h) {
            out.insert(h, n);
        }
    }
    out
}

fn header_trips(
    p: &Program,
    rpo: &[usize],
    def: &HashMap<Value, (usize, usize)>,
    h: usize,
) -> Option<u32> {
    let block = &p.blocks[h];
    let reachable = |q: usize| rpo[q] != usize::MAX;
    let back: Vec<usize> = (0..block.preds.len())
        .filter(|&k| reachable(block.preds[k]) && rpo[h] <= rpo[block.preds[k]])
        .collect();
    let [k_back] = back[..] else { return None };
    let latch = block.preds[k_back];
    // The latch's branch on SCC, one way back to the header.
    let Exit::Branch { taken, not_taken } = p.blocks[latch].exit else {
        return None;
    };
    if (taken == h) == (not_taken == h) {
        return None;
    }
    let branch = p.blocks[latch].insts.last()?;
    let on_set = match branch.name() {
        "s_cbranch_scc1" => true,
        "s_cbranch_scc0" => false,
        _ => return None,
    };
    let scc = branch.read(Reg::Scc)?;
    if !single_exit(p, h, latch) {
        return None;
    }
    // Continue (go back) when SCC is `again`.
    let again = on_set == (taken == h);
    for phi in &block.phis {
        let Reg::S(r) = phi.reg else { continue };
        let Some(n) = counted(
            p,
            def,
            phi.value,
            r,
            &phi.args,
            k_back,
            &block.preds,
            rpo,
            h,
            scc,
            again,
        ) else {
            continue;
        };
        return Some(n);
    }
    None
}

/// Whether the natural loop of `h` and `latch` leaves only from the latch.
fn single_exit(p: &Program, h: usize, latch: usize) -> bool {
    let mut body = HashSet::from([h, latch]);
    let mut work = vec![latch];
    while let Some(b) = work.pop() {
        if b == h {
            continue;
        }
        for &q in &p.blocks[b].preds {
            if body.insert(q) {
                work.push(q);
            }
        }
    }
    body.iter().all(|&b| match p.blocks[b].exit {
        Exit::End | Exit::Indirect => false,
        e => e
            .successors()
            .iter()
            .all(|s| body.contains(s) || b == latch),
    })
}

#[allow(clippy::too_many_arguments)]
fn counted(
    p: &Program,
    def: &HashMap<Value, (usize, usize)>,
    head: Value,
    r: u8,
    args: &[Value],
    k_back: usize,
    preds: &[usize],
    rpo: &[usize],
    h: usize,
    scc: Value,
    again: bool,
) -> Option<u32> {
    let inst = |v: Value| def.get(&v).map(|&(b, i)| &p.blocks[b].insts[i]);
    // The step: the back edge's value, made from the header's own value.
    let next = args[k_back];
    let step_inst = inst(next)?;
    let reads_head = |op: &Operand| {
        matches!(*op, Operand::Scalar { code, dwords: 1 } if code == r)
            && step_inst.read(Reg::S(r)) == Some(head)
    };
    let step = match (&step_inst.fields, step_inst.name()) {
        (Fields::Sop { src, .. }, n @ ("s_add_u32" | "s_add_i32" | "s_sub_u32" | "s_sub_i32"))
            if src.len() == 2 =>
        {
            let add = n.starts_with("s_add");
            if reads_head(&src[0]) {
                let c = constant(&src[1])?;
                if add { Step::Add(c) } else { Step::Sub(c) }
            } else if add && reads_head(&src[1]) {
                Step::Add(constant(&src[0])?)
            } else {
                return None;
            }
        }
        (
            Fields::Sop {
                dst: Some(Operand::Scalar { code, dwords: 1 }),
                imm: Some(imm),
                ..
            },
            "s_addk_i32",
        ) if *code == r && step_inst.read(Reg::S(r)) == Some(head) => {
            Step::Add(*imm as i16 as i32 as u32)
        }
        _ => return None,
    };
    if step_inst.write(Reg::S(r)) != Some(next) {
        return None;
    }
    // The compare that sets the latch's SCC, of the stepped value.
    let cmp_inst = inst(scc)?;
    if cmp_inst.write(Reg::Scc) != Some(scc) {
        return None;
    }
    let name = cmp_inst.name();
    let (kind, rest) = if let Some(rest) = name.strip_prefix("s_cmpk_") {
        (true, rest)
    } else {
        (false, name.strip_prefix("s_cmp_")?)
    };
    let (cc, ty) = rest.split_once('_')?;
    let cc: &'static str = ["eq", "lg", "gt", "ge", "lt", "le"]
        .into_iter()
        .find(|&c| c == cc)?;
    let signed = match ty {
        "i32" => true,
        "u32" => false,
        _ => return None,
    };
    let Fields::Sop { src, imm, .. } = &cmp_inst.fields else {
        return None;
    };
    let is_next = |op: &Operand| {
        matches!(*op, Operand::Scalar { code, dwords: 1 } if code == r)
            && cmp_inst.read(Reg::S(r)) == Some(next)
    };
    let compare = if kind {
        let imm = (*imm)?;
        if src.len() != 1 || !is_next(&src[0]) {
            return None;
        }
        let other = if signed {
            imm as i16 as i32 as u32
        } else {
            u32::from(imm)
        };
        Compare {
            cc,
            signed,
            other,
            swap: false,
        }
    } else {
        if src.len() != 2 {
            return None;
        }
        if is_next(&src[0]) {
            Compare {
                cc,
                signed,
                other: constant(&src[1])?,
                swap: false,
            }
        } else if is_next(&src[1]) {
            Compare {
                cc,
                signed,
                other: constant(&src[0])?,
                swap: true,
            }
        } else {
            return None;
        }
    };
    // Every edge into the loop brings a constant; the longest run counts.
    let mut most = 0;
    for (k, &q) in preds.iter().enumerate() {
        if k == k_back || rpo[q] == usize::MAX {
            continue;
        }
        debug_assert!(rpo[q] < rpo[h]);
        let init_inst = inst(args[k])?;
        if init_inst.write(Reg::S(r)) != Some(args[k]) {
            return None;
        }
        let start = match (&init_inst.fields, init_inst.name()) {
            (Fields::Sop { src, .. }, "s_mov_b32") if src.len() == 1 => constant(&src[0])?,
            (Fields::Sop { imm: Some(imm), .. }, "s_movk_i32") => *imm as i16 as i32 as u32,
            _ => return None,
        };
        most = most.max(run(start, step, compare, again)?);
    }
    Some(most)
}

/// Back edges taken from `start`, or `None` past [`MAX_TRIPS`].
fn run(start: u32, step: Step, compare: Compare, again: bool) -> Option<u32> {
    let mut x = start;
    for n in 0..=MAX_TRIPS {
        x = match step {
            Step::Add(c) => x.wrapping_add(c),
            Step::Sub(c) => x.wrapping_sub(c),
        };
        if compare.holds(x) != again {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lg0() -> Compare {
        Compare {
            cc: "lg",
            signed: false,
            other: 0,
            swap: false,
        }
    }

    #[test]
    fn down_counter_runs_its_count() {
        // s_mov s2, N; loop: ...; s_sub s2, s2, 1; s_cmp_lg s2, 0; s_cbranch_scc1 loop
        assert_eq!(run(16384, Step::Sub(1), lg0(), true), Some(16383));
        assert_eq!(run(1, Step::Sub(1), lg0(), true), Some(0));
    }

    #[test]
    fn up_counter_and_swapped_compare() {
        let lt = Compare {
            cc: "lt",
            signed: false,
            other: 100,
            swap: false,
        };
        assert_eq!(run(0, Step::Add(1), lt, true), Some(99));
        let gt = Compare {
            cc: "gt",
            signed: false,
            other: 100,
            swap: true,
        }; // 100 > x
        assert_eq!(run(0, Step::Add(4), gt, true), Some(24));
        let ge = Compare {
            cc: "ge",
            signed: true,
            other: -5i32 as u32,
            swap: false,
        };
        assert_eq!(run(5, Step::Add(-1i32 as u32), ge, true), Some(10));
    }

    /// A scalar loop as a synthetic load test encodes it: `s_mov_b32 s2, <init>`
    /// (or the given entry words), 16 FMA dwords of body, then
    /// `s_sub_u32 s2, s2, 1; s_cmp_lg_u32 s2, 0; s_cbranch_scc1 loop; s_endpgm`.
    fn down_loop(entry: &[u32]) -> Vec<u8> {
        let mut w = entry.to_vec();
        let at = w.len();
        for _ in 0..4 {
            w.extend([0xD54B0001u32, 257 | (257 << 9) | (241 << 18)]); // v_fma_f32 v1, v1, v1, -0.5
        }
        w.extend([0x80828102, 0xBF078002]);
        let n = w.len();
        w.push(0xBF850000 | ((at as i32 - (n as i32 + 1)) as u32 & 0xFFFF));
        w.push(0xBF810000);
        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn program_trips(code: &[u8]) -> Vec<u32> {
        let p = crate::ir::build(code, false, 8).expect("ir");
        let mut rpo = vec![usize::MAX; p.blocks.len()];
        for (n, &b) in crate::ir::reverse_postorder(&p.blocks).iter().enumerate() {
            rpo[b] = n;
        }
        let mut t: Vec<u32> = trips(&p, &rpo).into_values().collect();
        t.sort_unstable();
        t
    }

    #[test]
    fn literal_count_loop_is_counted() {
        // s_mov_b32 s2, 16384
        assert_eq!(program_trips(&down_loop(&[0xBE8203FF, 16384])), vec![16383]);
        // s_mov_b32 s2, 8 (inline constant 136)
        assert_eq!(program_trips(&down_loop(&[0xBE820388])), vec![7]);
        // s_movk_i32 s2, 5000
        assert_eq!(program_trips(&down_loop(&[0xB0021388])), vec![4999]);
    }

    #[test]
    fn count_from_data_is_not_counted() {
        // s_mov_b32 s2, s5 (an input register: the count comes from outside)
        assert!(program_trips(&down_loop(&[0xBE820305])).is_empty());
        // s_load_dword s2, s[0:1], 0 then wait: the count comes from memory
        let load = [0xF4000080u32, 0xFA000000, 0xBF8CC07F];
        assert!(program_trips(&down_loop(&load)).is_empty());
    }

    #[test]
    fn a_loop_with_another_exit_is_not_counted() {
        // loop: s_cmp_eq_u32 s5, 0; s_cbranch_scc1 out; <body>; s_sub; s_cmp_lg; s_cbranch_scc1 loop
        // out: s_endpgm. The early exit hangs on s5 (data), so the loop keeps its stage limit.
        let mut w = vec![0xBE8203FFu32, 16384];
        let at = w.len();
        w.extend([0xBF068005, 0]); // s_cmp_eq_u32 s5, 0; s_cbranch_scc1 (patched below)
        for _ in 0..4 {
            w.extend([0xD54B0001u32, 257 | (257 << 9) | (241 << 18)]);
        }
        w.extend([0x80828102, 0xBF078002]);
        let n = w.len();
        w.push(0xBF850000 | ((at as i32 - (n as i32 + 1)) as u32 & 0xFFFF));
        let end = w.len();
        w[at + 1] = 0xBF850000 | ((end as i32 - (at as i32 + 2)) as u32 & 0xFFFF);
        w.push(0xBF810000);
        let code: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert!(program_trips(&code).is_empty());
    }

    #[test]
    fn counted_limit_follows_the_count() {
        use super::super::lower::counted_limit;
        let lim = super::super::lower::LOOP_LIMIT;
        assert_eq!(counted_limit(4096, lim, Some(16383)), 16384);
        assert_eq!(counted_limit(4096, lim, Some(10)), 4096);
        assert_eq!(counted_limit(4096, lim, None), 4096);
        // A limit named by the switch stays as named.
        assert_eq!(counted_limit(64, 64, Some(16383)), 64);
    }

    #[test]
    fn a_loop_that_never_meets_its_exit_is_not_counted() {
        // Stepping by 2 from 1 never reaches 0 within the cap.
        assert_eq!(run(1, Step::Sub(2), lg0(), true), None);
        // Exit on scc0 of "lg": continue while x != 0 is false, i.e. stop at once.
        assert_eq!(run(10, Step::Sub(1), lg0(), false), Some(0));
    }
}
