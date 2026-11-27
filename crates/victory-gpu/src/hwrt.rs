//! The title's ray-tracing structures on the host's own ray tracing (DXR):
//! the census of what the title asks its structure builder for
//! (`VICTORY_RT_CENSUS=1`), and the switch of the hardware path
//! (`VICTORY_RT_HARDWARE=1`, off by default; docs/hardware-rt.md).
//!
//! The title's builder (`XboxBvhBuilder.dll`) is called through four of its
//! imports that `victory-hle`'s `modules::bvh` sees on the way:
//!
//! - `BuildRaytracingAccelerationStructureBatched2X(list, count, descs)`:
//!   `descs` are `count` records of 80 bytes ([`BATCHED`]): a
//!   `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC` (destination,
//!   inputs, source, scratch), then the count and address of post-build
//!   descriptions (the layout read off traced calls by Kemerd/VIctory-NVIDIA-RT,
//!   `bvh_builder.rs`; not public).
//! - `RehydrateRaytracingAccelerationStructureBatched2X(list, count, descs,
//!   extra)`: `count` plain build descriptions (48 bytes) of structures the
//!   title ships packed, unpacked in place, and one `u64` each at `extra`.
//! - `CopyRaytracingAccelerationStructure(list, dest, source, mode)`.
//! - `GetRaytracingAccelerationStructurePrebuildInfo(inputs, stream, info)`.
//!
//! The census reads what those descriptions name (CPU pointers of the title
//! into its own memory; the buffers they name are title GPU addresses, which
//! are the same addresses in this layer) and writes, for the first calls of
//! each kind, one line each, and every ten seconds the tallies. Nothing is
//! changed. Game data stays in the log (the user's), never in the repository.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Bytes of `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS`, of a
/// `D3D12_RAYTRACING_GEOMETRY_DESC`, of a build description, of the
/// batched record and of a `D3D12_RAYTRACING_INSTANCE_DESC`.
pub(crate) const INPUTS: usize = 24;
pub(crate) const GEOMETRY: usize = 56;
pub(crate) const BUILD: usize = 48;
pub(crate) const BATCHED: usize = 80;
pub(crate) const INSTANCE: usize = 64;

/// Calls of each kind written out one line each.
const DETAILED: u64 = 24;
/// Geometries and instances of one structure looked at, at most.
const LOOKED: u32 = 4096;

/// Whether the title's ray tracing runs on the host's ray tracing
/// (`VICTORY_RT_HARDWARE=1`; off by default until proven). Read once; every
/// crate asks [`victory_shader::rt_hardware`], so they cannot disagree.
pub fn hardware() -> bool {
    victory_shader::rt_hardware()
}

/// Whether the census is written (`VICTORY_RT_CENSUS=1`).
pub fn census() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RT_CENSUS").is_some_and(|v| v == "1"))
}

/// Whether the hle layer must see the builder's calls: the size bound, the
/// census or the hardware path wants them.
pub fn wanted() -> bool {
    crate::bvhsize::wanted() || census() || hardware()
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// A pointer of the title's CPU into its own memory.
fn pointer(p: u64) -> bool {
    (0x1_0000..1 << 47).contains(&p) && p % 4 == 0
}

/// Reads title memory: whether it was all there.
pub(crate) type Read<'a> = &'a dyn Fn(u64, &mut [u8]) -> bool;

/// The inputs of one build, as read.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Inputs {
    /// 0 top level, 1 bottom level.
    pub kind: u32,
    pub flags: u32,
    pub count: u32,
    /// 0 an array, 1 an array of pointers.
    pub layout: u32,
    pub at: u64,
    /// Bottom level: its geometries (those that read).
    pub geometries: Vec<Geometry>,
}

/// One `D3D12_RAYTRACING_GEOMETRY_DESC`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Geometry {
    /// 0 triangles, 1 boxes.
    pub kind: u32,
    pub flags: u32,
    pub transform: u64,
    pub index_format: u32,
    pub vertex_format: u32,
    pub index_count: u32,
    pub vertex_count: u32,
    pub index_buffer: u64,
    pub vertex_buffer: u64,
    pub vertex_stride: u64,
}

