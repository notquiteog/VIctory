//! Scalars at per-lane joins. On the console a scalar register is one
//! value per wave: where some lanes of a wave take a branch and the rest
//! skip it, the wave runs the branch and every lane reads what it left
//! after the join. The per-lane form keeps each lane's own path's value, so
//! the lanes that skipped the branch still read the old one.
//! [`Lower::wave_scalars`] gives every lane the wave's value instead, where
//! one host wave holds one console wave.

use super::*;

impl Lower<'_> {
    /// Each predecessor of IR block `b` (by its index in `preds`): its rank
    /// in code order, 0 the first.
    pub(super) fn pred_ranks(&self, b: usize) -> Vec<u32> {
        let p = self.x.p;
        let preds = &p.blocks[b].preds;
        preds
            .iter()
            .map(|&q| {
                let at = p.blocks[q].offset;
                preds.iter().filter(|&&r| p.blocks[r].offset < at).count() as u32
            })
            .collect()
    }

    /// Whether every lane enters IR block `b` along the edge the console's
    /// wave takes: every branch into it is uniform (SCC values). A per-lane
    /// branch into the join (an EXEC skip, a loop's per-lane exit) is where
    /// lanes take edges the wave did not.
    fn uniform_join(&self, b: usize) -> bool {
        let st = self.x.st;
        let preds = &self.x.p.blocks[b].preds;
        preds.iter().filter(|&&q| self.block[q].is_some()).all(|&q| match st.exits[q] {
            LaneExit::Branch { when, taken, not_taken } if taken != not_taken => {
                st.bools.support(when).iter().all(|a| matches!(a, Atom::Uniform(_)))
            }
            _ => true,
        })
    }

    /// At the start of IR block `b`, after its phis: a scalar the console
    /// wave holds is one value. Where its lanes reach `b` along different
    /// edges (a per-lane join), the per-lane form gives each lane the value
    /// along its own edge, so lanes that skipped a branch keep what the
    /// branch replaced; the console's wave ran every path some lane took,
    /// in code order, and holds what the last of them left. So each scalar
    /// phi takes, in every lane, the value of a lane that came from the
    /// latest predecessor any lane came from. Only where one host wave is
    /// one console wave (the console's lanes at the console's wave size),
    /// not at loop headers, and not past a loop between the branch and the
    /// join.
    pub(super) fn wave_scalars(&mut self, b: usize) {
        let p = self.x.p;
        if !self.console_lanes
            || self.s.mesh.is_some()
            || self.s.wave_size != Some(self.wave())
            || self.halves.is_some()
            || self.exchange.is_some()
            || self.uniform_join(b)
        {
            return;
        }
        let preds: Vec<usize> = p.blocks[b]
            .preds
            .iter()
            .copied()
            .filter(|&q| self.block[q].is_some())
            .collect();
        if preds.len() < 2 || preds.iter().any(|&q| self.rpo[b] <= self.rpo[q]) {
            return;
        }
        // A loop between the branch and the join: lanes leave it on their
        // own rounds, and the per-lane form's scalars there are each lane's
        // own (a round count), not the wave's, so no one lane's is the
        // console's.
        let from = preds.iter().map(|&q| self.rpo[q]).min().unwrap_or(0);
        if self
            .counters
            .keys()
            .any(|&h| from < self.rpo[h] && self.rpo[h] < self.rpo[b])
        {
            return;
        }
        let scalars: Vec<(Value, V)> = p.blocks[b]
            .phis
            .iter()
            .filter(|phi| matches!(phi.reg, Reg::S(c) if c != EXEC && c != EXEC + 1))
            .filter(|phi| {
                !self.x.st.masks.contains_key(&phi.value) && !self.x.st.highs.contains_key(&phi.value)
            })
            .filter_map(|phi| match self.vals.get(&phi.value) {
                Some(&Val::I32(x)) => Some((phi.value, x)),
                _ => None,
            })
            .collect();
        if scalars.is_empty() {
            return;
        }
        let c = self.cur;
        let i32t = self.s.t.i32;
        let rank = self.s.m.phi(c, i32t);
        self.edge_ranks.push((rank, b));
        let (max, min, unsigned) = (self.s.m.i8(3), self.s.m.i8(2), self.s.m.i8(1));
        let last = self.s.op(c, "waveActiveOp.i32", Attr::NoUnwind, i32t, 119, &[rank, max, unsigned]);
        let mine = self.s.m.cmp(c, pred::EQ, rank, last);
        let lane = self.lane_index();
        let none = self.s.m.i32(u32::MAX);
        let candidate = self.s.m.select(c, mine, lane, none);
        let from = self.s.op(c, "waveActiveOp.i32", Attr::NoUnwind, i32t, 119, &[candidate, min, unsigned]);
        self.s.flags |= flags::WAVE_OPS;
        for (value, x) in scalars {
            let wave = self.read_lane(x, from);
            self.vals.insert(value, Val::I32(wave));
        }
    }

    /// The phis of [`Lower::wave_scalars`]: each edge's rank.
    pub(super) fn fill_edge_ranks(&mut self) {
        let p = self.x.p;
        for (v, b) in std::mem::take(&mut self.edge_ranks) {
            let ranks = self.pred_ranks(b);
            let mut incoming = Vec::new();
            for (n, &q) in p.blocks[b].preds.iter().enumerate() {
                let Some(qb) = self.tail[q] else { continue };
                let qb = self.edges.get(&(q, b)).copied().unwrap_or(qb);
                incoming.push((self.s.m.i32(ranks[n]), qb));
            }
            self.s.m.set_phi(v, incoming);
        }
    }
}
