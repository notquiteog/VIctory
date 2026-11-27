//! The title's walks of its ray-tracing structures: the loops around its
//! node tests (`image_bvh_intersect_ray`, `image_bvh64_intersect_ray`), for
//! the hardware path (`VICTORY_RT_HARDWARE`, docs/hardware-rt.md), which
//! puts a host ray query in the place of a walk whose shape it knows.
//!
//! [`census`] describes each loop that holds a node test: its size, what it
//! reads and writes (group memory for a stack, title memory for instance
//! records), how lanes leave it, and the registers it leaves to the code
//! after it. `victory translate` writes one line per program with
//! `VICTORY_WALK_CENSUS=<file>` (outside the repository: game-derived).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::gfx10::Fields;
use crate::ir::{Program, Reg, Value};

/// One loop that holds node tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Walk {
    /// The loop header's block and byte offset.
    pub header: usize,
    pub offset: usize,
    /// Blocks and instructions of the loop (inner loops included).
    pub blocks: usize,
    pub insts: usize,
    /// Loops it lies in, and loops in it that hold node tests too.
    pub depth: usize,
    pub inner: usize,
    /// Node tests in it: 32-bit and 64-bit pointer forms.
    pub tests: usize,
    pub tests64: usize,
    /// Group memory reads, writes and atomics (a stack), scalar and vector
    /// loads of memory (buffers, flat/global: instance records, triangle
    /// data), stores, and atomics to memory.
    pub lds_reads: usize,
    pub lds_writes: usize,
    pub lds_other: usize,
    pub smem: usize,
    pub loads: usize,
    pub stores: usize,
    pub atomics: usize,
    /// Texture samples or loads other than the node test (alpha tests).
    pub images: usize,
    /// Cross-lane work (readlane, readfirstlane, DPP, ballots on masks).
    pub lanes: usize,
    /// Group barriers.
    pub barriers: usize,
    /// Edges that leave the loop, and the blocks they go to.
    pub exits: usize,
    pub targets: usize,
    /// Byte offsets of the blocks the loop leaves to.
    pub exit_offsets: Vec<usize>,
    /// Registers whose value, defined in the loop, is read after it.
    pub live_out: Vec<Reg>,
    /// Registers that carry values around the loop (header phis).
    pub carried: Vec<Reg>,
    /// Instruction names in the loop and how often, for the shape.
    pub ops: BTreeMap<&'static str, usize>,
    /// The loop's code with registers numbered by first use ([`canonical`]):
    /// loops with the same one are the same walk, whatever registers the
    /// compiler gave them (what a recogniser matches on).
    pub fingerprint: u64,
}

impl Walk {
    /// A short, stable name of the loop's shape: what a pattern matcher would
    /// sort on.
    pub fn shape(&self) -> String {
        format!(
            "tests{}{} lds{}/{}/{} loads{} smem{} stores{} atom{} img{} lanes{} bar{} exits{} inner{} out{} carried{} code {:016x}",
            self.tests,
            if self.tests64 > 0 { "w" } else { "" },
            self.lds_reads,
            self.lds_writes,
            self.lds_other,
            self.loads,
            self.smem,
            self.stores,
            self.atomics,
            self.images,
            self.lanes,
            self.barriers,
            self.exits,
            self.inner,
            self.live_out.len(),
            self.carried.len(),
            self.fingerprint,
        )
    }
}

/// The loops of `p`: (header, blocks in it), innermost last for each
/// header. A back edge goes to a block no later in reverse postorder.
pub fn loops(p: &Program) -> Vec<(usize, Vec<bool>)> {
    let order = crate::ir::reverse_postorder(&p.blocks);
    let mut index = vec![usize::MAX; p.blocks.len()];
    for (n, &b) in order.iter().enumerate() {
        index[b] = n;
    }
    let mut by_header: BTreeMap<usize, Vec<bool>> = BTreeMap::new();
    for &b in &order {
        for s in p.blocks[b].exit.successors() {
            if index[s] > index[b] {
                continue;
            }
            // The natural loop of the back edge b -> s.
            let body = by_header
                .entry(s)
                .or_insert_with(|| vec![false; p.blocks.len()]);
            body[s] = true;
            let mut stack = vec![b];
            while let Some(x) = stack.pop() {
                if body[x] {
                    continue;
                }
                body[x] = true;
                for &q in &p.blocks[x].preds {
                    if index[q] != usize::MAX {
                        stack.push(q);
                    }
                }
            }
        }
    }
    by_header.into_iter().collect()
}

/// The loops of `p` that hold node tests, innermost such loop per test.
pub fn census(p: &Program) -> Vec<Walk> {
    let all = loops(p);
    let is_test = |b: usize| {
        p.blocks[b]
            .insts
            .iter()
            .any(|i| i.name().starts_with("image_bvh"))
    };
    let tests: Vec<usize> = (0..p.blocks.len()).filter(|&b| is_test(b)).collect();
    // The loops holding a test, and of those the ones no smaller such loop
    // lies in (the walks; an outer loop around one, e.g. a loop over rays,
    // counts the walk as inner).
    let holding: Vec<&(usize, Vec<bool>)> = all
        .iter()
        .filter(|(_, body)| tests.iter().any(|&b| body[b]))
        .collect();
    let size = |body: &Vec<bool>| body.iter().filter(|&&x| x).count();
    let mut out = Vec::new();
    for (h, body) in &holding {
        let inner = holding
            .iter()
            .filter(|(h2, b2)| h2 != h && body[*h2] && size(b2) < size(body))
            .count();
        let depth = all
            .iter()
            .filter(|(h2, b2)| h2 != h && b2[*h] && size(b2) > size(body))
            .count();
        out.push(describe(p, *h, body, depth, inner));
    }
    out
}