impl Geometry {
    pub(crate) fn parse(g: &[u8]) -> Self {
        let kind = u32_at(g, 0);
        if kind == 1 {
            // Boxes: count at 8, then the buffer and its stride.
            return Self {
                kind,
                flags: u32_at(g, 4),
                vertex_count: u64_at(g, 8) as u32,
                vertex_buffer: u64_at(g, 16),
                vertex_stride: u64_at(g, 24),
                ..Self::default()
            };
        }
        Self {
            kind,
            flags: u32_at(g, 4),
            transform: u64_at(g, 8),
            index_format: u32_at(g, 16),
            vertex_format: u32_at(g, 20),
            index_count: u32_at(g, 24),
            vertex_count: u32_at(g, 28),
            index_buffer: u64_at(g, 32),
            vertex_buffer: u64_at(g, 40),
            vertex_stride: u64_at(g, 48),
        }
    }
}

impl Inputs {
    /// The inputs at the start of `bytes`, and a bottom level's geometries
    /// read through `read`. `None` if these are not inputs.
    pub(crate) fn parse(bytes: &[u8], read: Read) -> Option<Self> {
        let mut x = Self {
            kind: u32_at(bytes, 0),
            flags: u32_at(bytes, 4),
            count: u32_at(bytes, 8),
            layout: u32_at(bytes, 12),
            at: u64_at(bytes, 16),
            geometries: Vec::new(),
        };
        if x.kind > 1 || x.layout > 1 || x.count > 1 << 24 {
            return None;
        }
        if x.kind == 1 && pointer(x.at) {
            for i in 0..u64::from(x.count.min(LOOKED)) {
                let desc = if x.layout == 0 {
                    x.at + i * GEOMETRY as u64
                } else {
                    let mut p = [0u8; 8];
                    if !read(x.at + i * 8, &mut p) {
                        break;
                    }
                    u64::from_le_bytes(p)
                };
                let mut g = [0u8; GEOMETRY];
                if !pointer(desc) || !read(desc, &mut g) {
                    break;
                }
                x.geometries.push(Geometry::parse(&g));
            }
        }
        Some(x)
    }
}

/// One `D3D12_RAYTRACING_INSTANCE_DESC`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Instance {
    pub transform: [f32; 12],
    pub id: u32,
    pub mask: u32,
    pub contribution: u32,
    pub flags: u32,
    pub structure: u64,
}

impl Instance {
    pub(crate) fn parse(b: &[u8]) -> Self {
        let mut transform = [0f32; 12];
        for (k, t) in transform.iter_mut().enumerate() {
            *t = f32::from_bits(u32_at(b, k * 4));
        }
        let (a, c) = (u32_at(b, 48), u32_at(b, 52));
        Self {
            transform,
            id: a & 0xff_ffff,
            mask: a >> 24,
            contribution: c & 0xff_ffff,
            flags: c >> 24,
            structure: u64_at(b, 56),
        }
    }
}

/// Tallies since the last report, and what is kept across them.
#[derive(Default)]
struct Census {
    from: Option<std::time::Instant>,
    calls: HashMap<&'static str, u64>,
    /// (kind, flags, layout) of builds.
    builds: HashMap<(u32, u32, u32), u64>,
    rehydrates: HashMap<(u32, u32), u64>,
    geometries: u64,
    /// (kind, index format, vertex format, stride).
    formats: HashMap<(u32, u32, u32, u64), u64>,
    transforms: u64,
    /// Geometries whose vertex (index) buffer read on the CPU, of those named.
    vertices_read: (u64, u64),
    indices_read: (u64, u64),
    instances: u64,
    /// Instances whose structure is one built or unpacked before, of all.
    instances_known: (u64, u64),
    copies: HashMap<u32, u64>,
    /// Over the whole run.
    detailed: HashMap<&'static str, u64>,
    built: HashSet<u64>,
    rehydrated: HashSet<u64>,
    /// Every copy's destination (compacted structures the instances name).
    copied: HashSet<u64>,
}

static CENSUS: Mutex<Option<Census>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Census) -> T) -> T {
    let mut guard = CENSUS.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Census::default))
}

impl Census {
    fn detail(&mut self, what: &'static str) -> bool {
        let n = self.detailed.entry(what).or_default();
        *n += 1;
        *n <= DETAILED
    }

