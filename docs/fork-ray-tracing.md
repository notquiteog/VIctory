# Ray tracing in this fork: what the title does and what has been built

Status, 2026-11-26: **the title's own ray tracing is on by default** (the
structure builder runs unless `VICTORY_BVH_BUILDER=0`). See "On by default"
below for what the picture shows and what it costs. Without it,
**`VICTORY_AMBIENT=<share>`** puts an estimate of that light in its place for
next to nothing (below); the stand-in is off by itself while the builder has
run whole.

Before (2026-11-25): with `VICTORY_BVH_BUILDER=1` rooms got their indirect
light (runs light-10, light-11, n19), for some 40 ms more of the card's time a
frame on the RX 7900 XTX (the dorm: 75 of 90 ms busy against 34 of 64).

## On by default (2026-11-26, RTX card, Series S, render scale 0.5, walking)

Runs rt-on1/on2 (builder on, nothing skipped) against rt-off1 (builder off,
the 1034 pipelines of `rt_off_hashes.txt` skipped, `VICTORY_AMBIENT=0.8`),
captures in `captures\rt-on1`, `rt-on2`, `rt-off1` (not in the repository):

- The porch the opening stands on: off, under its roof everything is near
  black (wall, blinds, rail, the ladder); on, the wall, the blinds and the rail
  have their bounced light, the ladder is lit, and the sky has its clouds
  (their passes are in the skip list). The street at the end of the walk:
  the house front in shadow is dark in both, lighter with rays.
- No black, NaN or pink pixels, no light leaking or over-bright reflections
  seen. Blocks of grey tiles on the player and the rail in the first 100
  frames after the world appears (rt-on2 frame 1300), gone after: the probes
  settling while the first structures come. Rectangles in the water's
  reflections are there in both runs (not the rays').
- The log: the builder runs whole (53 pipelines) a few seconds after load,
  no "rests", no "stays off". Shaders still touch 100-500 pages a frame that
  have no copy (read as zero); most are refused (not committed title memory),
  as before.
- Time (frame medians; `GPU` is the bench's mean of the card's time a
  frame). Before the merge of main's two-stage line: off 67-70 ms (GPU 2-5),
  on 89-101 ms (GPU 7-22). On main b9d436df2 with the push below (runs
  rt-pon3, rt-poff3, rt-ppush03, one each, walking): on 86 ms (GPU 17), off
  73 ms (GPU 5), on with `VICTORY_BVH_PUSH=0` 78 ms (GPU 33). Runs of the
  walk differ by 10 ms and more from one to the next; the card is not what
  holds the frame (5-30 ms of it busy).
- The submitting thread (profiles, 120 s each): before the push, rays added
  18 ms a frame of work to it, 7 ms of it in the kernel's write watch
  (`GetWriteWatch`) for the copies of memory by address, the directory and
  the arenas refreshed whole for every list that reads them. With the push
  (`VICTORY_BVH_PUSH`, below) `GetWriteWatch` is 4.3 ms a frame against 8.6
  without it and 2.9 with rays off, and the thread's work with rays is some
  6 ms a frame above rays off (rt-pon3 against rt-poff3): 1.5-2 ms each in
  copying the title buffers the rays read (`Shadow::take`, `copy_stale`), the
  card's copies before a submission (`card_before`) and the write watch.
  The push itself costs the recording threads 0.02 ms a frame.
- Seen once (rt-pcap3 frame 2600, walking past the white house): a fan of
  small dark squares over the sky and the house front, gone in the frames
  around it; not explained yet.

What the skip list should become: nothing for ray tracing. `rt_off_hashes.txt`
is for `VICTORY_BVH_BUILDER=0` only (scripts\play.ps1 `-NoRayTracing`
gives both); with the builder on it would drop the passes that use the
structures it builds.

## What the title does

The title's indirect light (probes, bounce, reflections) and the
shadows of its lamps and sun are ray traced. Without them, anything the sun
does not hit directly is black: interiors, the shadowed side of a face, rooms
lit only by lamps.

The title does not use a ray-tracing API:

- Traversal is ordinary compute-shader code: a per-thread stack in shared
  memory, and loads through raw 40-bit addresses of title memory.
- The static world's acceleration structures ship pre-built in the title's
  data, in the console GPU's native node format (64-byte nodes: box nodes with
  four children and 16-bit float bounds, 128-byte box nodes with 32-bit float
  bounds, triangle nodes, instance nodes). They are unpacked on the GPU.
- A set of builder programs (from the title's `XboxBvhBuilder.dll`, 52 compute
  programs) builds and refits the dynamic structures on the GPU every frame.
- Only the per-node test is hardware: the instructions
  `image_bvh64_intersect_ray` and `image_bvh_intersect_ray`. One call tests a
  ray against a box node's four children (returning the hit children, nearest
  first) or against a triangle node (returning the intersection's numerator,
  denominator and barycentric numerators).

