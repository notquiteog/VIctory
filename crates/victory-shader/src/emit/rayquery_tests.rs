//! The inline ray query in emitted DXIL ([`super::rayquery`]): a compute
//! shader that traces one ray against the host's top level, commits
//! non-opaque candidates in a loop (as an alpha test would), and reads what
//! a walk leaves (status, distance, barycentrics, indices, facing). The
//! validator (`dxil.dll`, where there is one) must accept it.

use super::bitcode::{Attr, Bin, Cast, Term, V, pred};
use super::rayquery::{Index, ray};
use super::shader::Shader;

fn ray_query_shader() -> Vec<u8> {
    let mut s = Shader::new(false);
    s.threads = Some([32, 1, 1]);
    let b = s.entry;
    let rq = s.ray_query(b, ray::SKIP_PROCEDURAL_PRIMITIVES);
    let flags = s.m.i32(0);
    let mask = s.m.i32(0xff);
    let zero = s.m.f32(0.0f32.to_bits());
    let one = s.m.f32(1.0f32.to_bits());
    let far = s.m.f32(1000.0f32.to_bits());
    s.trace_ray_inline(
        b,
        rq,
        flags,
        mask,
        [zero, zero, zero],
        zero,
        [zero, zero, one],
        far,
    );
    // while (q.Proceed()) { if (candidate t < 500) q.CommitNonOpaqueTriangleHit(); }
    let (lp, cand, commit, done) = (
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
    );
    s.m.terminate(b, Term::Br(lp));
    let more = s.ray_query_proceed(lp, rq);
    s.m.terminate(lp, Term::CondBr(more, cand, done));
    let t = s.ray_query_t(cand, rq, true);
    let _ = s.ray_query_index(cand, rq, Index::PrimitiveIndex, true);
    let _ = s.ray_query_barycentrics(cand, rq, true);
    let _ = s.ray_query_candidate_type(cand, rq);
    let half = s.m.f32(500.0f32.to_bits());
    let near = s.m.cmp(cand, pred::OLT, t, half);
    s.m.terminate(cand, Term::CondBr(near, commit, lp));
    s.ray_query_commit_triangle(commit, rq);
    s.m.terminate(commit, Term::Br(lp));
    let status = s.ray_query_committed_status(done, rq);
    let t = s.ray_query_t(done, rq, false);
    let [u, v] = s.ray_query_barycentrics(done, rq, false);
    let inst = s.ray_query_index(done, rq, Index::InstanceIndex, false);
    let id = s.ray_query_index(done, rq, Index::InstanceId, false);
    let geo = s.ray_query_index(done, rq, Index::GeometryIndex, false);
    let prim = s.ray_query_index(done, rq, Index::PrimitiveIndex, false);
    let _front = s.ray_query_front_face(done, rq, false);
    // Keep the results alive: one store to a root UAV.
    let mut sum = s.m.bin(done, Bin::Add, status, inst);
    for x in [id, geo, prim] {
        sum = s.m.bin(done, Bin::Add, sum, x);
    }
    let tu = s.m.bin(done, Bin::Add, t, u);
    let tuv = s.m.bin(done, Bin::Add, tu, v);
    let h = s.root_uav_handle(0);
    let i32t = s.t.i32;
    let bits = s.m.cast(done, Cast::Bitcast, tuv, i32t);
    store(&mut s, done, h, 0, sum);
    store(&mut s, done, h, 4, bits);
    s.m.terminate(done, Term::Ret);
    s.finish()
}

/// One dword `x` to byte `at` of the raw buffer `h`.
fn store(s: &mut Shader, block: usize, h: V, at: u32, x: V) {
    let i32t = s.t.i32;
    let u = s.m.undef(i32t);
    let byte = s.m.i32(at);
    let mask = s.m.i8(1);
    let align = s.m.i32(4);
    let void = s.t.void;
    s.op(
        block,
        "rawBufferStore.i32",
        Attr::NoUnwind,
        void,
        140,
        &[h, byte, u, x, u, u, u, mask, align],
    );
}

#[test]
fn a_ray_query_shader_validates() {
    let mut dxil = ray_query_shader();
    assert!(!dxil.is_empty());
    #[cfg(windows)]
    if let Ok(mut v) =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
    {
        v.validate(&mut dxil).unwrap();
    }
}
