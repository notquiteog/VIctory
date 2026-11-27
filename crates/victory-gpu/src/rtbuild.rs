//! Host ray-tracing structures for the hardware path (`VICTORY_RT_HARDWARE=1`,
//! docs/hardware-rt.md, stage (b)): for each structure the title's builder
//! builds, unpacks or copies, a D3D12 structure of the host's, built from the
//! title's finished node data, so that a host hit names the title's nodes.
//!
//! At each such call of the builder's (`crate::hwrt`), after the builder's
//! own work, in the title list's order (`crate::external::in_order`):
//!
//! 1. the structure's bytes are copied out of title memory on the card
//!    (`crate::memory::card_page`; a page no copy holds yet leaves the
//!    structure for a later list) into a buffer of its own;
//! 2. a program of ours (`rtbuild.hlsl`) walks it from its root and writes,
//!    for a bottom level, each triangle at its node pointer's place (node
//!    index times four plus the triangle's number: a host hit's primitive
//!    index gives the title's triangle node pointer back) and, for a top
//!    level, each instance node as a D3D12 instance description at its node
//!    index (the instance index gives the instance node pointer back), with
//!    the host address of its bottom level by title address;
//! 3. the host structure is built from that.
//!
//! A copy (compacting or cloning) clones the host structure. With
//! `VICTORY_RT_VALIDATE=1` each walk's counts and bounds are read back some
//! frames later and set against the title's header, and every ten seconds a
//! line tells how they agreed and what the work cost on this thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, s};

use crate::host::{HostList, host};

const SOURCE: &str = include_str!("rtbuild.hlsl");

/// Title memory's pages, as `crate::memory` holds them.
const PAGE: u64 = 1 << 16;
/// The biggest structure built (a bottom level's triangles take 2.25 times
/// its bytes); a bigger one is left to the title's own walk.
const MOST: u64 = 32 << 20;
/// Statistics slots (32 dwords each); slot 0 takes the walks not checked.
const SLOTS: usize = 4096;
const SLOT_WORDS: usize = 32;
/// Frames after a walk its statistics are read.
const DUE: u64 = 8;
/// Frames a structure no longer named is kept for work in flight.
const BURY: u64 = 16;
/// Structures left for a later list (pages not there): tried again at most
/// so many at a time, for so many frames.
const RETRIES: usize = 16;
const PATIENCE: u64 = 60;

/// What a builder's call asks of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// Build the structure at `dest` from its `bytes` of title memory.
    Build {
        dest: u64,
        bytes: u64,
        top: bool,
        flags: u32,
    },
    /// The title copied the structure at `source` to `dest`.
    Copy { dest: u64, source: u64 },
}

/// Whether walks are checked (`VICTORY_RT_VALIDATE=1`).
fn validate() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RT_VALIDATE").is_some_and(|v| v == "1"))
}

/// A host structure and what builds it.
struct Built {
    top: bool,
    /// Nodes of 64 bytes the buffers are made for (0: a copy's).
    capacity: u32,
    /// The title's bytes, what the walk writes, its queue.
    staging: Option<ID3D12Resource>,
    out: Option<ID3D12Resource>,
    queue: Option<ID3D12Resource>,
    scratch: Option<ID3D12Resource>,
    /// The host structure (named by `va`; held here).
    #[allow(dead_code)]
    result: ID3D12Resource,
    result_bytes: u64,
    va: u64,
    bytes: u64,
}
// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Built {}
// SAFETY: as above.
unsafe impl Sync for Built {}

struct Entry {
    built: Arc<Built>,
    /// Built at least once.
    ready: bool,
    /// A top level: the bottom levels its instances may name.
    uses: Vec<Arc<Built>>,
    /// Left for a later list: (bytes, flags, frame first left).
    pending: Option<(u64, u32, u64)>,
}

/// A walk whose statistics are to be read.
#[derive(Clone, Copy)]
struct Check {
    frame: u64,
    dest: u64,
    top: bool,
}

#[derive(Default)]
struct Report {
    from: Option<std::time::Instant>,
    builds: [u64; 2],
    copies: u64,
    copies_unknown: u64,
    left: u64,
    retried: u64,
    given_up: u64,
    made: u64,
    failed: u64,
    too_big: u64,
    micros: u64,
    segments: u64,
    bytes_copied: u64,
    // Validation.
    checked: [u64; 2],
    triangles_match: u64,
    triangles: u64,
    elements: u64,
    bounds_match: u64,
    errors: u64,
    overflow: u64,
    unfinished: u64,
    instances: u64,
    unknown: u64,
    lines: u64,
}

