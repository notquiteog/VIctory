//! Inline ray queries (DXR 1.1, shader model 6.5) in emitted shaders, for
//! the hardware ray-tracing path (`VICTORY_RT_HARDWARE`,
//! docs/hardware-rt.md): the host's top level is the root SRV
//! `t<SLOT_TLAS>` in [`super::shader::ROOT_CBV_SPACE`], and a walk the
//! translator recognises is one query on it.
//!
//! After Linox-IDK/VIctory 44280ad213 ("inline ray queries ... on a host top
//! level at root SRV t252 space1"), with the rest of the query's state
//! (candidates, committed indices, barycentrics) added.

// Used by the walk translation of stage (c); until then by its tests only.
#![allow(dead_code)]

use super::bitcode::{Attr, V};
use super::shader::{ROOT_CBV_SPACE, SLOT_TLAS, Shader, flags};

/// `RAY_FLAG_*` of `TraceRayInline` and `AllocateRayQuery`.
pub mod ray {
    pub const FORCE_OPAQUE: u32 = 0x1;
    pub const FORCE_NON_OPAQUE: u32 = 0x2;
    pub const ACCEPT_FIRST_HIT_AND_END_SEARCH: u32 = 0x4;
    pub const CULL_BACK_FACING_TRIANGLES: u32 = 0x10;
    pub const CULL_FRONT_FACING_TRIANGLES: u32 = 0x20;
    pub const SKIP_PROCEDURAL_PRIMITIVES: u32 = 0x200;
}

/// `COMMITTED_STATUS`: what the query has committed.
pub mod committed {
    pub const NOTHING: u32 = 0;
    pub const TRIANGLE_HIT: u32 = 1;
    pub const PROCEDURAL_PRIMITIVE_HIT: u32 = 2;
}

/// DXIL opcodes of the ray-query operations used here.
mod op {
    pub const ALLOCATE: u32 = 178;
    pub const TRACE_RAY_INLINE: u32 = 179;
    pub const PROCEED: u32 = 180;
    pub const ABORT: u32 = 181;
    pub const COMMIT_NON_OPAQUE_TRIANGLE_HIT: u32 = 182;
    pub const COMMITTED_STATUS: u32 = 184;
    pub const CANDIDATE_TYPE: u32 = 185;
    pub const CANDIDATE_TRIANGLE_FRONT_FACE: u32 = 191;
    pub const COMMITTED_TRIANGLE_FRONT_FACE: u32 = 192;
    pub const CANDIDATE_TRIANGLE_BARYCENTRICS: u32 = 193;
    pub const COMMITTED_TRIANGLE_BARYCENTRICS: u32 = 194;
    pub const CANDIDATE_TRIANGLE_RAY_T: u32 = 199;
    pub const COMMITTED_RAY_T: u32 = 200;
    pub const CANDIDATE_INSTANCE_INDEX: u32 = 201;
    pub const CANDIDATE_INSTANCE_ID: u32 = 202;
    pub const CANDIDATE_GEOMETRY_INDEX: u32 = 203;
    pub const CANDIDATE_PRIMITIVE_INDEX: u32 = 204;
    pub const COMMITTED_INSTANCE_INDEX: u32 = 207;
    pub const COMMITTED_INSTANCE_ID: u32 = 208;
    pub const COMMITTED_GEOMETRY_INDEX: u32 = 209;
    pub const COMMITTED_PRIMITIVE_INDEX: u32 = 210;
}

/// A query's integer state, by opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Index {
    InstanceIndex,
    InstanceId,
    GeometryIndex,
    PrimitiveIndex,
}

impl Shader {
    /// The handle of the host's top level ([`SLOT_TLAS`]), made once in the
    /// entry block.
    pub fn tlas_handle(&mut self) -> V {
        if let Some(&(_, h)) = self.root_srvs.iter().find(|r| r.0 == SLOT_TLAS) {
            return h;
        }
        let r = self.m.i32(u32::from(SLOT_TLAS));
        let bind = [r, r, self.m.i32(ROOT_CBV_SPACE), self.m.i8(0)];
        let bind = self.m.aggregate(self.t.bind, &bind);
        let f = self.m.bool(false);
        let block = self.entry;
        let h = self.op(
            block,
            "createHandleFromBinding",
            Attr::ReadNone,
            self.t.handle,
            217,
            &[bind, r, f],
        );
        // Resource kind 16: RTAccelerationStructure.
        let h = self.annotate(block, h, 16, 0);
        self.flags |= flags::RAYTRACING_1_1;
        self.root_srvs.push((SLOT_TLAS, h));
        h
    }

    /// `AllocateRayQuery` with constant ray `flags`.
    pub fn ray_query(&mut self, block: usize, flags: u32) -> V {
        let f = self.m.i32(flags);
        self.flags |= flags::RAYTRACING_1_1;
        self.op(
            block,
            "allocateRayQuery",
            Attr::NoUnwind,
            self.t.i32,
            op::ALLOCATE,
            &[f],
        )
    }

