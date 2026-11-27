//! Float atomic minimum and maximum on title memory (`global_atomic_fmin`,
//! `global_atomic_fmax`), end to end: a synthetic program in which eight
//! threads put a first value in eight dwords and all 32 then take the
//! minimum (maximum) of a dword and a value of their own, lowered both ways
//! ([`super::float_atomic_ints`]), run on the host GPU and WARP, and the
//! dwords compared with the reference: NaNs (of both signs, in the dword and
//! as the value) are left out of the order, -0 orders below +0, denormals
//! are kept.

use super::host::{Dispatch, Host, Root, Stride, View};
use super::intersect_tests::build_compute_shader;
use super::lower::TEST_MEMORY_UAV;
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};

const THREADS: u32 = 32;
const SLOTS: usize = 8;
/// Where the tests' title memory is: past 4 GiB.
const BASE: u64 = 0x12_3456_7800;

/// Per thread, its record (address low, high, value, first value) in
/// v1-v4; threads 0-7 store the first value; a barrier; every thread's
/// atomic, the dword from before stored to the UAV at 4 bytes a thread.
fn program(max: bool) -> Vec<u32> {
    let op: u32 = if max { 0x40 } else { 0x3f };
    vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0xF408_0302,
        0xFA00_0020, // s_load_dwordx4 s[12:15], s[4:5], 0x20
        0x3428_0084, // v_lshlrev_b32 v20, 4, v0
        0xE038_1000,
        0x8002_0114, // buffer_load_dwordx4 v[1:4], v20, s[8:11], 0 offen
        0xBE94_037E, // s_mov_b32 s20, exec_lo
        0x7DA8_0088, // v_cmpx_gt_u32 exec_lo, 8, v0
        0xDC70_8000,
        0x007D_0401, // global_store_dword v[1:2], v4, off
        0xBEFE_0314, // s_mov_b32 exec_lo, s20
        0xBF8A_0000, // s_barrier
        0xDC01_8000 | op << 18,
        0x057D_0301, // global_atomic_fmin/fmax v5, v[1:2], v3, off glc
        0x342A_0082, // v_lshlrev_b32 v21, 2, v0
        0xE070_1000,
        0x8003_0515, // buffer_store_dword v5, v21, s[12:15], 0 offen
        0xBF81_0000, // s_endpgm
    ]
}

fn native(words: &[u32]) -> NativeProgram {
    let table = |kind, at| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords: at,
        lower_bound: 0,
        stride: 4,
        dwords: 4,
        user_data: 4,
        slot: 1,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 22,
        layout: vec![
            RootParam { root: RootType::Constants, table_offset_dwords: 0, sgprs: 4, base_register: 0, user_data: 0, slot: 0 },
            RootParam { root: RootType::Table, table_offset_dwords: 0, sgprs: 1, base_register: 0, user_data: 4, slot: 1 },
        ],
        used: vec![table(ResourceType::Srv, 0), table(ResourceType::Uav, 8)],
        io: StageIo::Cs(ComputeIo { rsrc2: 5 << 1, threads: [THREADS, 1, 1] }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

const NAN: u32 = 0x7fc0_0000;
const NEG_NAN: u32 = 0xffc0_0000;

/// The dwords' first values.
const FIRST: [u32; SLOTS] = [
    0,           // +0
    0x8000_0000, // -0
    0x7f80_0000, // +inf
    0xff80_0000, // -inf
    NAN,
    NEG_NAN,
    0x3fc0_0000, // 1.5
    0xc020_0000, // -2.5
];

/// The value of thread `t` (on dword `t % 8`).
fn value(t: usize) -> u32 {
    const VALUES: [u32; 16] = [
        0x3f80_0000, // 1
        0xbf80_0000, // -1
        0x8000_0000, // -0
        0,           // +0
        NAN,
        0x0000_0001, // the least denormal
        0x8000_0001, // and its negative
        0x4120_0000, // 10
        0xc120_0000, // -10
        NEG_NAN,
        0x7f7f_ffff, // the greatest float
        0xff7f_ffff,
        0x3e80_0000, // 0.25
        0xbe80_0000,
        0x7fa0_0001, // a signalling NaN
        0x4000_0000, // 2
    ];
    VALUES[(t * 7 + t / 8) % VALUES.len()]
}

fn is_nan(x: u32) -> bool {
    x & 0x7fff_ffff > 0x7f80_0000
}

/// The order of non-NaN floats, -0 below +0, as signed integers.
fn key(x: u32) -> i32 {
    let x = x as i32;
    x ^ ((x >> 31) & 0x7fff_ffff)
}

/// What the dword `slot` holds after every thread's operation (in any
/// order, the same).
fn expected(slot: usize, max: bool) -> u32 {
    let mut d = FIRST[slot];
    for t in (slot..THREADS as usize).step_by(SLOTS) {
        let v = value(t);
        if is_nan(v) {
            continue;
        }
        let better = if max { key(v) > key(d) } else { key(v) < key(d) };
        if is_nan(d) || better {
            d = v;
        }
    }
    d
}

fn records() -> Vec<u32> {
    let mut data = Vec::new();
    for t in 0..THREADS as usize {
        let at = BASE + 4 * (t % SLOTS) as u64;
        data.extend([at as u32, (at >> 32) as u32, value(t), FIRST[t % SLOTS]]);
    }
    data
}

fn run(max: bool, ints: bool) {
    // The switch is read once per process: these tests set the way for the
    // lowering alone.
    super::lower::TEST_FLOAT_ATOMIC_INTS.set(Some(ints));
    TEST_MEMORY_UAV.set(Some((2, BASE)));
    let built = build_compute_shader(&native(&program(max)));
    TEST_MEMORY_UAV.set(None);
    super::lower::TEST_FLOAT_ATOMIC_INTS.set(None);
    let (shader, _) = built.expect("the program lowers");
    // Every case is there.
    let all: Vec<u32> = (0..SLOTS).map(|s| expected(s, max)).collect();
    // (Of the minimum: a dword ends at -0 or a negative denormal; of both,
    // at a NaN never.)
    assert!(max || all.iter().any(|&x| x == 0x8000_0000 || x == 0x8000_0001), "no -0 or negative denormal case: {all:x?}");
    assert!(!all.iter().any(|&x| is_nan(x)), "a dword ends at a NaN: {all:x?}");
    let data = records();
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut ran = 0;
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) =
            host.compute_pipeline(&shader).unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(vec![0; 5]))],
                    views: &[
                        View::Buffer { data: &data, stride: Stride::Raw },
                        View::RawUav(THREADS),
                        View::RawUav(SLOTS as u32),
                    ],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        for slot in 0..SLOTS {
            let (got, want) = (out[1][slot], expected(slot, max));
            assert_eq!(
                got,
                want,
                "{label}, {} by {}, dword {slot}: got {got:#x} ({}), want {want:#x} ({})",
                if max { "maximum" } else { "minimum" },
                if ints { "integers" } else { "exchanges" },
                f32::from_bits(got),
                f32::from_bits(want)
            );
        }
        ran += 1;
    }
    assert!(ran > 0, "no SM 6.6 device available");
}

#[test]
fn a_float_atomic_minimum_by_integers_matches_the_reference_on_the_card() {
    run(false, true);
}

#[test]
fn a_float_atomic_maximum_by_integers_matches_the_reference_on_the_card() {
    run(true, true);
}