So mapping this onto the host's own ray tracing (DXR) is not practical: the
layer would have to know the geometry behind structures the title never
describes to it, and the traversal loops live in 64 of the title's own
programs. This fork runs the title's builder and traversal as translated
shaders and emulates the one instruction.

## What this fork adds

1. **The builder programs translate** (all 52, with host pipelines on the RX
   7900 XTX; the title creates 53 such pipelines). They pass buffer addresses in a spilled parameter table
   rather than in descriptors. Added: the spilled user-data table, root
   unordered-access views (translator and host binding), byte and 16-bit buffer
   accesses, and several control-flow shapes.
2. **Title memory by raw address from shaders.** A directory with one entry per
   64 KiB of title address space leads a shader to the layer's host copy of
   that memory; loads, stores, atomic add/sub and float min/max go through it.
   Before a dispatch the host makes ready the memory its root views and root
   constants name.
3. **The ray test, emulated in DXIL.** Four slab tests and a sorting network
   for box nodes; a watertight ray/triangle test for triangle nodes; both forms
   of the instruction. A plain Rust reference lives in
   `crates/victory-shader/src/bvh.rs`; tests run the emitted DXIL on the GPU
   over 311 ray/node cases and compare with the reference bit for bit.
4. **The trace and shading programs translate** (59 of 64 trace programs, and
   the irradiance-cache shading program). This needed lane masks treated as
   plain numbers on some paths, and exact forms for programs written for
   64-lane waves running on a GPU with 32-lane waves.
