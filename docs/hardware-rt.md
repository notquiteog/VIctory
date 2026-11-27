# Ray tracing on the host's ray-tracing hardware (`VICTORY_RT_HARDWARE`)

Status, 2026-11-27: **stage (a), the census**. Nothing changes what the
title draws yet. `VICTORY_RT_HARDWARE=1` exists as a switch and is part of
the shader cache's key, and nothing reads it beyond that.
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

(The census of the builder's calls at run time is in the next section.)

## Feasibility (interim verdict)

To come with the run-time census.