    /// One build's inputs, `dest` its destination: tallied, and with `line`
    /// written out.
    fn inputs(&mut self, dest: u64, x: &Inputs, read: Read, line: bool, what: &str) {
        let readable = |at: u64| at != 0 && read(at, &mut [0u8; 16]);
        if x.kind == 1 {
            self.geometries += x.geometries.len() as u64;
            for g in &x.geometries {
                *self
                    .formats
                    .entry((g.kind, g.index_format, g.vertex_format, g.vertex_stride))
                    .or_default() += 1;
                self.transforms += u64::from(g.transform != 0);
                let v = readable(g.vertex_buffer);
                self.vertices_read.0 += u64::from(v);
                self.vertices_read.1 += 1;
                if g.index_buffer != 0 {
                    let i = readable(g.index_buffer);
                    self.indices_read.0 += u64::from(i);
                    self.indices_read.1 += 1;
                }
            }
            if line {
                let first: Vec<String> = x
                    .geometries
                    .iter()
                    .take(4)
                    .map(|g| {
                        format!(
                            "{{kind {} flags {:#x} transform {:#x} index fmt {} x{} at {:#x} vertex fmt {} x{} at {:#x} stride {} cpu {}}}",
                            g.kind,
                            g.flags,
                            g.transform,
                            g.index_format,
                            g.index_count,
                            g.index_buffer,
                            g.vertex_format,
                            g.vertex_count,
                            g.vertex_buffer,
                            g.vertex_stride,
                            readable(g.vertex_buffer)
                        )
                    })
                    .collect();
                tracing::info!(
                    "rt census: {what} dest {dest:#x}: bottom level, flags {:#x}, {} geometries (layout {}, at {:#x}, {} read): {}",
                    x.flags,
                    x.count,
                    x.layout,
                    x.at,
                    x.geometries.len(),
                    first.join(" ")
                );
            }
        } else {
            let mut known = 0;
            let mut seen = 0;
            let mut first = Vec::new();
            if pointer(x.at) || x.at >> 32 != 0 {
                for i in 0..u64::from(x.count.min(LOOKED)) {
                    let at = if x.layout == 0 {
                        x.at + i * INSTANCE as u64
                    } else {
                        let mut p = [0u8; 8];
                        if !read(x.at + i * 8, &mut p) {
                            break;
                        }
                        u64::from_le_bytes(p)
                    };
                    let mut b = [0u8; INSTANCE];
                    if !read(at, &mut b) {
                        break;
                    }
                    let inst = Instance::parse(&b);
                    seen += 1;
                    let k = self.built.contains(&inst.structure)
                        || self.rehydrated.contains(&inst.structure)
                        || self.copied.contains(&inst.structure);
                    known += u64::from(k);
                    if first.len() < 3 {
                        first.push(format!(
                            "{{id {} mask {:#x} contribution {} flags {:#x} structure {:#x} known {k} t ({:.2} {:.2} {:.2})}}",
                            inst.id,
                            inst.mask,
                            inst.contribution,
                            inst.flags,
                            inst.structure,
                            inst.transform[3],
                            inst.transform[7],
                            inst.transform[11]
                        ));
                    }
                }
            }
            self.instances += seen;
            self.instances_known.0 += known;
            self.instances_known.1 += seen;
            if line {
                tracing::info!(
                    "rt census: {what} dest {dest:#x}: top level, flags {:#x}, {} instances (layout {}, at {:#x}; {seen} read, {known} name a structure seen built, unpacked or copied): {}",
                    x.flags,
                    x.count,
                    x.layout,
                    x.at,
                    first.join(" ")
                );
            }
        }
    }

    fn report(&mut self) {
        let now = std::time::Instant::now();
        let since = *self.from.get_or_insert(now);
        if now.duration_since(since).as_secs_f64() < 10.0 {
            return;
        }
        let mut formats: Vec<_> = self
            .formats
            .iter()
            .map(|(k, n)| format!("{k:?}x{n}"))
            .collect();
        formats.sort();
        tracing::info!(
            "rt census (10 s): calls {:?}; builds (kind, flags, layout) {:?}; unpacked (inputs kind, geometries) {:?}; \
             geometries {} (kind, index fmt, vertex fmt, stride) {}; with a transform {}; vertex buffers read on the CPU {}/{}, \
             index buffers {}/{}; instances {}, naming a known structure {}/{}; copies by mode {:?}; structures seen built {}, unpacked {}, copied to {}",
            self.calls,
            self.builds,
            self.rehydrates,
            self.geometries,
            formats.join(" "),
            self.transforms,
            self.vertices_read.0,
            self.vertices_read.1,
            self.indices_read.0,
            self.indices_read.1,
            self.instances,
            self.instances_known.0,
            self.instances_known.1,
            self.copies,
            self.built.len(),
            self.rehydrated.len(),
            self.copied.len(),
        );
        let keep = (
            std::mem::take(&mut self.detailed),
            std::mem::take(&mut self.built),
            std::mem::take(&mut self.rehydrated),
            std::mem::take(&mut self.copied),
        );
        *self = Census::default();
        (self.detailed, self.built, self.rehydrated, self.copied) = keep;
        self.from = Some(now);
    }
}