5. **A safety rule on the host side.** The builder's stages run only while
   every builder pipeline the title has created has a host pipeline; otherwise
   none run (`VICTORY_BVH_BUILDER=1` asks for it; `VICTORY_BVH_QUARANTINE=1`
   also keeps the builder's stores out of title memory, for a first test). A
   partly working builder wrote garbage bounds into title memory and crashed
   the title.

## The structures (from dumps of the arenas, `VICTORY_MEMORY_DUMP`)

- Header, 64 bytes: dword 0 the version 6 in the low half and marks in the high
  (0x8000 bottom level, 0x4 compacted, 0x2 rehydrated, 0x100 still packed),
  dword 1 the size in 64-byte nodes, 3 the number of elements, 4-9 the bounds,
  10 the offset of the box nodes, 12 the root's node pointer, 13 the offset of
  the elements, 14 the offset of the parent table (from the header; the static
  ones' index counts the leaves too, so it is negative).
- An instance is 128 bytes at the top level's offset 13: a 3x4 matrix in dwords
  0-11, id and mask, flags, the root pointer (bit 30 set for static ones), the
  bottom level's address in 256-byte units.
- The static world's structures are shipped **packed ("dehydrated")** and
  unpacked in place, once, by DecompressHeader, Box32/Box16Rehydration and
  LeafRehydration when they have been streamed in (3,000 in the opening).
- Dynamic ones are built, their compacted size read back by the title from a
  small buffer, and copied to a place of that size.

## How title memory by address is held (`memory.rs`)

Pages of 64 KiB in arenas on the card, whole title buffers in their own copies,
pages with small title buffers as windows of title memory. The module's
comment has the rules. In short: what the builder stores stays on the card
(only small title buffers it was given to write come back); a packed structure
goes up whole when the builder comes to it; title buffers take its stores only
after it was given them as an unordered access view.

| variable | does |
|---|---|
| `VICTORY_BVH_BUILDER=0` | the builder's stages do not run (on by default since 2026-11-26); skip `rt_off_hashes.txt` with it |
| `VICTORY_BVH_PUSH=0` | the directory and the arenas are refreshed whole, through the write watch, for every list that reads memory by address, as before (on: what this module writes there is pushed at once, the dwords that changed) |
| `VICTORY_BVH_FENCES=0` | no fences around the builder's buffer atomics (beloooo's 72e748ea has them too) |
| `VICTORY_BVH_SIZES_WAIT=1` | submissions that ask for structures' sizes are waited for (off: no help, slow) |
| `VICTORY_BVH_BUILDER_EARLY=0` | the builder's pipelines are made one after another at its first dispatch, as before (Known gaps) |
| `VICTORY_BVH_SIZES_BOUND=0` | a structure's compacted size is not given a bound before the card's lands (on with the builder; Known gaps) |
| `VICTORY_BVH_BOUNDS=0` | the builder's accesses are not bounded |
| `VICTORY_BVH_TRACE=1` | logs what sizes are asked, when they came, and the copies (diagnostics) |
| `VICTORY_BVH_QUARANTINE=1` | nothing it stores comes back to title memory |
| `VICTORY_BVH_NODE_ONCE=1` / `=2` | how a node test reads the node (unset or 0: each kind's dwords in that kind's branch). 1: the node's first 64 bytes at once, before the kind is looked at, with a triangle's vertices picked from them. 2: a triangle node's 64 bytes at once, in its own branch. On the RTX card, 1 made the trace passes 3-4x slower (register pressure), and 2 was within noise |
| `VICTORY_FLOAT_ATOMIC_INTS=0` | a float atomic minimum or maximum on title memory is a loop of compare-exchanges, as before (on: one integer atomic on its bits) |
| `VICTORY_MEMORY_CARD_MB` | the most of it on the card (4096) |
| `VICTORY_MEMORY_DUMP=<folder>` | a file `go` there writes the arenas out (diagnostics) |
| `VICTORY_UPLOAD_LOW=0`, `VICTORY_LENT_HEAP=0` | lists' upload memory above 2^40, no view heap for lists without one, as before |

Every ten seconds the log has a line "title memory by address: ..." with pages,
dispatches, faults and what came.

## Known gaps

- Sparse tree canopy on the walk (2026-11-26, runs rt-pcap3, rt-aon2,
  rt-apush02 against rt-aon3, rt-askip1, rt-off1, rt-aoff3, rt-aoffns1; world
  captures every 20 frames, `VICTORY_CAPTURE_WORLD_EVERY=20`). Where the walk
  passes the side of the blue house, the tree in front of the white house is
  drawn as a fan of small dark squares over the sky and the house instead of a
  full canopy, for some 150 frames. It is not the push: rt-apush02
  (`VICTORY_BVH_PUSH=0`) has it too. It came only with the builder on and
  rays traced (3 of the 4 such runs that reached the spot; rt-aon3 drew the
  tree whole). None of the 4 runs without traced rays had it: builder off with
  the skip list, builder off without it (rays find nothing), and builder on
  with the ray passes skipped. So it is likely tied to ray tracing, but it
  does not come every time. The other half of the tree draws whole in the same
  frame, so it looks like one instance's level of detail or fade-in stuck
  partway. Not mended yet.
- Grey tiles on the player and the rail just after the cutscene cuts to play
  (rt-on2 `frame-1300.png`: flat grey and dark squares of 16 rendered pixels
  on the shirt, legs and rail, and lighter rectangles on the water). Seen once
  in 6 builder-on runs. rt-aon3 captured every 2 frames from the world's first
  frame and every 5 frames from 180 to 440 after it, and its first frames of
  play are clean, apart from the rectangles on the water, which all the runs
  show. Faults right after the world appears are bounded per frame
  (`FAULTS`=128). The 900-odd pages of the first burst take two frames, so
  that burst comes and goes some 250 frames before the cut. Not reproduced, so
  not mended.
- Dynamic structures overlap each other (8 of 65 good in run light-10, 48 of
  275 in light-15). Part of the cause, run light-11's trace (`VICTORY_BVH_TRACE=1`): the title asks for a new
  structure's compacted size, reads it one or two frames later without looking
  whether it is there, takes memory of that size and has the structure copied
  into it. Our submissions land up to two frames late; the title read what the
  slot held before, another structure's size, and the copy wrote over the
  neighbours. Waiting for a submission that asks (`VICTORY_BVH_SIZES_WAIT=1`)
  did not mend it and costs some 20 ms a frame (light-15): the title asks in
  every frame, and what it read was as old as before.

  Why the wait could not help (2026-11-26, from the code): the asking is the
  builder's one-thread program `GetBVHCompactedSize`, recorded by
  `XboxBvhBuilder::BuildRaytracingAccelerationStructureBatched2X` (the only
  build call the title imports). With `VICTORY_THIN=1` the list is translated
  by threads of ours and sent by our line's thread (`queue/behind.rs`), and
  the wait (`memory::asked_sizes`, `Host::send_late`) blocks that thread, not
  the title's: the title goes on and reads the slot all the same. Nor can the
  CPU know the size from the structure's header (dword 1): the builder writes
  it on the card, in an arena, and it never comes to title memory.

  Mended, untested in a run: **`VICTORY_BVH_SIZES_BOUND`** (on with the
  builder; `0`: off), `crates/victory-gpu/src/bvhsize.rs`. The main image's
  imports of the builder's `GetRaytracingAccelerationStructurePrebuildInfo`
  and of the batched build go through `victory-hle`'s `modules/bvh.rs`
  first. A prebuild answer (`ResultDataMaxSizeInBytes`, at least the
  compacted size) is kept by what it depends on (kind, flags, counts and
  formats of the inputs); a batched build's descriptions are looked through
  for build descriptions (destination, then inputs) whose inputs were
  answered (the batch's own description is not public: in place or pointed
  at). When the size program is translated, its slot is given that bound in
  title memory at once, so a read before the card's size lands takes memory
  enough (more than it needs, until then). A bottom-of-pipe look on the
  asking list (`Written::SizeLanded`) puts the latest bound back where an
  earlier asking of the same slot landed after it. What is not found is left
  as before. Every ten seconds the log says how many answers, structures
  found, sizes asked with a bound and without, bounds put back.

  The first run with it (bench bvh-bound1, 2026-11-26) ended in the title's
  watchdog (`mov [0]` at +0x23cf683, 30 s after frame 1064 began), not in
  anything the bound did: no bound was ever given (no size program had been
  translated yet). The batches' bounds were right: 0x1cc00, the prebuild
  answer, is the spacing of the title's own uncompacted destinations in the
  same batch. What held the frame was the builder's first dispatch, which
  made its 53 pipelines one after another on its thread: 8 s with a warm
  driver cache in the next run (its frame 1053 took 13 s), about a second
  each with a cold one, 40 made when the watchdog came. Now they are queued
  for the workers when the title creates them, and all at once at that
  first dispatch (`VICTORY_BVH_BUILDER_EARLY=0`: as before). A bound is
  still more than the structure needs (0x1cc00 against some 0x13000 here):
  the title takes that much of its pool for a structure whose size it reads
  before the card's lands, which is the case that wrote over neighbours.
  Making that read wait for the landing (the slot's page without access
  until then) was not done: the page holds other slots and the title's
  data, the layer writes it with the kernel's copies, and a wait on a title
  thread for our line is the kind that ended this run.
- What a structure that was written over says of its size sent the builder's
  copy of it through gigabytes of other structures; bounded now
  (`VICTORY_BVH_BOUNDS`). Pages touched that are not there: 2,500 a frame
  before, 90 after (the dorm).
- The card's time in the dorm with the builder (timing run light-13, title's
  commands 84 ms): `CopyForBVHUpdate` 13.3 ms in 63 dispatches (before the
  bound), traces 24 ms (`RaytraceVBuffer` 7.9, screen probes 6.4, irradiance
  cache 3.5, primary probes 2.3, shadows 1.6).
