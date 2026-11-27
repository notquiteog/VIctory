# Ray tracing on the host's ray-tracing hardware (`VICTORY_RT_HARDWARE`)

Status, 2026-11-27: **stage (b), host structures**. Nothing changes what
the title draws yet. `VICTORY_RT_HARDWARE=1` (off by default, part of the
shader cache's key) builds host structures from the title's; the title's
own walks still trace.
`VICTORY_RT_CENSUS=1` logs what the title asks its structure builder for.
`VICTORY_WALK_CENSUS=<file>` with `victory translate` describes the title's
walks.

The aim is to port, not emulate. Today the title's ray tracing runs as the
title wrote it. Its builder writes structures in the console's node format
into title memory, and its trace programs walk them in a loop of node tests,
which we emulate in DXIL (docs/fork-ray-tracing.md). The port has two parts:

1. **Host structures.** At the builder's calls, build D3D12 bottom and top
   levels on the host device, and keep a map from each title structure
   address to its host structure.
2. **Host traversal.** Replace each walk the translator recognises with one
   inline ray query (DXR 1.1, shader model 6.5). The loop's results go into
   the registers the code after it reads. A walk that is not recognised
   stays the software walk.

## The census of the walks (static, from the title's shaders)

How to repeat it (game-derived output, keep it outside the repository):

```
set VICTORY_WALK_CENSUS=<scratch>\walks.txt
victory translate --jobs 4 <game>\shaders.rpf
```

With the census on, `translate` skips every program without a node test.
`crates/victory-shader/src/walk.rs` finds each natural loop that holds a node
test and records:

- its size;
- group-memory reads, writes and exchanges (the stack);
- loads (instance records, triangle data, materials);
- image operations other than the node test (alpha tests);
- cross-lane operations;
- exits, carried registers, and the registers read after it.

What it finds:

- **64 programs walk structures**, all compute: 57 with wave32 and 7 with
  wave64. They are the same 64 that docs/fork-ray-tracing.md counts (59
  translate; the 5 `CS_TraceShadowRays_*` programs pick their structure
  from the heap by a computed index). The builder's 52 programs have no
  node test.
- There are 717 loops around node tests in the SSA form. That counts the
  copies the IR makes to give loops one entry, and the walks the title
  inlined once per light or per tap: up to 36 in a `_Tiled` shadow program.
- **There are two loop shapes.** They match the title's own `_Shared` and
  `_Mixed` variants, and Kemerd's "two traversal loops".

  **Shape S ("shared"), 32-bit pointers, nested: 319 pairs in 46 programs.**
  - The outer loop walks the *top level* with one node pointer for the
    whole wave, held in an SGPR (`s_load` of the instance records).
  - Its stack lives in group memory and is indexed by an SGPR, with at most
    0x101 entries.
  - The children the lanes hit are merged across lanes with
    `v_readlane`, `v_writelane` and `v_max3`/`v_med3`/`v_min3`, which sort
    the four results.
  - Instance nodes (kind 6) transform the ray with the instance's 3x4
    matrix, read with scalar loads.
  - The inner loop walks a *bottom level* per lane:
    - a VGPR node pointer and a per-lane stack in group memory, popped
      with `ds_wrxchg_rtn_b32` at offset 1024;
    - the triangle results (numerator, denominator, I and J numerators);
    - and inside the loop an **alpha test** (`image_sample_lz` or
      `image_sample_l` on a texture). Its material comes from
      `global_load`s of the triangle node's spare bytes and of the
      geometry's records.
  - The inner loop carries 22-26 registers, and all of them are read after
    it: hit distance, barycentrics, the hit's instance and triangle node
    pointers, and the ray in the instance's space.
  - Programs: `CS_RTShadowTrace32*`, `CS_RTDistant*`, `CS_TraceShadowRays_*`
    (wave32), `CS_RaytraceVBuffer_*Shared*`, `CS_RaytraceShadows_Shared`,
    `CS_ScreenProbes_TraceRays_RayBinning_BVH_Shared_*`,
    `CS_DeferredGather_Shared_Wave32`, `CS_LaserLightShadowRaytracing`,
    `CS_ComputeClosestDepth`.

  **Shape M ("mixed"), 64-bit pointers, flat: 78 loops in 18 programs.**
  - One per-lane loop of `image_bvh64_intersect_ray` walks the top level
    and the bottom levels together. Instance nodes are entered in the loop
    itself, with its `global_load`s of the matrix.
  - The stack is the same group-memory stack with `ds_wrxchg_rtn_b32` pops.
  - Most have no image operation in the loop. Those with one sample alpha
    the same way as shape S.
  - The loop leaves 2-19 registers to the code after it, most often 6: the
    hit distance, the barycentrics, and the two node pointers.
  - Programs: `CS_*Gather*VBuffer*`, `CS_IrradianceCache_*`,
    `CS_PPR_Raytrace`, `CS_RaytraceVBuffer_*Mixed*`,
    `CS_RaytraceShadows_Mixed`, `CS_ScreenProbes_TraceRays_*BVH_Mixed*`,
    `CS_ScreenProbes_TraceRays_BVH_Shared_*` (wave64), `CS_DeferredGather_*`
    (wave64), `CS_TraceShadowRays_Full*` (wave64),
    `CS_CollisionRaytraced`.
- What the code after a walk does with the hit (seen in
  `CS_RTShadowTrace32HQ`): it takes the hit's *instance node pointer* and its
  *triangle node pointer*, both kept by the loop. It reads two dwords of the
  instance node in the top level: byte 52, of which it keeps a 14-bit field,
  and byte 60, the bottom level's address in 256-byte units. It then reads
  the dword at byte 48 of the triangle node in that bottom level, shifted
  right by 8. From those it finds the geometry's material and its vertex
  attributes. So a host query must give back title node pointers, not only
  DXR indices.

- **Shape M loops do more than find the nearest triangle.** In a small one
  (`CS_ScreenProbes_TraceRays_RayBinning_BVH_Mixed_*`, the third walk, with
  7 registers out and no image operation), each triangle the node test hits
  inside the ray's extent is looked up: the instance node's dword at byte 52
  (24 bits: the instance's contribution) plus the triangle node's dword at
  byte 48 shifted right by 8 index a table of material flags (a typed buffer
  load). The flags decide whether the hit is skipped, kept as the nearest
  hit, or kept apart as the nearest hit of another kind (its distance and a
  10-bit field of the triangle node). So even the loops without an alpha
  test have any-hit work per candidate, and a host query has to run that
  work on each candidate (non-opaque triangles, `Proceed` and
  `CommitNonOpaqueTriangleHit`) to give the same results.
- **Each loop is close to unique.** `walk.rs` gives each loop a fingerprint:
  its code with registers renumbered by first use. Of the 65 shape-M loops
  (in 18 programs) there are about 50 different fingerprints, and of all
  658 loops 458. The compiler inlined the walk into each caller, with the
  candidate work and what the caller keeps around it. So a recogniser
  cannot be a table of known loops; it has to match the loop's parts (the
  stack, the node test, the instance entry, the candidate work) and prove
  each register it leaves.

(The census of the builder's calls at run time is in the next section.)

## Feasibility (verdict)

- **Host structures (stage b) can be built from the title's own node
  data**, after the builder's work in each list. Building them from the
  title's D3D12 inputs (vertex and index buffers) would be simpler for the
  host, but then a host hit could not name the title's nodes, and the code
  after a walk reads them (above). With the node data, a hit's primitive
  index and instance index give the title's node pointers back by
  arithmetic, with no table (next section).
- **Host traversal (stage c) needs a recogniser per loop variant** that
  proves what each register the loop leaves means: the hit distance, the
  barycentrics, the two node pointers, the instance-space ray, the flags.
  Anything not proven keeps the software walk, per program.
- **On the host, "ran out" is always false.** Shape M loops count their
  iterations against a budget (`s_cmp_le_u32`) and leave a flag when the
  budget ran out before the walk finished; the code after them treats such
  a ray as a miss or as unknown. A host query always finishes, so the flag
  is always clear. That is the walk as the title meant it, not a change for
  the worse.
- **What it buys.** The frame is bound by the CPU (submit thread), so the
  hardware path does not reach 30 fps by itself. It removes the card's ray
  time (about 24 ms of card time with rays on) and the card's slow state
  that the software walks bring, and it costs the submit thread the host
  builds (measured below).

## Stage (b): host structures (`VICTORY_RT_HARDWARE=1`)

`crates/victory-gpu/src/rtbuild.rs` and `rtbuild.hlsl`. At each of the
builder's `Batched2X`, `Rehydrate...Batched2X` and `Copy...` calls
(`victory-hle`'s `modules::bvh`), after the builder's own work, in the
list's order:

1. **Copy out.** The structure's bytes are copied from where title memory
   lives on the card (`crate::memory::card_page`, read only), page by page,
   into a buffer of ours, as far as pages have copies. How many bytes: the
   builder's own prebuild answer for those inputs (`crate::bvhsize`), the
   header's node count for an unpacked structure (its header is on the CPU
   while packed), or a generous bound from the inputs. A structure whose
   first page has no copy yet is tried again in later lists, for up to 60
   frames.
2. **Walk.** One group of 1024 threads per structure walks it from the
   header's root, level by level (a queue of box nodes in a buffer).
   - A bottom level's triangle `k` of the triangle node at node index `n`
     goes to triangle `4n + k` of a non-indexed triangle list. Its vertices
     are ordered as the console's barycentrics weigh them: the vertex
     neither weighs, then I's, then J's. So the host's barycentrics are the
     title's I and J, and the host's primitive index `p` names the title's
     triangle node pointer `(p >> 2) << 3 | (p & 3)`. Slots no node points
     at are NaN: inactive triangles.
   - A top level's instance node at node index `n` becomes D3D12 instance
     description `n`. The first 64 bytes of an instance node are a D3D12
     instance description already (transform, id and mask, contribution and
     flags), except for the last 8 bytes: the bottom level's root pointer,
     then its address in 256-byte units. Those become the host address of
     the host bottom level built for that title address, found in a sorted
     table of ours. So the host's instance index `i` names the title's
     instance node pointer `i << 3 | 6`. An instance whose bottom level has
     no host structure gets mask 0 and no structure: inactive.
3. **Build.** One host build per structure, with the title's preference for
   fast tracing or fast building, no updates (each build is whole) and no
   compaction. A copy clones the host structure to one of its own.

Each structure keeps its buffers and builds into them again when it fits.
One it no longer names is kept for 16 frames for work in flight. Bigger
than 32 MiB is left alone (counted).

**Validation** (`VICTORY_RT_VALIDATE=1`): each walk's counts are read back
8 frames later and set against the title's header. For a bottom level the
triangles found are compared with the header's element count (dword 3), and
the bounds of their vertices with the header's bounds (dwords 4-9). For a top
level the instances and the ones naming no known bottom level are counted.
Errors (a pointer outside the structure, a kind that does not belong), queue
overflows and walks left unfinished are counted too. Every ten seconds the
log gets a line `rt hardware (10 s): ...`, with the time this work took on
the recording thread. The first disagreements get a line each
(`rt hardware check: ...`).

## Stage (c): host traversal (next)

Not started in code beyond the emitter (`emit/rayquery.rs`: the query's
operations, the top level at root SRV `t252 space1`, a test through the
validator). The plan, from what the census found:

1. **Split each shape-M loop into its parts** and prove each one: the
   stack (group-memory pops with `ds_wrxchg_rtn_b32`, pushes of the sorted
   children, the parent-pointer restart when the short stack is spent),
   the node test, the instance entry (kind 6: the mask test against the
   instance's byte 51, the inverse transform at bytes 64-111, the bottom
   level's address at byte 60), and the **candidate work**: the code that
   runs for a triangle the node test hit inside the ray's extent, with the
   distance, the barycentrics, the facing and the two node pointers as its
   inputs, and the closest-hit registers as its outputs.
2. **Lower the loop as one query**: `TraceRayInline` with the loop's ray
   (origin, direction, extent, the instance mask), then
   `while (Proceed())` running the title's own candidate work on the
   candidate's values (`CandidateTriangleRayT`, `...Barycentrics`,
   `...FrontFace`, the node pointers from `CandidateInstanceIndex` and
   `CandidatePrimitiveIndex` by the arithmetic of stage (b)), committing
   when it keeps the hit as the nearest; after it, the registers the loop
   leaves from the committed values. The budget's "ran out" is false.
3. **Which top level.** The loop reads the top level's address from its
   constants (64 bits); the host query needs that structure bound. Either
   one host top level per dispatch, found at the dispatch from the same
   constants, or a heap of host top levels indexed by a table lookup in the
   shader (shader model 6.6). `WithFar` programs walk two.
4. **Per program, all or nothing**: a program with any walk not proven keeps
   all its walks in software. First targets: `CS_IrradianceCache_ShadowTrace`
   (310 instructions, 3-4 registers out, a shadow ray that may end on the
   first hit), then the 329-instruction loops of `CS_ScreenProbes_*`,
   `CS_IrradianceCache_GatherVBuffer*` and `CS_Primary_GatherVBuffer*`
   (6 registers out; two fingerprints cover 8 of them), then
   `CS_RaytraceShadows_Mixed` and `CS_RaytraceVBuffer_Mixed`.