#[cfg(windows)]
fn title_read(at: u64, bytes: &mut [u8]) -> bool {
    crate::host::read_into(at as *const u8, bytes)
}

/// Reads `n` records of `size` bytes at `at` (what is not there: zeros).
#[cfg(windows)]
fn records(at: u64, n: u32, size: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n.min(LOOKED) as usize * size];
    if !title_read(at, &mut bytes) {
        for (k, r) in bytes.chunks_mut(size).enumerate() {
            if !title_read(at + (k * size) as u64, r) {
                r.fill(0);
            }
        }
    }
    bytes
}

/// The title asks for a batch of `count` builds described at `descs`.
#[cfg(windows)]
pub fn batched(descs: u64, count: u32) {
    if !census() || !pointer(descs) {
        return;
    }
    let bytes = records(descs, count, BATCHED);
    with(|c| {
        *c.calls.entry("batched").or_default() += 1;
        let line = c.detail("batched");
        if line {
            tracing::info!(
                "rt census: batched build of {count} at {descs:#x}, frame {}, first record's last 32 bytes {:x?}",
                crate::frame::current(),
                &bytes.get(48..BATCHED).unwrap_or(&[])
            );
        }
        for (k, r) in bytes.chunks(BATCHED).enumerate() {
            let dest = u64_at(r, 0);
            let Some(x) = Inputs::parse(&r[8..8 + INPUTS], &title_read) else {
                continue;
            };
            *c.builds.entry((x.kind, x.flags, x.layout)).or_default() += 1;
            if x.kind == 1 {
                c.built.insert(dest);
            }
            c.inputs(dest, &x, &title_read, line && k < 4, "build");
            if line && k < 4 {
                tracing::info!(
                    "rt census:   record {k}: source {:#x} scratch {:#x} post-build count {} at {:#x}",
                    u64_at(r, 32),
                    u64_at(r, 40),
                    u32_at(r, 48),
                    u64_at(r, 56)
                );
            }
        }
        c.report();
    });
}

/// The title asks for `count` packed structures described at `descs` to be
/// unpacked in place, with one `u64` each at `extra`.
#[cfg(windows)]
pub fn rehydrate(descs: u64, count: u32, extra: u64) {
    if !census() || !pointer(descs) {
        return;
    }
    let bytes = records(descs, count, BUILD);
    let extras = if pointer(extra) {
        records(extra, count, 8)
    } else {
        Vec::new()
    };
    with(|c| {
        *c.calls.entry("rehydrate").or_default() += 1;
        let line = c.detail("rehydrate");
        if line {
            tracing::info!(
                "rt census: unpacking {count} at {descs:#x}, extra at {extra:#x}, frame {}",
                crate::frame::current()
            );
        }
        for (k, r) in bytes.chunks(BUILD).enumerate() {
            let dest = u64_at(r, 0);
            c.rehydrated.insert(dest);
            let Some(x) = Inputs::parse(&r[8..8 + INPUTS], &title_read) else {
                if line && k < 4 {
                    tracing::info!(
                        "rt census:   record {k}: dest {dest:#x}, not inputs: {:x?}",
                        &r[8..BUILD]
                    );
                }
                continue;
            };
            *c.rehydrates.entry((x.kind, x.count.min(64))).or_default() += 1;
            let mut header = [0u8; 64];
            let have = title_read(dest, &mut header);
            if line && k < 4 {
                tracing::info!(
                    "rt census:   record {k}: dest {dest:#x} source {:#x} scratch {:#x} extra {:#x}; header on the CPU {have}: {:x?}",
                    u64_at(r, 32),
                    u64_at(r, 40),
                    extras.get(k * 8..k * 8 + 8).map_or(0, u64_at_slice),
                    &header[..32]
                );
            }
            c.inputs(dest, &x, &title_read, line && k < 4, "unpack");
        }
        c.report();
    });
}

fn u64_at_slice(b: &[u8]) -> u64 {
    u64_at(b, 0)
}