struct Pass {
    root: ID3D12RootSignature,
    gather: ID3D12PipelineState,
    extract: ID3D12PipelineState,
    stats: ID3D12Resource,
    stats_data: *mut u32,
    /// A map of no entries, for bottom levels.
    empty: ID3D12Resource,
}
// SAFETY: D3D12 objects are free-threaded; the mapping is written under the
// state's lock.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

#[derive(Default)]
struct State {
    structures: HashMap<u64, Entry>,
    /// Kept for work in flight: the frame let go.
    buried: Vec<(u64, Arc<Built>)>,
    checks: Vec<Option<Check>>,
    slot: usize,
    report: Report,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn pass() -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| match make() {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, "rt hardware: no structure walk of ours; host structures are not built");
            None
        }
    })
    .as_ref()
}

fn make() -> windows::core::Result<Pass> {
    let host = host().ok_or_else(windows::core::Error::empty)?;
    let device = &host.device;
    let gather = crate::host::compile(SOURCE, s!("gather"), s!("cs_5_1"))?;
    let extract = crate::host::compile(SOURCE, s!("extract"), s!("cs_5_1"))?;
    // SAFETY: the root signature is in the shader; plain creations.
    let root: ID3D12RootSignature = unsafe { device.CreateRootSignature(0, &gather)? };
    let pipeline = |code: &[u8]| -> windows::core::Result<ID3D12PipelineState> {
        let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
            // SAFETY: the same pointer, not counted, for the call.
            pRootSignature: unsafe { std::mem::transmute_copy(&root) },
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: code.as_ptr().cast(),
                BytecodeLength: code.len(),
            },
            ..Default::default()
        };
        // SAFETY: a plain creation.
        unsafe { device.CreateComputePipelineState(&desc) }
    };
    let gather = pipeline(&gather)?;
    let extract = pipeline(&extract)?;
    let (stats, data) = host
        .shared_buffer((SLOTS * SLOT_WORDS * 4) as u64)
        .ok_or_else(windows::core::Error::empty)?;
    crate::host::name(&stats, || "rt hardware: walk statistics".into());
    let stats_data = data.cast::<u32>();
    // SAFETY: a fresh mapping of that many bytes.
    unsafe { std::ptr::write_bytes(stats_data, 0, SLOTS * SLOT_WORDS) };
    let (empty, data) = host
        .shared_buffer(64)
        .ok_or_else(windows::core::Error::empty)?;
    // SAFETY: as above.
    unsafe { std::ptr::write_bytes(data, 0xff, 64) };
    tracing::info!(
        validate = validate(),
        "rt hardware: host structures are built from the title's (stage b; traversal stays the title's)"
    );
    Ok(Pass {
        root,
        gather,
        extract,
        stats,
        stats_data,
        empty,
    })
}

/// A device-local buffer of `bytes`, writable by shaders, in `state`.
fn buffer(bytes: u64, state: D3D12_RESOURCE_STATES) -> Option<ID3D12Resource> {
    let host = host()?;
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: bytes.max(256).next_multiple_of(256),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: plain creation of a buffer.
    let r = unsafe {
        host.device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &desc,
            state,
            None,
            &mut made,
        )
    };
    if let Err(e) = r {
        tracing::warn!(error = %e, bytes, "rt hardware: no buffer");
        return None;
    }
    made
}

fn gpu(r: &ID3D12Resource) -> u64 {
    // SAFETY: a live buffer.
    unsafe { r.GetGPUVirtualAddress() }
}