- Captures made on 2026-11-25 (builder on or off) hang the card in a replay
  (cause unknown; do not try).
- Five shadow-mask trace programs do not translate (a structure's address out
  of a descriptor picked from the heap by a computed index: the host would have
  to give a table of addresses by slot). The title did not create them in any
  run so far.
- An access to a page that is not there is lost (a read gives zero, a store is
  dropped).
- Not publicly documented, and assumed: the instruction's rounding, the order
  of two boxes entered at the same distance, and the result for a ray exactly
  through an edge.

## Without rays: the stand-in (`crates/victory-gpu/src/ambient.rs`)

Without structures every ray misses. The title's program that resolves the
diffuse light of its screen probes (`CS_ScreenProbes_TemporalResolve_*`, list
184 of a frame) then leaves the sky's light outdoors, nothing indoors, and in
blocks of tiles what the probes' memory held. `VICTORY_AMBIENT=<share>` raises
it to an estimate right after that program: the title keeps, for its own screen
traces, the light visible surfaces send out (`CS_CacheScreenSpaceRadiance`,
1280x720, the frame before); its mean in 64x36 cells is gathered at 160x90
points, each cell weighted by the share of the surroundings it covers, as a
directional value; each pixel takes it between the points by depth, with its
normal and a shade in creases. So level and tint follow the hour, the room and
its lamps. What the program left counts up to four times the estimate.

Where it can be put, and the crease shade, are beloooo's (VIctory-Belobug-AMD
06031237, 4f45ab88, cd9f78ce); `VICTORY_AMBIENT=constant:<level>` is that
fork's constant.

| variable | does |
|---|---|
| `VICTORY_AMBIENT=<share>` | the estimate, 1 all of it (unset: the title's own) |
| `VICTORY_AMBIENT=constant:<level>` | the same light everywhere |
| `VICTORY_AMBIENT_SHADE=<strength>,<metres>` | the creases (1,0.6) |
| `VICTORY_AMBIENT_AIR=<multiple>` | what the air scatters is bounded by a multiple of the light around (off: the brown veil in the day room stays with it, light-14) |

It is off while the builder has run whole (`builder_whole`: asked without making the builder's pipelines), so with the default it covers only the seconds before the builder's pipelines are all made; with `VICTORY_BVH_BUILDER=0` it is the light.

## Where to look

- `crates/victory-shader/src/bvh.rs`: the reference for the node test.
- `crates/victory-shader/src/emit/lower.rs`: the lowering of the two
  instructions and of memory by address (`Lower::memory`).
- `crates/victory-shader/src/resource.rs`: address tracking for descriptors
  and raw addresses.
- `crates/victory-gpu/src/pipeline.rs`, `draw.rs`: the builder rule and the
  host side of root views and title memory.