/// The title copies the structure at `source` to `dest` (`mode`: 0 clone,
/// 1 compact, ...).
#[cfg(windows)]
pub fn copied(dest: u64, source: u64, mode: u32) {
    if !census() {
        return;
    }
    with(|c| {
        *c.calls.entry("copy").or_default() += 1;
        *c.copies.entry(mode).or_default() += 1;
        c.copied.insert(dest);
        if c.detail("copy") {
            tracing::info!(
                "rt census: copy {source:#x} -> {dest:#x} mode {mode} (source built in a batch: {}, unpacked: {})",
                c.built.contains(&source),
                c.rehydrated.contains(&source)
            );
        }
        c.report();
    });
}

/// The builder's prebuild answer for the inputs at `inputs`.
#[cfg(windows)]
pub fn prebuilt(inputs: u64, bound: u64) {
    if !census() || !pointer(inputs) {
        return;
    }
    with(|c| {
        *c.calls.entry("prebuild").or_default() += 1;
        if c.detail("prebuild") {
            let mut b = [0u8; INPUTS];
            if title_read(inputs, &mut b) {
                tracing::info!(
                    "rt census: prebuild kind {} flags {:#x} count {} layout {}: {bound:#x} bytes",
                    u32_at(&b, 0),
                    u32_at(&b, 4),
                    u32_at(&b, 8),
                    u32_at(&b, 12)
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry_bytes() -> Vec<u8> {
        let mut g = vec![0u8; GEOMETRY];
        g[0..4].copy_from_slice(&0u32.to_le_bytes());
        g[4..8].copy_from_slice(&1u32.to_le_bytes());
        g[16..20].copy_from_slice(&42u32.to_le_bytes()); // DXGI_FORMAT_R32_UINT
        g[20..24].copy_from_slice(&6u32.to_le_bytes()); // R32G32B32_FLOAT
        g[24..28].copy_from_slice(&36u32.to_le_bytes());
        g[28..32].copy_from_slice(&24u32.to_le_bytes());
        g[32..40].copy_from_slice(&0x1_0000_1000u64.to_le_bytes());
        g[40..48].copy_from_slice(&0x1_0000_2000u64.to_le_bytes());
        g[48..56].copy_from_slice(&12u64.to_le_bytes());
        g
    }

    #[test]
    fn a_bottom_level_reads_its_geometries() {
        let g = geometry_bytes();
        let mut inputs = vec![0u8; INPUTS];
        inputs[0..4].copy_from_slice(&1u32.to_le_bytes());
        inputs[8..12].copy_from_slice(&1u32.to_le_bytes());
        inputs[16..24].copy_from_slice(&0x2_0000u64.to_le_bytes());
        let read = |at: u64, b: &mut [u8]| {
            if at == 0x2_0000 && b.len() == GEOMETRY {
                b.copy_from_slice(&g);
                true
            } else {
                false
            }
        };
        let x = Inputs::parse(&inputs, &read).unwrap();
        assert_eq!(x.geometries.len(), 1);
        let g = x.geometries[0];
        assert_eq!(
            (
                g.index_format,
                g.vertex_format,
                g.index_count,
                g.vertex_count
            ),
            (42, 6, 36, 24)
        );
        assert_eq!(
            (g.index_buffer, g.vertex_buffer, g.vertex_stride),
            (0x1_0000_1000, 0x1_0000_2000, 12)
        );
    }

    #[test]
    fn not_inputs_are_refused() {
        let mut inputs = vec![0u8; INPUTS];
        inputs[0..4].copy_from_slice(&7u32.to_le_bytes());
        assert!(Inputs::parse(&inputs, &|_, _| false).is_none());
    }

    #[test]
    fn an_instance_record() {
        let mut b = vec![0u8; INSTANCE];
        b[0..4].copy_from_slice(&1f32.to_bits().to_le_bytes());
        b[48..52].copy_from_slice(&(0xff00_0007u32).to_le_bytes());
        b[52..56].copy_from_slice(&(0x0400_0002u32).to_le_bytes());
        b[56..64].copy_from_slice(&0x12_3400u64.to_le_bytes());
        let i = Instance::parse(&b);
        assert_eq!(
            (i.id, i.mask, i.contribution, i.flags, i.structure),
            (7, 0xff, 2, 4, 0x12_3400)
        );
        assert_eq!(i.transform[0], 1.0);
    }
}