    /// `RayQuery::TraceRayInline` on the host's top level.
    #[allow(clippy::too_many_arguments)]
    pub fn trace_ray_inline(
        &mut self,
        block: usize,
        rq: V,
        flags: V,
        mask: V,
        origin: [V; 3],
        tmin: V,
        dir: [V; 3],
        tmax: V,
    ) {
        let h = self.tlas_handle();
        let args = [
            rq, h, flags, mask, origin[0], origin[1], origin[2], tmin, dir[0], dir[1], dir[2], tmax,
        ];
        let void = self.t.void;
        self.op(
            block,
            "rayQuery_TraceRayInline",
            Attr::NoUnwind,
            void,
            op::TRACE_RAY_INLINE,
            &args,
        );
    }

    /// `RayQuery::Proceed`: whether a candidate waits for the shader.
    pub fn ray_query_proceed(&mut self, block: usize, rq: V) -> V {
        let i1 = self.t.i1;
        self.op(
            block,
            "rayQuery_Proceed.i1",
            Attr::NoUnwind,
            i1,
            op::PROCEED,
            &[rq],
        )
    }

    /// `RayQuery::Abort`.
    pub fn ray_query_abort(&mut self, block: usize, rq: V) {
        let void = self.t.void;
        self.op(
            block,
            "rayQuery_Abort",
            Attr::NoUnwind,
            void,
            op::ABORT,
            &[rq],
        );
    }

    /// `RayQuery::CommitNonOpaqueTriangleHit`.
    pub fn ray_query_commit_triangle(&mut self, block: usize, rq: V) {
        let void = self.t.void;
        self.op(
            block,
            "rayQuery_CommitNonOpaqueTriangleHit",
            Attr::NoUnwind,
            void,
            op::COMMIT_NON_OPAQUE_TRIANGLE_HIT,
            &[rq],
        );
    }

    fn state_i32(&mut self, block: usize, rq: V, opcode: u32) -> V {
        let i32t = self.t.i32;
        self.op(
            block,
            "rayQuery_StateScalar.i32",
            Attr::ReadOnly,
            i32t,
            opcode,
            &[rq],
        )
    }

    fn state_i1(&mut self, block: usize, rq: V, opcode: u32) -> V {
        let i1 = self.t.i1;
        self.op(
            block,
            "rayQuery_StateScalar.i1",
            Attr::ReadOnly,
            i1,
            opcode,
            &[rq],
        )
    }

    fn state_f32(&mut self, block: usize, rq: V, opcode: u32) -> V {
        let f32t = self.t.f32;
        self.op(
            block,
            "rayQuery_StateScalar.f32",
            Attr::ReadOnly,
            f32t,
            opcode,
            &[rq],
        )
    }

    /// `CommittedStatus` ([`committed`]).
    pub fn ray_query_committed_status(&mut self, block: usize, rq: V) -> V {
        self.state_i32(block, rq, op::COMMITTED_STATUS)
    }

    /// `CandidateType` (0: a non-opaque triangle, 1: a procedural primitive).
    pub fn ray_query_candidate_type(&mut self, block: usize, rq: V) -> V {
        self.state_i32(block, rq, op::CANDIDATE_TYPE)
    }

    /// `CommittedRayT` or `CandidateTriangleRayT`.
    pub fn ray_query_t(&mut self, block: usize, rq: V, candidate: bool) -> V {
        self.state_f32(
            block,
            rq,
            if candidate {
                op::CANDIDATE_TRIANGLE_RAY_T
            } else {
                op::COMMITTED_RAY_T
            },
        )
    }

    /// `Committed...` or `Candidate...` of `which`.
    pub fn ray_query_index(&mut self, block: usize, rq: V, which: Index, candidate: bool) -> V {
        let opcode = match (which, candidate) {
            (Index::InstanceIndex, false) => op::COMMITTED_INSTANCE_INDEX,
            (Index::InstanceId, false) => op::COMMITTED_INSTANCE_ID,
            (Index::GeometryIndex, false) => op::COMMITTED_GEOMETRY_INDEX,
            (Index::PrimitiveIndex, false) => op::COMMITTED_PRIMITIVE_INDEX,
            (Index::InstanceIndex, true) => op::CANDIDATE_INSTANCE_INDEX,
            (Index::InstanceId, true) => op::CANDIDATE_INSTANCE_ID,
            (Index::GeometryIndex, true) => op::CANDIDATE_GEOMETRY_INDEX,
            (Index::PrimitiveIndex, true) => op::CANDIDATE_PRIMITIVE_INDEX,
        };
        self.state_i32(block, rq, opcode)
    }

    /// Whether the committed (or candidate) triangle faces the ray.
    pub fn ray_query_front_face(&mut self, block: usize, rq: V, candidate: bool) -> V {
        self.state_i1(
            block,
            rq,
            if candidate {
                op::CANDIDATE_TRIANGLE_FRONT_FACE
            } else {
                op::COMMITTED_TRIANGLE_FRONT_FACE
            },
        )
    }

    /// The committed (or candidate) triangle's barycentrics: the weights
    /// of its second and third vertices.
    pub fn ray_query_barycentrics(&mut self, block: usize, rq: V, candidate: bool) -> [V; 2] {
        let opcode = if candidate {
            op::CANDIDATE_TRIANGLE_BARYCENTRICS
        } else {
            op::COMMITTED_TRIANGLE_BARYCENTRICS
        };
        let f32t = self.t.f32;
        [0, 1].map(|k| {
            let c = self.m.i8(k);
            self.op(
                block,
                "rayQuery_StateVector.f32",
                Attr::ReadOnly,
                f32t,
                opcode,
                &[rq, c],
            )
        })
    }
}