fn describe(p: &Program, h: usize, body: &[bool], depth: usize, inner: usize) -> Walk {
    let mut w = Walk {
        header: h,
        offset: p.blocks[h].offset,
        depth,
        inner,
        ..Walk::default()
    };
    let mut defined: HashSet<Value> = HashSet::new();
    let mut targets = BTreeSet::new();
    for (b, block) in p.blocks.iter().enumerate() {
        if !body[b] {
            continue;
        }
        w.blocks += 1;
        w.insts += block.insts.len();
        for phi in &block.phis {
            defined.insert(phi.value);
        }
        for inst in &block.insts {
            for &(_, v) in &inst.writes {
                defined.insert(v);
            }
            let name = inst.name();
            *w.ops.entry(name).or_default() += 1;
            match &inst.fields {
                Fields::Image { .. } if name.starts_with("image_bvh") => {
                    w.tests += 1;
                    w.tests64 += usize::from(name.starts_with("image_bvh64"));
                }
                Fields::Image { .. } => w.images += 1,
                Fields::Ds { .. } => {
                    if name.starts_with("ds_read") || name.starts_with("ds_load") {
                        w.lds_reads += 1;
                    } else if name.starts_with("ds_write") || name.starts_with("ds_store") {
                        w.lds_writes += 1;
                    } else {
                        w.lds_other += 1;
                    }
                }
                Fields::Smem { .. } => w.smem += 1,
                _ if name.contains("atomic") => w.atomics += 1,
                _ if name.starts_with("buffer_load")
                    || name.starts_with("global_load")
                    || name.starts_with("flat_load")
                    || name.starts_with("tbuffer_load") =>
                {
                    w.loads += 1
                }
                _ if name.starts_with("buffer_store")
                    || name.starts_with("global_store")
                    || name.starts_with("flat_store")
                    || name.starts_with("tbuffer_store") =>
                {
                    w.stores += 1
                }
                _ if name.contains("readlane")
                    || name.contains("readfirstlane")
                    || name.contains("permlane")
                    || name.contains("_dpp")
                    || name.contains("bcnt")
                    || name.contains("ff1") =>
                {
                    w.lanes += 1
                }
                _ if name == "s_barrier" => w.barriers += 1,
                _ => {}
            }
        }
        for s in block.exit.successors() {
            if !body[s] {
                w.exits += 1;
                targets.insert(s);
            }
        }
    }
    w.targets = targets.len();
    w.exit_offsets = targets.iter().map(|&t| p.blocks[t].offset).collect();
    w.carried = p.blocks[h].phis.iter().map(|phi| phi.reg).collect();
    // Values defined in the loop read outside it (by instructions or phis).
    let mut out = BTreeSet::new();
    for (b, block) in p.blocks.iter().enumerate() {
        if body[b] {
            continue;
        }
        for phi in &block.phis {
            for (k, a) in phi.args.iter().enumerate() {
                if defined.contains(a) && block.preds.get(k).is_some_and(|&q| body[q]) {
                    out.insert(phi.reg);
                }
            }
        }
        for inst in &block.insts {
            for &(r, v) in &inst.reads {
                if defined.contains(&v) {
                    out.insert(r);
                }
            }
        }
    }
    w.live_out = out.into_iter().collect();
    w.fingerprint = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        canonical(p, body).hash(&mut h);
        h.finish()
    };
    w
}

/// The loop's code, block by block in program order, each instruction its
/// name and fields with every register renamed by its first appearance in
/// the loop (scalars and vectors apart; `vcc`, `m0`, `exec` and the like
/// keep their names). Literals, inline constants and offsets stay.
pub fn canonical(p: &Program, body: &[bool]) -> String {
    let mut blocks: Vec<usize> = (0..p.blocks.len()).filter(|&b| body[b]).collect();
    blocks.sort_by_key(|&b| p.blocks[b].offset);
    let mut scalars: BTreeMap<u8, usize> = BTreeMap::new();
    let mut vectors: BTreeMap<u8, usize> = BTreeMap::new();
    let mut out = String::new();
    for b in blocks {
        out.push_str("|");
        for inst in &p.blocks[b].insts {
            out.push_str(inst.name());
            // Fields' operands, renamed, in the order the fields print them.
            let mut text = format!("{:?}", inst.fields);
            let mut renamed = String::with_capacity(text.len());
            for (kind, key) in [("Scalar { code: ", 0u8), ("Vector { reg: ", 1)] {
                renamed.clear();
                let mut rest = text.as_str();
                while let Some(at) = rest.find(kind) {
                    renamed.push_str(&rest[..at + kind.len()]);
                    rest = &rest[at + kind.len()..];
                    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
                    let n: u8 = rest[..digits].parse().unwrap_or(0);
                    rest = &rest[digits..];
                    let name = if key == 0 && n >= 106 && n < crate::ir::PARKED {
                        format!("x{n}")
                    } else {
                        let map = if key == 0 { &mut scalars } else { &mut vectors };
                        let next = map.len();
                        format!("r{}", *map.entry(n).or_insert(next))
                    };
                    renamed.push_str(&name);
                }
                renamed.push_str(rest);
                std::mem::swap(&mut text, &mut renamed);
            }
            out.push_str(&text);
            out.push(';');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_without_loops_has_no_walks() {
        // s_endpgm
        let code = 0xbf81_0000u32.to_le_bytes();
        let p = crate::ir::build(&code, false, 4).unwrap();
        assert!(loops(&p).is_empty());
        assert!(census(&p).is_empty());
    }
}