/// The inputs of the host build of a structure of `capacity` nodes, at
/// `data` (triangles or instance descriptions); `geometry` is filled for a
/// bottom level and must outlive the inputs.
fn inputs(
    top: bool,
    capacity: u32,
    data: u64,
    flags: u32,
    geometry: &mut D3D12_RAYTRACING_GEOMETRY_DESC,
) -> D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
    // The title's preference for fast tracing or fast building; no updates
    // (each build is whole) and no compaction (copies clone).
    let flags = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS((flags & 0xc) as i32);
    if top {
        return D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: flags,
            NumDescs: capacity,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                InstanceDescs: data,
            },
        };
    }
    *geometry = D3D12_RAYTRACING_GEOMETRY_DESC {
        Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
        Flags: D3D12_RAYTRACING_GEOMETRY_FLAG_NONE,
        Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
            Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                Transform3x4: 0,
                IndexFormat: DXGI_FORMAT_UNKNOWN,
                VertexFormat: DXGI_FORMAT_R32G32B32_FLOAT,
                IndexCount: 0,
                VertexCount: capacity * 4 * 3,
                IndexBuffer: 0,
                VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                    StartAddress: data,
                    StrideInBytes: 12,
                },
            },
        },
    };
    D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
        Flags: flags,
        NumDescs: 1,
        DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
        Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
            pGeometryDescs: geometry,
        },
    }
}

impl Built {
    /// The buffers of a structure of `capacity` nodes.
    fn new(top: bool, capacity: u32, flags: u32) -> Option<Built> {
        let device: ID3D12Device5 = host()?.device.cast().ok()?;
        let out_bytes = if top {
            u64::from(capacity) * 64
        } else {
            u64::from(capacity) * 4 * 36
        };
        let out = buffer(out_bytes, D3D12_RESOURCE_STATE_COMMON)?;
        let mut geometry = D3D12_RAYTRACING_GEOMETRY_DESC::default();
        let x = inputs(top, capacity, gpu(&out), flags, &mut geometry);
        let mut info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        // SAFETY: inputs whose geometry lives through the call.
        unsafe { device.GetRaytracingAccelerationStructurePrebuildInfo(&x, &mut info) };
        if info.ResultDataMaxSizeInBytes == 0 {
            return None;
        }
        let staging = buffer(u64::from(capacity) * 64, D3D12_RESOURCE_STATE_COMMON)?;
        let queue = buffer((u64::from(capacity) + 1) * 4, D3D12_RESOURCE_STATE_COMMON)?;
        let scratch = buffer(info.ScratchDataSizeInBytes, D3D12_RESOURCE_STATE_COMMON)?;
        let result = buffer(
            info.ResultDataMaxSizeInBytes,
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        )?;
        crate::host::name(&result, || {
            format!(
                "rt hardware: host {} level of {capacity} nodes",
                if top { "top" } else { "bottom" }
            )
        });
        let bytes = u64::from(capacity) * (64 + 4)
            + 4
            + out_bytes
            + info.ScratchDataSizeInBytes
            + info.ResultDataMaxSizeInBytes;
        Some(Built {
            top,
            capacity,
            va: gpu(&result),
            staging: Some(staging),
            out: Some(out),
            queue: Some(queue),
            scratch: Some(scratch),
            result,
            result_bytes: info.ResultDataMaxSizeInBytes,
            bytes,
        })
    }

