//! Which VGPR values are the same in every lane of a console wave, for a console wave64
//! program on a host of 32-lane waves: such a value read with `v_readfirstlane` needs no
//! exchange between the two host waves. After fxceless/victory-fork PR #156 (sedzz, with
//! ryotdev's correction for guarded writes): without the three further proofs the structure
//! builder's three prefix-sum passes do not translate for 32-lane waves ("a first-lane read
//! where not every lane runs"), and the builder, whose stages run all or none, stays off.

use std::collections::HashSet;

use crate::gfx10::{Fields, NULL, Operand};
use crate::ir::{Program, Reg, Value};
use crate::structure::{Atom, Bool, LaneExit, Structured};

/// VGPR values that are the same in every lane holding them (`all` is `lower.rs`'s `all_lane_blocks`):
/// written by an instruction every arriving lane runs (no guard), lane by lane, from scalars,
/// constants and such values; or loaded from an address made of them. The compiler keeps
/// wave-uniform data in VGPRs and fetches it with `v_readfirstlane`.
///
/// Three more facts are proved, each only where no lane of a wave can part from the others:
/// - the wave number (`tid >> 6` of the entry `v0`, in a group whose x size is a multiple of
///   64) is the same in all lanes of a console wave;
/// - an LDS read (`ds_read_b32`) at such an address is;
/// - a value joined by a phi is, also at a loop header, when its arguments are and every
///   predecessor is entered by all of the arriving lanes or none (the lanes' EXEC there is
///   made of conditions every lane agrees on: SCC values, compares of such values, and the
///   loop-carried masks made of them). That is a greatest fixed point: a loop-carried value
///   is taken to be alike and the assumption is dropped once an argument is not.
///
/// shortcut: an LDS read is taken to see what a read by another wave of the same instruction
/// would; a write by another wave between the two host waves' reads is not looked for (the
/// scan programs read after a barrier).
pub(super) fn lane_uniform(p: &Program, st: &Structured, all: &[bool], threads: Option<[u32; 3]>) -> HashSet<Value> {
    let is_mask = |v: Value| st.masks.contains_key(&v) || st.highs.contains_key(&v);
    let entry_v0 = p.inputs.iter().find(|(r, _)| *r == Reg::V(0)).map(|x| x.1);
    let waves_whole = threads.is_some_and(|t| t[0] != 0 && t[0].is_multiple_of(64));
    // the V-register phis, and the lane-mask phis, assumed alike until shown not to be
    let mut assumed_v: HashSet<Value> = HashSet::new();
    let mut assumed_m: HashSet<Value> = HashSet::new();
    for block in &p.blocks {
        for phi in &block.phis {
            if matches!(phi.reg, Reg::V(_)) {
                assumed_v.insert(phi.value);
            }
            if st.masks.contains_key(&phi.value) {
                assumed_m.insert(phi.value);
            }
        }
    }
    // Whether a condition is the same in every lane of the wave.
    let uniform_bool = |b: Bool, same: &HashSet<Value>, cmp: &HashSet<Value>, masks: &HashSet<Value>| {
        st.bools.support(b).iter().all(|a| match a {
            Atom::Uniform(_) => true,
            Atom::Lane(v) => cmp.contains(v) || same.contains(v),
            Atom::Phi(v) => masks.contains(v),
            _ => false,
        })
    };
    loop {
        let mut same: HashSet<Value> = HashSet::new();
        // values written by compares whose operands are all alike (the lane's bit is the wave's)
        let mut cmp: HashSet<Value> = HashSet::new();
        for b in crate::ir::reverse_postorder(&p.blocks) {
            let block = &p.blocks[b];
            let whole = block.preds.iter().all(|&q| all.get(q).copied().unwrap_or(false));
            // every predecessor is entered by all the arriving lanes or none of them
            let together = block.preds.iter().all(|&q| {
                uniform_bool(st.reach[q], &same, &cmp, &assumed_m)
                    && st.guards[q]
                        .iter()
                        .all(|g| (*g).is_none_or(|g| uniform_bool(g, &same, &cmp, &assumed_m)))
                    && match &st.exits[q] {
                        LaneExit::Branch { when, .. } => uniform_bool(*when, &same, &cmp, &assumed_m),
                        _ => true,
                    }
            });
            for phi in &block.phis {
                if !matches!(phi.reg, Reg::V(_)) {
                    continue;
                }
                if whole && phi.args.iter().all(|a| same.contains(a)) {
                    same.insert(phi.value);
                } else if together && assumed_v.contains(&phi.value) {
                    // its arguments are checked once the pass is done
                    same.insert(phi.value);
                }
            }
            for (i, inst) in block.insts.iter().enumerate() {
                let name = inst.name();
                let alike_ops = |ops: &[Operand], same: &HashSet<Value>| {
                    ops.iter().all(|op| match *op {
                        Operand::Vector { reg, .. } => {
                            // every dword of the operand the instruction reads from that register on
                            let read: Vec<Value> = inst
                                .reads
                                .iter()
                                .filter(|r| matches!(r.0, Reg::V(x) if x >= reg))
                                .map(|r| r.1)
                                .collect();
                            !read.is_empty() && read.iter().all(|x| same.contains(x))
                        }
                        // a scalar is one value; a lane mask is not, unless a compare of such values made it
                        Operand::Scalar { code, .. } if code != NULL => {
                            inst.read(Reg::S(code)).is_none_or(|x| !is_mask(x) || same.contains(&x))
                        }
                        _ => true,
                    })
                };
                // Even a wave-uniform guard can be false. The lanes then keep the old
                // destination, which may differ between the two halves of a console wave.
                if st.guards[b][i].is_some()
                    && inst.writes.iter().any(|&(reg, _)| {
                        matches!(reg, Reg::V(_))
                            && inst.read(reg).is_some_and(|old| !same.contains(&old))
                    })
                {
                    continue;
                }
                // a read of the group's LDS at an address the lanes agree on
                if let Fields::Ds { dst: Some(_), addr: Some(addr), gds: false, .. } = &inst.fields
                    && name == "ds_read_b32"
                    && alike_ops(&[*addr], &same)
                    && st.guards[b][i].is_none_or(|g| uniform_bool(g, &same, &cmp, &assumed_m))
                {
                    same.extend(inst.writes.iter().filter(|w| w.0 != Reg::Scc).map(|w| w.1));
                    continue;
                }
                let ops: Vec<Operand> = match &inst.fields {
                    Fields::Valu(v) if v.dpp.is_none() && v.sdwa.is_none() => v.src.clone(),
                    _ => continue,
                };
                if name.starts_with("v_cmp") {
                    if alike_ops(&ops, &same) {
                        cmp.extend(inst.writes.iter().map(|w| w.1));
                    }
                    if name.starts_with("v_cmpx") {
                        continue;
                    }
                } else if name.starts_with("v_mbcnt") || name.contains("lane") {
                    continue;
                }
                // `tid >> k` (k at least 6) of the entry thread number: the wave's
                if waves_whole
                    && name == "v_lshrrev_b32"
                    && let [Operand::Const(134..=159), Operand::Vector { reg: 0, .. }] = &ops[..]
                    && let Some(v0) = entry_v0
                    && inst.read(Reg::V(0)) == Some(v0)
                    && st.guards[b][i].is_none_or(|g| uniform_bool(g, &same, &cmp, &assumed_m))
                {
                    same.extend(inst.writes.iter().filter(|w| w.0 != Reg::Scc).map(|w| w.1));
                    continue;
                }
                // lanes outside EXEC keep another value, unless the lanes agree on running it
                if st.guards[b][i].is_some_and(|g| !uniform_bool(g, &same, &cmp, &assumed_m)) {
                    continue;
                }
                if alike_ops(&ops, &same) {
                    same.extend(inst.writes.iter().filter(|w| w.0 != Reg::Scc).map(|w| w.1));
                }
            }
        }
        // what was assumed of the loop-carried values: dropped where it did not hold
        let mut changed = false;
        for block in &p.blocks {
            for phi in &block.phis {
                if assumed_v.contains(&phi.value)
                    && same.contains(&phi.value)
                    && !phi.args.iter().all(|a| same.contains(a))
                {
                    assumed_v.remove(&phi.value);
                    changed = true;
                }
                if assumed_m.contains(&phi.value)
                    && !phi.args.iter().all(|a| {
                        st.masks.get(a).is_some_and(|&m| uniform_bool(m, &same, &cmp, &assumed_m))
                    })
                {
                    assumed_m.remove(&phi.value);
                    changed = true;
                }
            }
        }
        if !changed {
            return same;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::Reg;
    use crate::{ir, structure};

    const S_ENDPGM: u32 = 0xBF81_0000;

    /// A write under a lane guard leaves the other lanes their old value: the result is alike
    /// in every lane only if the old value was (ryotdev's correction to the proof).
    #[test]
    fn a_guarded_uniform_write_can_keep_different_values_in_each_lane() {
        let check = |initial: u32, write: &[u32]| {
            let mut words = vec![
                initial,     // v_mov_b32 v1, v0 (thread ID) or 0
                0x7E04_0280, // v_mov_b32 v2, 0
                0x7D88_0400, // v_cmp_gt_u32 vcc, s0, v2
                0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
            ];
            words.extend_from_slice(write);
            words.extend_from_slice(&[
                0xBEFE_040C, // s_mov_b64 exec, s[12:13]
                0x7E0E_0501, // v_readfirstlane_b32 s7, v1
                S_ENDPGM,
            ]);
            let code: Vec<_> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let p = ir::build(&code, true, 8).unwrap();
            let st = structure::structure(&p).unwrap();
            let all = super::super::lower::all_lane_blocks(&st).unwrap();
            let uniform = super::lane_uniform(&p, &st, &all, Some([64, 1, 1]));
            let read = p
                .blocks
                .iter()
                .flat_map(|b| &b.insts)
                .find(|i| i.name() == "v_readfirstlane_b32")
                .unwrap();
            uniform.contains(&read.read(Reg::V(1)).unwrap())
        };

        let writes: &[(&str, &[u32])] = &[
            ("v_mov_b32 v1, 1", &[0x7E02_0281]),
            ("v_lshrrev_b32 v1, 6, v0", &[0x2C02_0086]),
            ("ds_read_b32 v1, v2", &[0xD8D8_0000, 0x0100_0002]),
        ];
        let mut divergent = Vec::new();
        for &(name, write) in writes {
            assert!(check(0x7E02_0280, write), "{name}: old and new values are uniform");
            if check(0x7E02_0300, write) {
                divergent.push(name);
            }
        }
        assert!(divergent.is_empty(), "when s0 is zero, lanes keep their IDs: {divergent:?}");
    }
}