    /// The structure a copy of `from` goes to.
    fn copy_of(from: &Built) -> Option<Built> {
        let result = buffer(
            from.result_bytes,
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        )?;
        Some(Built {
            top: from.top,
            capacity: 0,
            va: gpu(&result),
            staging: None,
            out: None,
            queue: None,
            scratch: None,
            result,
            result_bytes: from.result_bytes,
            bytes: from.result_bytes,
        })
    }
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        ..Default::default()
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

fn turn(
    list: &ID3D12GraphicsCommandList,
    buffer: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // SAFETY: the same pointer, not counted; the buffer outlives
                // the call.
                pResource: unsafe { std::mem::transmute_copy(buffer) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    };
    // SAFETY: an open list.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Where title bytes `[at, at + bytes)` are on the card: pieces of (address,
/// offset in the structure's copy, bytes), each within one buffer, as far
/// as pages have copies (a bound may reach past the structure, into pages
/// nothing wrote), and how far that is. `None` if the first page has none
/// (yet). The buffers go into `keep`.
fn pieces(
    at: u64,
    bytes: u64,
    keep: &mut Vec<ID3D12Resource>,
) -> Option<(Vec<(u64, u64, u64)>, u64)> {
    let mut out: Vec<(u64, u64, u64)> = Vec::new();
    let mut done = 0;
    while done < bytes {
        let address = at + done;
        let len = ((address / PAGE + 1) * PAGE - address).min(bytes - done);
        let Some((shadow, offset)) = crate::memory::card_page(address) else {
            break;
        };
        let o = u64::from(offset.wrapping_add((address % PAGE) as u32));
        // SAFETY: a live buffer.
        let width = unsafe { shadow.buffer.GetDesc() }.Width;
        if o + len > width {
            break;
        }
        let from = gpu(&shadow.buffer) + o;
        match out.last_mut() {
            Some(last) if last.0 + last.2 == from => last.2 += len,
            _ => {
                out.push((from, done, len));
                keep.push(shadow.buffer.clone());
            }
        }
        done += len;
    }
    (done > 0).then_some((out, done))
}

/// Sets the pass's root parameters: constants, u0-u3, t0.
///
/// # Safety
/// An open list; live buffers.
unsafe fn bind(list: &ID3D12GraphicsCommandList, constants: &[u32; 8], u: [u64; 4], t: u64) {
    // SAFETY: as the caller promises.
    unsafe {
        list.SetComputeRoot32BitConstants(0, 8, constants.as_ptr().cast(), 0);
        for (k, &a) in u.iter().enumerate() {
            list.SetComputeRootUnorderedAccessView(1 + k as u32, a);
        }
        list.SetComputeRootShaderResourceView(5, t);
    }
}

impl State {
    fn new() -> State {
        State {
            checks: vec![None; SLOTS],
            slot: 1,
            ..State::default()
        }
    }

    fn bury(&mut self, built: Arc<Built>, frame: u64) {
        self.buried.push((frame, built));
    }

    /// A statistics slot for a walk of `dest` (0: not checked), set up.
    fn slot(&mut self, pass: &Pass, dest: u64, top: bool, frame: u64) -> u32 {
        if !validate() {
            return 0;
        }
        let n = self.slot;
        if self.checks[n].is_some() {
            return 0;
        }
        self.slot = if n + 1 >= SLOTS { 1 } else { n + 1 };
        self.checks[n] = Some(Check { frame, dest, top });
        // SAFETY: a slot of the mapping, not in use on the card (its last
        // walk was read).
        unsafe {
            let s = pass.stats_data.add(n * SLOT_WORDS);
            std::ptr::write_bytes(s, 0, SLOT_WORDS);
            for k in 0..3 {
                s.add(8 + k).write_volatile(u32::MAX);
            }
        }
        n as u32
    }

    /// Reads the statistics of walks that are due.
    fn harvest(&mut self, pass: &Pass, frame: u64) {
        for n in 1..SLOTS {
            let Some(c) = self.checks[n] else { continue };
            if c.frame + DUE > frame {
                continue;
            }
            self.checks[n] = None;
            // SAFETY: a slot of the mapping; its walk ran frames ago.
            let w: [u32; SLOT_WORDS] = unsafe {
                std::array::from_fn(|k| pass.stats_data.add(n * SLOT_WORDS + k).read_volatile())
            };
            let r = &mut self.report;
            r.checked[usize::from(c.top)] += 1;
            r.errors += u64::from(w[4]);
            r.overflow += u64::from(w[5]);
            r.unfinished += u64::from(w[6] != 0);
            let header = &w[16..32];
            let unordered = |b: u32| {
                f32::from_bits(if b & 0x8000_0000 != 0 {
                    b & 0x7fff_ffff
                } else {
                    !b
                })
            };
            let found: [f32; 6] = std::array::from_fn(|k| unordered(w[8 + k]));
            let said: [f32; 6] = std::array::from_fn(|k| f32::from_bits(header[4 + k]));
            let near = |a: f32, b: f32| (a - b).abs() <= 1e-3 * a.abs().max(b.abs()).max(1.0);
            let bounds = (0..6).all(|k| near(found[k], said[k]));
            if c.top {
                r.instances += u64::from(w[2]);
                r.unknown += u64::from(w[3]);
                r.bounds_match += u64::from(bounds);
            } else {
                r.triangles += u64::from(w[0]);
                r.elements += u64::from(header[3]);
                r.triangles_match += u64::from(w[0] == header[3]);
                r.bounds_match += u64::from(bounds);
            }
            if (w[0] != header[3] && !c.top || !bounds || w[4] != 0 || w[5] != 0 || w[6] != 0)
                && r.lines < 24
            {
                r.lines += 1;
                tracing::info!(
                    "rt hardware check: {} level {:#x} (frame {}): triangles {} instances {} (unknown {}) boxes {}, header elements {}, \
                     errors {} overflow {} left {} limit {}; bounds found {found:?}, header {said:?}; header {:x?}",
                    if c.top { "top" } else { "bottom" },
                    c.dest,
                    c.frame,
                    w[0],
                    w[2],
                    w[3],
                    w[1],
                    header[3],
                    w[4],
                    w[5],
                    w[6],
                    w[7],
                    &header[..4]
                );
            }
        }
    }

    fn report(&mut self) {
        let now = std::time::Instant::now();
        let since = *self.report.from.get_or_insert(now);
        if now.duration_since(since).as_secs_f64() < 10.0 {
            return;
        }
        let held: u64 = self.structures.values().map(|e| e.built.bytes).sum();
        let r = &self.report;
        tracing::info!(
            "rt hardware (10 s): built {} bottom, {} top; copies {} (of unknown structures {}); left for later {}, tried again {}, given up {}; \
             new host structures {} (failed {}, too big {}); this thread {:.1} ms, {} pieces, {} MiB copied; held {} structures, {} MiB, buried {}; \
             checked {} bottom: triangles as the header says {}, triangles {} of {}, bounds as the header's {}; {} top: instances {}, unknown {}; \
             errors {}, overflow {}, unfinished {}",
            r.builds[0],
            r.builds[1],
            r.copies,
            r.copies_unknown,
            r.left,
            r.retried,
            r.given_up,
            r.made,
            r.failed,
            r.too_big,
            r.micros as f64 / 1000.0,
            r.segments,
            r.bytes_copied >> 20,
            self.structures.len(),
            held >> 20,
            self.buried.len(),
            r.checked[0],
            r.triangles_match,
            r.triangles,
            r.elements,
            r.bounds_match,
            r.checked[1],
            r.instances,
            r.unknown,
            r.errors,
            r.overflow,
            r.unfinished,
        );
        let lines = self.report.lines;
        self.report = Report {
            from: Some(now),
            lines,
            ..Report::default()
        };
    }

    /// Builds the structure at `dest`; false if its pages are not there yet.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        pass: &Pass,
        host: &HostList,
        list4: &ID3D12GraphicsCommandList4,
        dest: u64,
        bytes: u64,
        top: bool,
        flags: u32,
        frame: u64,
    ) -> bool {
        if bytes > MOST {
            self.report.too_big += 1;
            return true;
        }
        let bytes = bytes.max(64).next_multiple_of(64);
        let mut keep = Vec::new();
        let Some((pieces, copied)) = pieces(dest, bytes, &mut keep) else {
            return false;
        };
        let capacity = (bytes / 64) as u32;
        // The structure's buffers: its own again if they fit.
        let reuse = self
            .structures
            .get(&dest)
            .is_some_and(|e| e.built.top == top && e.built.capacity >= capacity);
        if !reuse {
            let Some(built) = Built::new(top, capacity, flags) else {
                self.report.failed += 1;
                return true;
            };
            self.report.made += 1;
            let entry = Entry {
                built: Arc::new(built),
                ready: false,
                uses: Vec::new(),
                pending: None,
            };
            if let Some(old) = self.structures.insert(dest, entry) {
                // (The bottom levels a top level named are buried, if
                // replaced, themselves.)
                self.bury(old.built, frame);
            }
        }
        // A top level's map of bottom levels by title address.
        let mut map = None;
        let mut entries = 0u32;
        let mut uses = Vec::new();
        if top {
            let mut known: Vec<(u32, u64)> = self
                .structures
                .iter()
                .filter(|(a, e)| e.ready && !e.built.top && a.is_multiple_of(256) && *a >> 40 == 0)
                .map(|(a, e)| {
                    uses.push(e.built.clone());
                    ((a >> 8) as u32, e.built.va)
                })
                .collect();
            known.sort_unstable();
            entries = known.len() as u32;
            let Some((m, data)) =
                crate::host::host().and_then(|h| h.shared_buffer((known.len().max(1) * 12) as u64))
            else {
                self.report.failed += 1;
                return true;
            };
            let mut words = Vec::with_capacity(known.len() * 3);
            for (k, va) in &known {
                words.extend([*k, *va as u32, (*va >> 32) as u32]);
            }
            // SAFETY: a fresh mapping of at least that many bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(words.as_ptr(), data.cast::<u32>(), words.len())
            };
            host.keep(&m);
            map = Some(m);
        }
        let slot = self.slot(pass, dest, top, frame);
        let Some(entry) = self.structures.get_mut(&dest) else {
            return true;
        };
        let b = entry.built.clone();
        let (Some(staging), Some(out), Some(queue), Some(scratch)) =
            (&b.staging, &b.out, &b.queue, &b.scratch)
        else {
            return true;
        };
        let l = &host.list;
        let stats = gpu(&pass.stats);
        let t = map.as_ref().map_or(gpu(&pass.empty), gpu);
        // SAFETY: an open list; live buffers, kept until it has run (the
        // title's copies by the list, ours by the state).
        unsafe {
            l.SetComputeRootSignature(&pass.root);
            l.SetPipelineState(&pass.gather);
            for &(from, at, len) in &pieces {
                let words = (len / 4) as u32;
                bind(
                    l,
                    &[words, 0, 0, 0, 0, 0, 0, 0],
                    [from, gpu(staging) + at, gpu(queue), stats],
                    t,
                );
                l.Dispatch(words.div_ceil(256), 1, 1);
            }
            uav_barrier(l);
            l.SetPipelineState(&pass.extract);
            bind(
                l,
                &[
                    0,
                    u32::from(top),
                    b.capacity,
                    entries,
                    slot,
                    b.capacity,
                    (copied / 64) as u32,
                    0,
                ],
                [gpu(staging), gpu(out), gpu(queue), stats],
                t,
            );
            l.Dispatch(1, 1, 1);
            uav_barrier(l);
            turn(
                l,
                out,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
            let mut geometry = D3D12_RAYTRACING_GEOMETRY_DESC::default();
            let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                DestAccelerationStructureData: b.va,
                Inputs: inputs(top, b.capacity, gpu(out), flags, &mut geometry),
                SourceAccelerationStructureData: 0,
                ScratchAccelerationStructureData: gpu(scratch),
            };
            list4.BuildRaytracingAccelerationStructure(&desc, None);
            turn(
                l,
                out,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
            uav_barrier(l);
        }
        for k in &keep {
            host.keep(k);
        }
        entry.ready = true;
        entry.pending = None;
        if top {
            entry.uses = uses;
        }
        self.report.builds[usize::from(top)] += 1;
        self.report.segments += pieces.len() as u64;
        self.report.bytes_copied += copied;
        true
    }

    fn copy(
        &mut self,
        host: &HostList,
        list4: &ID3D12GraphicsCommandList4,
        dest: u64,
        source: u64,
        frame: u64,
    ) {
        self.report.copies += 1;
        let Some(from) = self
            .structures
            .get(&source)
            .filter(|e| e.ready)
            .map(|e| e.built.clone())
        else {
            self.report.copies_unknown += 1;
            return;
        };
        let reuse = self.structures.get(&dest).is_some_and(|e| {
            e.built.top == from.top
                && e.built.result_bytes >= from.result_bytes
                && !Arc::ptr_eq(&e.built, &from)
        });
        if !reuse {
            let Some(built) = Built::copy_of(&from) else {
                self.report.failed += 1;
                return;
            };
            self.report.made += 1;
            let entry = Entry {
                built: Arc::new(built),
                ready: false,
                uses: Vec::new(),
                pending: None,
            };
            if let Some(old) = self.structures.insert(dest, entry) {
                self.bury(old.built, frame);
            }
        }
        let source_uses = self
            .structures
            .get(&source)
            .map(|e| e.uses.clone())
            .unwrap_or_default();
        let Some(entry) = self.structures.get_mut(&dest) else {
            return;
        };
        // SAFETY: an open list; both structures live while it runs (the
        // state holds them, and buries them for frames after).
        unsafe {
            list4.CopyRaytracingAccelerationStructure(
                entry.built.va,
                from.va,
                D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE,
            );
        }
        uav_barrier(&host.list);
        entry.ready = true;
        entry.pending = None;
        entry.uses = source_uses;
    }

    fn run(&mut self, host: &HostList, jobs: &[Job]) {
        let started = std::time::Instant::now();
        let Some(pass) = pass() else { return };
        let Ok(list4) = host.list.cast::<ID3D12GraphicsCommandList4>() else {
            return;
        };
        let frame = crate::frame::current();
        self.buried.retain(|(f, _)| f + BURY > frame);
        self.harvest(pass, frame);
        // Structures left before, oldest first.
        let mut waiting: Vec<(u64, u64, u32, u64)> = self
            .structures
            .iter()
            .filter_map(|(&a, e)| e.pending.map(|(bytes, flags, f)| (a, bytes, flags, f)))
            .collect();
        waiting.sort_unstable_by_key(|w| w.3);
        for (dest, bytes, flags, first) in waiting.into_iter().take(RETRIES) {
            if jobs
                .iter()
                .any(|j| matches!(*j, Job::Build { dest: d, .. } if d == dest))
            {
                continue;
            }
            let top = self.structures.get(&dest).is_some_and(|e| e.built.top);
            self.report.retried += 1;
            if !self.build(pass, host, &list4, dest, bytes, top, flags, frame)
                && first + PATIENCE < frame
            {
                self.report.given_up += 1;
                if let Some(e) = self.structures.get_mut(&dest) {
                    e.pending = None;
                }
            }
        }
        for job in jobs {
            match *job {
                Job::Build {
                    dest,
                    bytes,
                    top,
                    flags,
                } => {
                    if !self.build(pass, host, &list4, dest, bytes, top, flags, frame) {
                        self.report.left += 1;
                        self.leave(dest, bytes, top, flags, frame);
                    }
                }
                Job::Copy { dest, source } => self.copy(host, &list4, dest, source, frame),
            }
        }
        self.report.micros += started.elapsed().as_micros() as u64;
        self.report();
    }

    /// The structure at `dest` waits for its pages: kept with the host
    /// structure it had (stale, but named), or made later.
    fn leave(&mut self, dest: u64, bytes: u64, top: bool, flags: u32, frame: u64) {
        if let Some(e) = self.structures.get_mut(&dest) {
            let first = e.pending.map_or(frame, |p| p.2);
            e.pending = Some((bytes, flags, first));
            return;
        }
        // Not made yet: made now (empty), so the retry knows its level.
        let Some(built) = Built::new(
            top,
            (bytes.clamp(64, MOST).next_multiple_of(64) / 64) as u32,
            flags,
        ) else {
            self.report.failed += 1;
            return;
        };
        self.report.made += 1;
        self.structures.insert(
            dest,
            Entry {
                built: Arc::new(built),
                ready: false,
                uses: Vec::new(),
                pending: Some((bytes, flags, frame)),
            },
        );
    }
}

/// Runs `jobs` in the title list `list` at the place it is at now, after
/// the builder's own work there.
pub(crate) fn queue(list: *const crate::object::Object, jobs: Vec<Job>) {
    if jobs.is_empty() || pass().is_none() {
        return;
    }
    crate::external::in_order(list, move |l| {
        crate::list::with_state(l, |host, state| {
            let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
            guard.get_or_insert_with(State::new).run(host, &jobs);
            state.host_state_lost();
        });
    });
}

/// The host structure built for the title's structure at `address`, if
/// there is one: its address on the card (for the traversal, stage (c)).
#[allow(dead_code)]
pub(crate) fn host_structure(address: u64) -> Option<u64> {
    let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()?
        .structures
        .get(&address)
        .filter(|e| e.ready)
        .map(|e| e.built.va)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk's choice of vertices (`rtbuild.hlsl`, `emit_triangle`),
    /// in Rust: the same table and arithmetic.
    fn walk_vertices(words: &[u32; 28], k: u32) -> [[f32; 3]; 3] {
        let map = [0x210u32, 0x231, 0x432, 0x042][k as usize];
        let id = words[15] >> (8 * k);
        let (i, j) = (id & 3, id >> 2 & 3);
        let first = 3u32.wrapping_sub(i).wrapping_sub(j) & 3;
        [first, i, j].map(|n| {
            let v = (map >> (4 * n) & 0xf) as usize;
            std::array::from_fn(|c| f32::from_bits(words[3 * v + c]))
        })
    }

    #[test]
    fn the_walk_takes_the_vertices_the_console_weighs() {
        let mut words = [0u32; 28];
        for (n, w) in words.iter_mut().enumerate().take(15) {
            *w = (n as f32 + 1.0).to_bits();
        }
        for k in 0..4 {
            for id in 0..16u32 {
                words[15] = id << (8 * k);
                assert_eq!(
                    walk_vertices(&words, k),
                    victory_shader::bvh::triangle_vertices(&words, k),
                    "triangle {k}, id {id:#x}"
                );
            }
        }
    }

    #[test]
    fn the_walk_compiles() {
        for entry in [s!("gather"), s!("extract")] {
            crate::host::compile(SOURCE, entry, s!("cs_5_1")).unwrap();
        }
    }
}
