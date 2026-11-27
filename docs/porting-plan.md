# Porting plan: from emulating the console to a native PC renderer

Status: design, 2026-11-26. Nothing here is implemented by this document. It is
the plan for the whole compat layer, from main b8e2b192e.

The direction, in Belobug's words: the way to speed is porting, not emulating.
We patch the title's renderer so it behaves like a PC renderer, instead of having
the layer watch and copy the console's shared memory every frame. Shaders are
translated ahead of time into a versioned store kept between builds.

The user then raised the bar: as native as Proton. Build it the way
DXVK/vkd3d-proton are built, and make GTA VI run better here than on the console.
That means four rules:

1. **Translate once, cache, keep.** Shaders, pipelines, root signatures,
   descriptor layouts, memory classes and state are worked out once and stored
   under versioned keys. A rebuild of the layer does not throw them away.
2. **Hot paths are thin.** A list call is a direct mapping onto one D3D12 call,
   or a few. Nothing in a frame inspects, diffs, hashes or copies title memory.
3. **Console memory semantics are settled at allocation and map time, not every
   frame.** Every title allocation the GPU uses becomes a real D3D12 resource of
   the right kind, chosen when it is allocated or mapped:
   - title memory opened as a heap (the title's CPU pointers point straight at
     what the card reads);
   - a DEFAULT-heap resource the CPU never touches;
   - a READBACK destination.

   Real fences and events replace the watching.
4. **Console-only work in the title's renderer is hooked and replaced** with the
   PC equivalent: its own residency and paging, console sync (memory labels,
   `mwaitx`), byte-copied descriptor records, and 16-byte constant alignment.

The target is a CPU frame well under 33 ms in the Series X profile: the world
drawn at 1080p, the title's ray tracing on, walking and driving. The console
runs this at 30 fps; the aim is 40-60.

Every port stays behind a `VICTORY_PORT_*` switch, off by default, until it has
been benched in Series X (`tools/bench.ps1`, off/on/on/off) and its captures
checked against the switch off. The emulation stays as the fallback and the
reference.

## 1. Where a Series X frame goes now

The measurements are:

- Run sox-on2 (bench 2026-11-26 22:22, build 1207144cb, Series X, render scale
  0.5 with the title's own upscaler, the title's ray tracing on, walking, all
  ports off). Its 10-second stat block is copied to the scratchpad as
  `port-plan/sox-on2-lastblock.txt`, outside the repository.
- The figures the coordinator gave: frame 127 ms, GPU 39 ms, the line 78 ms.
- The record-cost agent's profile (`record-cost/prof-base.txt`).

| Wall | ms a frame | What holds it |
|---|---|---|
| Frame (median) | 126-127 | CPU-bound |
| The line (submitting thread, `queue::behind`) | 73-82 | its steps of the lists 25-28, of which refreshes 17-18; write watch 6.4-6.8; title bytes under flights' writes 6.7-7.9; planning card steps 11-14; card-memory copies 7.9-8.7 |
| Relay / landing thread ("copying back") | 22.7-27 | merge of fetched pieces 13.5-13.9; fetches 6.8-7.8; guards 4.5-5.4; texture read-backs 1.8-2.2 |
| Line's second stage | 8-15 | sends, the steps after the first stage |
| Recording threads (8, `VICTORY_THIN`) | 520-580 busy (680 in the record-cost profile) | 70k calls at 7.1-7.8 us. Mostly `State::tables`; `shadow::exact` is 41 % of that |
| Heap record sync | 9.5-11.6 | 115k slots looked at, about 300 pages, 1k mirrored |
| Card | 15-39 (bench mean 15, coordinator 39) | rays, the title's passes, our hash and compare passes, clamps |
| Title's render thread | 12-34 waiting at presents | waits for the line |

Per frame the title's memory sees:

- 49.6 MB sent up in 77k ranges;
- 113.8 MB fetched back;
- 57.9 MB of shader writes landed (24.5 MB of it as dwords);
- 48,887 refreshes;
- 577-616 kernel write-watch askings over 2.9 GB;
- 710-776 blocks made read-only again, 598 guard faults and 6,840 page faults;
- 20k fills done on the card, 8.6 MB, almost all out of one 174 MB buffer at
  title 0xb10000 (`0xb10000+0xae00000`, see §5.1).

Sum of the walls: the line and the relay are separate threads, but the title
waits on both. Recording overlaps with the line. The frame is about
max(line + waits, recording wall) plus the title's own CPU. To reach below
33 ms, every wall must fall under about 25 ms:

- the line;
- the relay;
- the recording wall (busy time / 8 threads);
- the card.

## 2. Inventory: every per-frame emulation mechanism

The cost is Series X sox-on2 unless marked otherwise. The "Port" column points
into §5.

| # | Mechanism | Where | What it does every frame | Cost (Series X) | Source | Port |
|---|---|---|---|---|---|---|
| M1 | Write watch | `watch.rs`, hle `xmem` `MEM_WRITE_WATCH` | asks the kernel which 4 KiB pages of title memory were written, keeps a journal per page | 6.4-6.8 ms line; 577-616 askings, 2.9 GB a frame | log "preparing", "asking about title memory" | P3, P4 |
| M2 | Read-only guards on section views | `watch.rs` (`cover`, `open`, `views_mapped`), port 6 lock | 64 KiB blocks left read-only after each copy; the first title write faults, opens all aliases | 7-13 ms waiting for guards and 6.0-6.5 ms changing protections (all threads); 598 guard faults; guard lock 621 waits, 54.9 ms all threads | "asking about title memory", "waits for the write trackers' locks" | P3, P4 |
| M3 | Store/copy beside guards, write-site rewrites | `titlewrite.rs`, loader `write_sites.rs` | title stores into guarded blocks go through our view; redundant flag stores patched | off by default; findings: 69 → 6 ms of thread time where on | findings.md | absorbed by P3 |
| M4 | Host copies of title buffers (shadows) and their refresh | `shadow.rs` `Shadow::update`, `take`, `copy_stale`, `Batch::refresh_spans` | every range a submission reads is brought up to date from title memory | refreshes 16.9-18.2 ms of the line (11.5k a frame, 8.8k known to copy nothing); profile: `shadow::wide` 25.6, `update_free` 15.6, `take_all` 9.9 | log; record-cost profile | P1, P3, P4 |
| M5 | Exact copies of off-element views | `shadow::exact`, `descriptor/host.rs` `view_home` | a buffer view that does not start on an element of its buffer's copy gets a copy of its own bytes | 8,100-8,300 asked, 800-830 made, 9-10k kept a frame; 41 % of recording's `State::tables` (about 280 ms busy, all threads) | log "buffer views off their copy's elements"; coordinator | P5 |
| M6 | Raw twins of buffer views | `descriptor.rs`, `VICTORY_RAW_VIEWS` | a raw twin made for each typed buffer view | 28k made a frame | log | P5 |
| M7 | Descriptor heap record sync | `descriptor.rs` heap sync, `descriptor/host.rs` | finds the title's descriptor records copied as bytes, mirrors them into the host heap | 9.5-11.6 ms (heap thread), 1.4-3.2 ms of it waited for; 115k slots looked at | log "descriptor records the title copied" | P2 |
| M8 | Draw-table slot pass | `draw.rs` tables, "pass over the heap's views" | each table a draw reads: slots looked at, their copies' spans refreshed, textures | 2.6-5.2 slots + 2.1-3.9 spans + 1.4-2.1 textures; 0.3-4.0 ms waited at close | log | P2, P5 |
| M9 | Per-call binding at record time | `draw.rs` `State::tables`, `list/thin.rs` | each draw's tables resolved, views made or found, exact copies asked, dynamic slots searched (27k tables, 20.2 ms searching) | 520-680 ms busy across 8 threads (about 70 ms of wall); 7.1-7.8 us a call | record-cost; "recording per frame" | P5 (record-cost owns call cost) |
| M10 | Constant-buffer re-alignment copies | `draw.rs` `defer_constants`, port 5 | console CBs on 16 bytes copied to 256-aligned upload memory each submission | about 23k a frame, 9 MB (Belobug: −4.7 ms with port 5). With port 5 off on NVIDIA, each is a line step | port_cbv.rs; avm-413 | P1b |
| M11 | Card steps | `host/card_steps.rs` (`VICTORY_CARD_STEPS=2`) | fills that read title memory flights may still write, done on the card in flight order | planning 11.3-14.0 ms of the line; 20k fills, 8.6 MB | log | P1 |
| M12 | Card-memory copies: send up, hash and compare | `host.rs` `card_before`, `copy_mapped`, card rehash | host copies in VRAM: changed ranges sent up (49.6 MB in 77k ranges); 525 hash and 473 compare passes over 16.7k pieces on the card | 7.9-8.7 ms line (`card_before` 11.35 ms with callees in the profile); card time | log; profile | P1, P3 |
| M13 | Title bytes under flights' writes (late wait) | `host.rs`, `VICTORY_LATE_WAIT=2` | title CPU writes into ranges a flight's shaders may write, put in after the flight | 6.7-7.9 ms line; 671-810 pages, 3.4-5.8 MB | log | P3 |
| M14 | Relay / landing of shader writes | `land.rs`, `host.rs` relay, `safecopy::merge` | fetches pieces of card copies shaders wrote, merges the changed dwords into title memory, reopens guards | 22.7-27 ms (relay thread); 1,925 pieces, 57.9 MB new, 113.8 MB fetched | log "landing", "relay" | P3 |
| M15 | Texture read-back | `texback.rs` | textures shaders wrote that the CPU reads (ocean height and displacement) copied back and re-tiled | 1.8-2.2 ms; 17.9 a frame; lock waits up to 5.6 ms | log | P3 (class C) |
| M16 | Texture refresh and detile | `texture.rs`, `tiles.rs`, "textures picked by index" | title textures hashed and detiled when their memory changed; levels on demand | 8.3-8.6 ms all threads (1.5-3.2 asking the watch); 35.9 MB hashed | log | P6 |
| M17 | Streaming notices | hle `dstorage.rs` → `title_memory_landed`; `CopyPageMappingsBatchX` → `map_aliases` → `views_mapped` | DirectStorage writes and page remaps force re-copies, guard resets and re-looks | in the M1/M2/M4/M16 costs; in a car or boat it multiplies (5 fps) | coordinator; findings | P6 |
| M18 | Indirect argument clamps | `indirect.rs`, `indirect/batch.rs` | all indirect records copied and clamped on the card before they execute; port 1's order rule clamps in-place records alone | card time; port 1 order 2-5 ms (avatarmanz) | indirect.rs; avm notes | P8 |
| M19 | Memory by address (rays) | `memory.rs`, `shadow::by_address` | directory and arenas for raw-address shaders; faults read back; push of changed dwords | push 0.02 ms; 1.5-2 ms each of take / copy_stale, card_before, watch (fork-ray-tracing.md) | fork-ray-tracing.md | gpu-rays owns; P3 subsumes the arenas |
| M20 | BVH size bound | `bvhsize.rs`, hle `modules/bvh.rs` | prebuild answers kept; size slots given a bound | small | fork-ray-tracing.md | P7 |
| M21 | Fence and label emulation | `fence.rs`, `queue/behind.rs`, loader `console_insn` (`mwaitx` → `pause` at title+0x4fb0ad0) | bottom-of-pipe writes into title memory by the relay; the title spins on a memory fence | 22-26 waits met by flights' writes; render thread 12-34 ms at presents | log "submissions sent ahead" | P7 |
| M22 | Settles and flight waits | `host.rs` `settle`, `waited for flights` | a step that reads memory a flight may write waits for the card | 4.5-15.4 ms waiting for the card; 1.8-5.8 ms for upload-memory fills | log | P1, P3 |
| M23 | Shader translation and pipeline keys | `shader_cache.rs` (key `env!("VICTORY_TRANSLATOR")`), `pipeline/known.rs`, `apps/victory/src/warm.rs` | not per frame, but every build of the layer invalidates all 25k translations and the warm list; stalls of 200 ms+ while pipelines are made | 270-660 s of stalls a session (findings); a full retranslate per build | Belobug 1162d38f9 | P0 |
| M24 | Console CPU instructions | loader `console_insn.rs` | `clzero` loops, `rdpru`, `mwaitx`, `monitorx`, `extrq`/`insertq` patched on first fault | about 0 after patching; the `mwaitx` spin burns a core | console_insn.rs | P7 (sleep) |
| M25 | Diagnostics on the hot path | `copystat`, `framelog`, `heapstat`, panel | counters on every step | small (under 1 ms), but in every function | profile | P9 |

Title patches that exist already:

- In `victory-loader/src/hooks.rs`, fixed RVAs gated on the BLAKE3 prefix
  `b99f876b74d4a77f` and checked byte for byte: the watchdog, the own-upscaler
  choice, render scale and FSR scale.
- `write_sites.rs`.
- Ports 1, 5 and 6, all off by default.

None of them is per frame.

## 3. What the others did

### Belobug and avatarmanz renderer ports

They are on `belobug/a-publier` and `belobug/rx9070xt`, and also in
avatarmanz/VIctory-7900XTX. All are off by default. The reports they cite, such
as `reports/PORT-PATCH-*.md`, are not in any tree we have.

| Port | Switch | Title side | D3D12 side | Gain cited (RX 7900 XTX, Series S, city) | Ours |
|---|---|---|---|---|---|
| 1 | `VICTORY_PORT_CONSTANTS` | the routine at +0x1e86440 makes the 185 MB 32-bit graphics heap (kind 0x42000, five placeholders, one physical allocation); the 174 MB constants ring is in it | a second view of the section opened with `OpenExistingHeapFromAddress1`; buffers placed in it, so nothing is tracked, uploaded or compared | 57-75 → 53 ms with 1+2 (Series X) | in main, plus avatarmanz's order fixes (9812504b4, 32746a28d, 23b5d82d5) |
| 2 | `VICTORY_PORT_DESCRIPTORS` | six 32-byte record stores in +0x4d9efd0 and +0x4eec8e0 (the frame allocator's view heap at +0x4d9f400) patched to mark the slot and its 4 KiB chunk; the allocator's memcpys (+0x4d9f386, +0x4d9f54c, +0x4d9f26e) and the title's memcpy/memmove imports mark too | `sync_heaps` walks the marks, not the write watch; bounded fallback (`_EVERY` 8 frames, `_CHECK` 16 pages) | part of 1+2; heap sync 15-16 → about 4 ms | not taken |
| 3, 4 | (none) | never published. Port 7 brings two parts of patch 4 over from `port/lane` 97033e5a, which we do not have | | | |
| 5 | `VICTORY_PORT_CBV` | none: the shader side | shaders read CBs as raw bytes; table CBs through a raw twin of title memory in place; root CBs as root SRVs | −4.7 ms | in main. On NVIDIA (1M-view cap) the twins are off, so it does nothing there. avm-413 (`=2`) fixes that |
| 6 | `VICTORY_PORT_GUARD` (+`_PLAN`) | none: the layer | guard lock as a reader-writer lock with a claim per block; refresh planning on helpers | title waits 29 → 2 ms; 47.2 → 45.1 ms | in main |
| 7 | `VICTORY_PORT_JOURNAL` | heap-sync round asked at the title's ExecuteCommandLists | journal in 64 shards | journal waits 8-9 → 0.8 ms; line 42.9 → 40.4 ms; 46.1 → 45.1 ms | not taken |

The **versioned shader store** is avatarmanz 1162d38f9, merged in 190a68694:

- `victory_shader::TRANSLATOR_VERSION: u32`, bumped only when a change alters a
  translation.
- The key is BLAKE3 over:
  - `version N`;
  - the entry format;
  - the translation switches;
  - `shader_opt`;
  - `PORT_CBV` when it is on;
  - `host_settings()`.
- Entries (`VSH2`) also record the translator source hash. An entry from an
  older build is served and counted `STALE`.
- `victory shaders` retranslates stale entries and reports any that come out
  different, which means someone forgot a bump. `VICTORY_SHADER_FRESH=1` does
  the same during a run; `VICTORY_SHADER_KEY=source` restores the old keying.
- `pipeline/known.rs`'s fingerprint follows the same key. The warm list
  survives rebuilds.
- Evidence: after a comment-only rebuild, 25,790 entries refreshed and 0
  differed.

**Ours**: the key is `env!("VICTORY_TRANSLATOR")`, a BLAKE3 over all of
`victory-shader/src`, `Cargo.lock` and `rust-toolchain.toml`. Any edit or
dependency bump invalidates all of it, and `warm.rs` treats every entry of
`warm-done.txt` as stale.

### Upstream and the other forks

The refs are `refs/prs/*`; the triage data is the scratchpad's
`upstream-prs/scored.tsv`.

- **Marvisak's memory model** (main 63bcca43c) is the emulation we inherited:
  persistent system-memory copies, guards and write watch, a page table for raw
  addresses, and every submission waited for. The open PRs go toward porting:
  - #2112 `VICTORY_IN_PLACE` and #2734 `_IN_PLACE_VRAM`;
  - #2993 `VICTORY_VRAM_COPIES` (a GPU-upload twin the CPU only writes);
  - #3228 `VICTORY_UPLOAD_VRAM`;
  - #3102, the full model: a VRAM image kept coherent both ways. Benchmark GPU
    time went from 44 to 11 ms. In place over the bus was 43.7 ms per
    submission against 29.7-36.7.
  - #3120 `VICTORY_GPU_OVERLAP`: a completion thread signals the title's
    fences, as the console does.
  - #3053: graphics blocks of `XMemAllocDefault` get their own watched
    reservation.

  Marvisak #2120 measured shader reads of 256 MiB at 11.6 ms from system
  memory against 0.37 ms from VRAM. **System memory is fine for data read once
  (constants) and wrong for data read many times (textures, static meshes,
  BVH).**
- **fxceless**:
  - #772 and #980 (`VICTORY_INPLACE=3/4`, pools): 75 % less copy-back.
  - fxc-1117 `VICTORY_INPLACE_SECTIONS`: SEC_COMMIT sections opened with
    `OpenExistingHeapFromFileMapping`.
  - fxc-868 `VICTORY_SKIP_WC_BACK`: no copy-back for write-combined title
    memory; −11 to −20 % frame time.
  - fxc-670: no kernel askings for view memory.
  - fxc-1029 `VICTORY_MWAITX_SLEEP`: the fence spin waits on `WaitOnAddress`.
- **avatarmanz, beyond the ports**:
  - avm-197 `VICTORY_BARRIERS`: a store that faults is rewritten into a
    trampoline that writes through our view and marks; untested in game.
  - avm-420 `VICTORY_PORT_PLACED` (Linux only).
  - avm-339 `VICTORY_PORT_BUFFERS`.
  - avm-359, avm-384 and avm-385: port 2 refinements.
  - `VICTORY_OUTPUT` (`resolution.rs`): the title's settings applier
    +0x22434a0 and screen-size writer +0x2478130. About 16 ms less card time
    in Series X. Not in main.

## 4. The target architecture

What each layer becomes. The phases are in §6.

### 4.1 Memory model: a class per allocation, decided once

Today every GPU-visible title byte has two homes: title memory and a host copy.
The layer keeps them equal in both directions, every frame, by watching (M1-M4,
M12-M14). On a PC the engine knows up front what each allocation is for.

The port is to **decide that per allocation, at `XMemVirtualAlloc`,
`XMemAllocatePhysicalPages`, `XMemMapPhysicalPages` and `CopyPageMappings`
time, and keep the decision in a versioned per-build profile.**

The classes, and what backs each:

| Class | Title use | Backing | Per frame after |
|---|---|---|---|
| **U: CPU-write, GPU-read once** | constants ring, upload and dynamic buffers, the frame allocator, indirect arguments the CPU writes, the view heap's records | title memory itself, opened as a host heap (`OpenExistingHeapFromAddress1` on the section view, as port 1). Pinned system memory, write-back. The GPU reads over the bus once. | nothing. The title's fences already order writes before reads (port 5's assumption, held by port 1 since its order fixes) |
| **G: GPU-only** | render targets, UAV scratch, GPU-built BVH, culling records, skinning output read only by shaders | DEFAULT heap (VRAM), placed resources. The title's pages stay a reservation the layer never copies. | nothing: no refresh, no fetch, no landing |
| **C: GPU-write, CPU-read** | ocean height and displacement maps, BVH compacted sizes, occlusion and query results, timestamps, fence lists | title memory in place (as U), so the GPU writes the title's bytes directly; or a READBACK twin copied at the list's end for typed textures (texback) | one bus write, no compare or merge |
| **S: streamed static** | textures, static meshes, packed BVH, everything DirectStorage writes | DEFAULT heap. Uploaded once, on the event that writes it (the DirectStorage completion our hle serves, `XMemMapPhysicalPages`, `CopyPageMappings`); dropped at unmap | nothing between events; guards only to catch the rare CPU write (then demoted to U) |
| **X: unknown** | anything not yet classified | today's emulation | today's cost, shrinking as the profile learns |

How classes are learned ("learn then port", like a pipeline cache):

- The first sessions of a build run with the emulation plus light probes:
  - which allocations the CPU writes (the guards we already have);
  - which it reads (`VICTORY_READ_WATCH` already exists: no-access guards on
    landed pages);
  - which the GPU writes (declared UAV ranges, the land's pieces).
- An allocation is named by a key stable from run to run, not its address:
  - the allocating call site (return address in the title image);
  - its kind and flags;
  - its size;
  - its order among the site's calls.
- The profile lives beside the shader store and is keyed by title build plus
  `MEMORY_MODEL_VERSION`.
- A class is only ever widened at run time, never narrowed:
  - G or S seeing a CPU write becomes U;
  - G seeing a CPU read becomes C;
  - each widening is logged and saved.

  That is the correctness net. It is the same mechanism as port 2's fallback
  and port 6's guards, used for classifying instead of copying.

Why this suits the 5060 Ti: the card is PCIe 4.0 x8, about 14 GB/s.

- U and C data crosses the bus once: about 50 MB up a frame is 3.5 ms of bus
  time, hidden behind the card's work.
- G and S data never crosses.

Today 49.6 MB goes up and 113.8 MB comes back every frame, with hashes and
compares on both ends.

### 4.2 Binding model: descriptors made once, tables are offsets

Today, for every draw, a recording thread resolves each table slot:

1. looks at the title's record;
2. finds or makes a host view;
3. when the view starts off an element of its buffer's copy, asks
   `shadow::exact` for a copy of its own;
4. makes raw twins;
5. searches dynamic slots.

That costs about 70 ms of wall a frame on 8 threads (M5, M6, M8, M9).

The PC form (vkd3d-proton's bindless model):

1. **A host descriptor per title descriptor record, made when the title writes
   the record**, not when a draw reads it.
   - Port 2's patched store sites and memcpy marks say which slots changed.
   - A translator thread turns them into host descriptors in the mirrored heap,
     at the same index, off the recording path.
   - Recording binds a table as one `SetGraphicsRootDescriptorTable` (heap
     start + title offset). Nothing is looked at.
2. **Views straight onto the title's own buffers.**
   - Once U, C and G memory are D3D12 heaps (4.1), a title GPU address is a
     fixed offset into a placed buffer over the whole heap stretch.
   - A view that starts anywhere on 16 bytes becomes:
     - a raw (byte-address) view with its byte offset, or
     - a typed view with `FirstElement = (va - base) / stride` when the start
       is on an element of a whole-stretch buffer.
   - `shadow::exact` (M5) and the per-view copies go away. Port 5's raw twins
     generalise it.
   - Structured views off-element take the raw form. The translator already
     reads structured buffers raw where needed: `VICTORY_VIEW_BYTES` and the
     raw twins.
3. **The NVIDIA 1M-view cap.**
   - The title's heap is a little over 1,000,000 slots
     (`descriptor/host.rs` `PORTABLE_VIEWS`).
   - With persistent descriptors the host heap need not mirror slot for slot.
     A remap table keyed by heap page (title slot → host slot), made once per
     heap layout, compacts the unused ranges. Translated shaders that index the
     heap directly read through that table.
   - Simpler first step: avm-413's root SRV for the CB rows (`PORT_CBV=2`).
4. **Root signatures and descriptor layouts made once per shader family** and
   kept in the store (4.4), not rebuilt per pipeline.

### 4.3 Sync model: real fences, no watched labels

- The title waits on memory fence values it polls with `mwaitx` (title
  +0x4fb0ad0). Bottom-of-pipe writes into title memory are relayed by our
  landing thread (M21).
- The port:
  - the title's queue fences map onto D3D12 fences;
  - a completion thread waits on `SetEventOnCompletion` or
    `ID3D12Fence::GetCompletedValue` and writes the label;
  - the spin sleeps on `WaitOnAddress` (fxc-1029; avm-330);
  - this is Marvisak #3120's overlap, but without copy-back, since C memory is
    in place.
- What remains of settle waits (M22) are true orders, expressed as D3D12 waits
  on the GPU (queue `Wait`), not CPU waits on the line.

### 4.4 Shader, pipeline and state store

The versioned store follows avatarmanz 1162d38f9, adapted. Its one key is:

```
store key = BLAKE3(TRANSLATOR_VERSION, FORMAT, translation switches,
                   host settings, title build)
```

- It covers DXIL, root signatures (4.2), the warm list and the memory-class
  profile (4.1).
- The driver library (`host-pipelines.bin`) stays keyed by the PSO's full
  description. The driver itself refuses another driver's.
- Every pipeline seen is compiled at the start or after exit (play.ps1 already
  does this, f4947528d). Stalls go to zero after the first session of a
  translator version.

### 4.5 Submission and recording

- With 4.1-4.3 in place the line has little left:
  - no refreshes;
  - no card steps;
  - no late waits;
  - no write watch;
  - no settles.
- What stays: `ExecuteCommandLists`, fence signals and waits, and presents. It
  is about 2-5 ms, as on a PC.
- Recording's per-call cost is the record-cost agent's. This plan removes the
  binding work it measured (4.2), which is most of the 680 ms.

## 5. The ports

For each port:

- the hook, and the data path to D3D12;
- **CPU ms saved** in Series X;
- **risk**, **effort**, **dependencies**;
- **D/S**: whether it helps driving and streaming.

The savings are estimates from the sox-on2 block. They overlap, so each figure
is "of today's frame".

### P0. Versioned shader and state store (`VICTORY_SHADER_KEY`, default the version)

- **Hook:** none on the title side. The key changes as in §3: port 1162d38f9,
  keep our longer switch list and the `*_ONLY` hashing, add the title build id,
  and make `known::fingerprint` and `warm.rs` follow the version.
- **Savings:**
  - Frame: 0 ms steady.
  - Every rebuild: about 25k retranslations and all warmed pipelines kept. The
    200 ms+ stalls of the first minutes after a rebuild are gone. Bench runs
    stop measuring cold caches.
- **Risk:** low. A forgotten bump serves stale DXIL. `victory shaders` reports
  "N different", and CI can run it over the store.
- **Effort:** S (1-2 days).
- **Dependencies:** none. Do first: every later port's benches need warm,
  comparable caches.
- **D/S:** stalls only.

### P1. Constants and upload memory in place (ports 1 and 5 made to work in Series X on NVIDIA)

The hook:

1. Port 1 recognises the 32-bit heap by kind 0x42000 (xmem `LOW_RESERVATIONS`),
   not by address. In Series X the 174 MB buffer the card steps fill from
   lies at title 0xb10000 (`0xb10000+0xae00000`), not 0x34e10000.
   - **Check first** that port 1 opens it in Series X.
   - The log's `host::card_steps` lines name it as the source of 20k fills a
     frame. If port 1 opens it, all of them go away.
2. Port 5 on NVIDIA. Its raw twins are off under the 1M-view cap. Take avm-413
   (`VICTORY_PORT_CBV=2`): the CBV is placed at the 256 bytes below the title
   address, the row bias goes in the stride word, and a root SRV goes at slot
   252.
3. Extend the same in-place opening to `upload.rs` arenas and the title's
   frame allocator (+0x4d9f400). Both are class U.

The data path: title CPU writes land in pinned pages that the GPU reads through
placed buffers. Port 1's order rule (in-place indirect records clamped alone)
stays.

**Saves (CPU):**

| Item | ms |
|---|---|
| Card-step planning | 11-14 |
| Card copies of the ring (`card_before`, sends of 18 MB / 671 pieces for that buffer) | 5-8 |
| Its refreshes | 3-5 |
| Upload-memory fill waits | 2-6 |
| CB re-alignment copies | a few |
| **Line, total** | **about 20-30** |

On the card: fewer hash and compare passes.

- **Risk:** medium.
  - Ordering of reads against the title's CPU writes. Port 1's palms and trees
    bug is the known case, fixed by the order rule.
  - The 1M cap on NVIDIA.
- **Effort:** S-M.
- **Dependencies:** P0. gpu-slow is testing ports 1, 5 and 6, so take their
  results and fixes, and do not repeat their runs.
- **D/S:** a little. Constants scale with draw count.

### P2. Descriptor records from the title's own stores (ports 2 and 7, `VICTORY_PORT_DESCRIPTORS`, `VICTORY_PORT_JOURNAL`)

- **Hook:**
  - Belobug/avatarmanz port 2: the six record stores at +0x4d9efd0 and
    +0x4eec8e0 are patched to mark, and so are the memcpys (+0x4d9f386,
    +0x4d9f54c, +0x4d9f26e) and the title's memcpy/memmove imports.
  - avm-384 and avm-385 for the missed records (101 → 6 a frame).
  - Port 7: the heap round is asked at ExecuteCommandLists, and the journal is
    sharded.
  - All are byte-checked against build `b99f876b74d4a77f`, as hooks.rs does.
- **Data path:** marked slots → the host heap at the same index, made on the
  heap thread during the time a submission waits in line.
- **Saves:**
  - Heap sync: 9.5-11.6 → about 2 ms (the bounded fallback).
  - Line waits: 1-4 ms.
  - Recording: the slot looks of M8 (2.6-5.2 ms).
  - **Total: about 10-15 ms of thread time, 3-6 ms of the frame wall.**
- **Risk:** medium.
  - A record written by a path nobody marks gives a stale descriptor, which is
    a visible pop. The fallback check and the avm-385 trace catch it.
- **Effort:** M (port and rebase onto our `descriptor.rs`, which has moved).
- **Dependencies:** none. It is a prerequisite for P5.
- **D/S:** yes. Streaming writes many new records.

### P3. Memory classes: GPU-only memory leaves the copy loop (`VICTORY_PORT_CLASSES`)

The hook is the title's allocation and map calls, all already ours in hle
`xmem`:

- `XMemVirtualAlloc` (`virtual_alloc`);
- `XMemAllocatePhysicalPages`;
- `XMemMapPhysicalPages` and `...Scatter`;
- `map_alias` for `CopyPageMappingsBatchX`.

Each allocation gets its class from the profile (4.1).

Data path:

- G memory: the layer's copy (already a VRAM copy for most: "host copies in the
  card's memory: 3,488 copies, 2,238 MB") becomes the *only* home. Never sent
  up, never fetched, never landed. Guards off.
- C memory: placed in place, so GPU stores go straight into title memory. Or
  texback for tiled textures, as today.
- The rest stays X (emulated).

Order of work:

1. A census switch, `VICTORY_PORT_CLASSES=learn`: logs per allocation key what
   the CPU wrote, what it read and what the GPU wrote, using the guards and the
   land we already have.
2. `=use`: applies the classes.
3. Widening at run time, as in 4.1.

**Saves (CPU):**

| Item | Today | After |
|---|---|---|
| The relay/landing (most of the 57.9 MB landed is shader output the CPU never reads; the traffic lines show 48.7 MB fetched from copies nothing was sent up to, which is G by definition) | 23-27 | about 3-5 |
| Late waits | 7-8 | about 1 |
| Refreshes | 17 | about 6-8 |
| Write watch and guards on those ranges | about half of 13-20 | |

- **Total:** about 35-45 ms of thread time, 20-25 ms of the frame.
- **Card:** hash and compare passes over G copies stop.
- **Risk:** high. This is the semantic core. A wrong G kills CPU readers: boats
  sink (texback found the ocean case this way), and BVH sizes go wrong.
  - Mitigation: the learn phase, widening at run time, the per-allocation
    override list `VICTORY_PORT_CLASSES_X=<keys>`, and captures against the
    emulation.
- **Effort:** L (2-3 weeks).
- **Dependencies:** P1, which shows in-place opening on these sections works on
  NVIDIA. The section must be SEC_COMMIT for `OpenExistingHeapFromFileMapping`
  (fxc-1117) or opened by view with `...FromAddress1`. Work with settle-order
  (copy-back owner) on the land's interfaces and gpu-rays on the arenas
  (`memory.rs`).
- **D/S:** yes. It is the first half of the driving fix: fewer pages watched
  per streamed byte.

### P4. Write watch and guards only for class U/X writes (`VICTORY_PORT_NOWATCH`)

- **Hook:**
  - After P3, U memory is in place: the GPU reads the title's bytes, so nothing
    needs to know they changed.
  - G and S need no watch at all.
  - Remove `MEM_WRITE_WATCH` from their reservations at `virtual_alloc` and the
    guards from their section views (`watch::cover`).
  - What remains watched is X.
- **Saves:** the rest of M1 and M2. Write watch 6.4-6.8 → under 1 ms; guard
  waits and protection changes 13-20 → about 2 (all threads); page faults from
  6.8k to hundreds.
  - **Total: about 8-12 ms of the frame wall**, and the title's own threads stop
    faulting (they waited 29 ms before port 6).
- **Risk:** medium. Everything rests on P3's classes.
- **Effort:** M.
- **Dependencies:** P3.
- **D/S:** yes. Every DirectStorage write today is a watched write.

### P5. Native binding: persistent descriptors, tables as offsets, views on title buffers (`VICTORY_PORT_BIND`)

The hook: port 2's marks (P2) feed a descriptor translator. Each title record
becomes one host descriptor when written:

- a view onto the heap-placed buffer of U, C or G memory, by offset;
- for S, its DEFAULT copy;
- a raw view when off-element.

`draw.rs` `State::tables` becomes "bind table at host offset". `shadow::exact`
and the raw twins (M5, M6) are dropped where the memory is in place. Off the 1M
cap: the remap table of 4.2.

Phases:

- **5a.** Views onto in-place memory skip `exact` (U, C only). The gain shows on
  P1's memory at once.
- **5b.** Records made at write time, not at bind time.
- **5c.** Remap or compaction for NVIDIA's cap, root signature per family in
  the store.

**Saves:**

- Recording busy time: 520-680 → about 100-150 ms (exact is 41 %; the table
  walk is most of the rest).
- **Recording wall: about 70 → 15 ms.**
- With P2, M8 is gone (2-9 ms of the line's waits).

- **Risk:** medium-high. Stale descriptors when a record is rewritten while a
  list that reads it is still to run. Console rule: the title may not rewrite a
  record a list in flight uses. It fences; port 2's ordering argument holds.
  Also the views' formats and strides on NVIDIA.
- **Effort:** L.
- **Dependencies:** P2 (marks), P1 and P3 (in place). Coordinate with
  record-cost: they own the per-call cost; this removes work from the calls,
  not the call machinery.
- **D/S:** yes. Streaming makes 8-10k new views a frame today, each an
  exact-copy ask.

### P6. Streaming the PC way: upload on the event, not on the watch (`VICTORY_PORT_STREAM`)

The hook is hle `dstorage.rs`. It reads the files and writes title memory
through the section window, so the layer *is* the DirectStorage runtime.

- **On completion:** for ranges of class S, write the same bytes into a
  GPU-upload (ReBAR) staging ring and record a copy into the DEFAULT copy or
  texture tile on the copy queue. Detile at that time for textures
  (`texture.rs`), once.
- **`XMemMapPhysicalPages`, `CopyPageMappingsBatchX` and unmaps:**
  `UpdateTileMappings` of the host copies (tiles.rs already has reserved
  textures), with no re-copy of pages whose contents did not change. A remap
  moves a mapping, not data.
- The title's chunk manager (+0x293b4b0), streaming batch submit (+0xe88f0) and
  page-list writer (+0x55aa6a0) are the places to observe if a stream bypasses
  our DirectStorage.

**Saves:**

- Walking: texture refreshes 8.3 → about 1 ms (no hashing or watch asking),
  plus the streaming part of M1, M2 and M4.
- **Driving: the 5 fps case.** Every streamed byte is today watched, then
  hashed or compared, then copied. After this it is copied once, on the copy
  queue.

- **Risk:** medium. The title's CPU sometimes patches streamed data after load
  (fix-ups, the BVH rehydration in place, which is GPU-side). Guards on S catch
  CPU writes and demote the range to U.
- **Effort:** L.
- **Dependencies:** P3 (class S), P0.
- **D/S:** yes. It is the main driving fix.

### P7. Real fences and completion (`VICTORY_PORT_FENCES`)

- **Hook:**
  - The title's fence objects and bottom-of-pipe writes (D3D12.X `Signal` and
    `EnqueueMakeResident`-style labels) map onto host fences.
  - A completion thread writes the labels and wakes `WaitOnAddress` (from
    patched `mwaitx` at +0x4fb0ad0; fxc-1029).
  - Settles become GPU-side `Wait`s.
  - BVH compacted sizes: the size slot is class C, in place, so the title reads
    the card's value once it has landed. The real fix for the overlaps in
    fork-ray-tracing.md is to make the title's size read wait on the build's
    fence. That needs a hook on the read of the compacted-size buffer, owned by
    gpu-rays.
- **Saves:** line waits 4.5-15 ms; the relay's label writes; a core freed from
  spinning. Frame latency drops, since the title is told when the card is done,
  not when the relay is.
- **Risk:** medium (deadlocks; the order of signals against landings).
- **Effort:** M.
- **Dependencies:** P3. Once the C memory is in place, labels need no landing
  before them.
- **D/S:** a little.

### P8. Indirect clamps only where a writer may be missing (`VICTORY_PORT_TRUST_INDIRECT`)

- **Hook:** none on the title side. Clamp only indirect records whose writing
  pass is not a translated pipeline the host has (the `builder_whole` rule,
  generalised). Keep port 1's order rule but put the clamp on the card's queue
  only.
- **Saves:** 1-3 ms of card time; port 1's order cost of 2-5 ms.
- **Risk:** medium (a TDR if a bad record gets through). The `VICTORY_INDIRECT_*`
  diagnostics and the 65,535 cut stay.
- **Effort:** S.
- **Dependencies:** P1.
- **D/S:** no.

### P9. The rest

| Port | What | Saves |
|---|---|---|
| **P9a** `VICTORY_OUTPUT` (avatarmanz `resolution.rs`) | the title renders at its own table's mode | about 16 ms of card time in Series X: headroom for rays at 1080p |
| **P9b** Diagnostics behind one global off | `copystat` counters compiled out of release benches | under 1 ms |
| **P9c** The title's console-only render work | worth looking for after P3, through the class census: an allocation the title fills with `clzero` (console_insn +0x4e8ab4c) every frame then hands to the GPU is a clear, and becomes a GPU clear | unknown |

## 6. Order of work and the path to 33 ms and past it

The projected walls in Series X, walking, rays on. Each step's figure assumes
the ones before it.

| Step | Ports | Line | Relay | Recording wall | Frame (est.) |
|---|---|---|---|---|---|
| today | | 78 | 23-27 | about 70 | 127 |
| 1 | P0 (store) | 78 | 25 | 70 | 127, no stalls after rebuilds |
| 2 | P1 (constants and upload in place, Series X, NVIDIA) | about 50 | 22 | 65 | about 95-100 |
| 3 | P2 (descriptor marks, journal) | about 45 | 22 | 60 | about 85-90 |
| 4 | P3 (memory classes) | about 25 | about 5 | 55 | about 60-65, then held by recording |
| 5 | P5a and b (native binding) | about 22 | 5 | about 15-20 | **about 30-35** |
| 6 | P4 (no watch), P7 (fences) | about 10 | 2 | 15 | about 25 (card 15-39 then the wall) |
| 7 | P6 (streaming) | 10 | 2 | 15 | driving at about walking's time |
| 8 | P8, P9a, record-cost's call work | under 10 | 2 | about 10 | card-bound: 40-60 fps with `VICTORY_OUTPUT` |

**The first five to do together: P0, P1, P2, P3, P5a.** They take the line from
78 to about 22 ms and the relay from 25 to about 5. Recording is what is left
over 33 ms, and P5a/b is aimed at exactly that.

P4 and P7 then take the line toward a PC's 5-10 ms. P6 is the driving fix. After
it the card is the wall. gpu-slow and gpu-rays own the card's time. P9a and P8
give it room.

What each step must show before its switch goes on by default:

- **Bench:** 4 runs off/on/on/off in Series X, walking, plus one drive (car)
  and one boat. Frame median, p75, and the stat block's per-mechanism lines.
- **Captures:** `VICTORY_CAPTURE` with world captures every 20 frames
  (`VICTORY_CAPTURE_WORLD_EVERY=20`) on the walk, compared with the switch off.
  Check the known canaries:
  - palms and trees (indirect order);
  - lit rooms flicker;
  - boats float (ocean C class);
  - BVH structures (rays light the porch);
  - water reflections;
  - no grey tiles.
- **Long run:** no device loss in a 20-minute session, no watchdog.
- **Fallback:** the switch off gives today's behaviour bit for bit.

## 7. Working around the other agents

| Agent | Owns | This plan |
|---|---|---|
| record-cost | per-call recording cost | P5 takes the binding work out of the calls; the call path is theirs. Agree the `State::tables` interface before P5b. |
| settle-order | copy-back and refresh order | P3 replaces most of what they order. Until P3 is benched their order is the default, and P3's X class keeps it. Share the land's interfaces. |
| gpu-rays | BVH builder, `memory.rs` arenas | the arenas become class G pages in P3; the compacted-size read hook (P7) is theirs to place |
| gpu-slow | card slowdown; tests of ports 1, 5 and 6 | P1 starts from their Series X results with ports 1, 5 and 6, and does not repeat those runs |
| upstream-prs | hand-ports of upstream PRs | P0 (1162d38f9), P2 (ports 2 and 7, avm-384, avm-385) and P1's avm-413 are on their list. Ask them to take those first, or take them here, not both. |
| water-fix | water artefacts | P3's C class must keep the ocean read-back (texback). They review it. |

## 8. Reverse-engineering notes (roles and addresses only)

Build `b99f876b74d4a77f` (Series X 1.0.2.1 executable); RVAs from the image
base.

| RVA or address | Role | Used by |
|---|---|---|
| +0x1e86440 | makes the 185 MB 32-bit graphics heap (kind 0x42000, five placeholders, one physical allocation) | port 1 |
| title 0x34e10000 (Series S); 0xb10000 seen in Series X, to be confirmed | 174 MB constants ring buffer, skinning also writes it | P1 |
| +0x4d9f400 | frame allocator; its memory is the shader-visible view heap | port 2, P2, P5 |
| +0x4d9efd0, +0x4eec8e0 | the six 32-byte descriptor record stores | port 2 |
| +0x4d9f386, +0x4d9f54c, +0x4d9f26e | the allocator's record copies | port 2 (`VICTORY_PORT_COPIES`) |
| +0x4fb0ad0 | the fence value the title waits on with `mwaitx` | P7 |
| +0x4e9b54a | a fence list in title memory (once broken by copy-back) | P3 (C class) |
| +0x4e8ab4c | a `clzero` loop | P9c |
| +0x2479466 | ExecuteIndirect signatures | P8 |
| +0x2477578, +0x24800d5 | pipeline state creation calls | P0 |
| +0x86f090 | pipeline stream layout build | P0 |
| +0x24821dd | frame-token submission (PresentX) | P7 |
| +0x24841a6 | queue timestamp frequency | |
| +0xe88f0 | streaming batch submit | P6 |
| +0xf2c470 | batch page-mapping list (`CopyPageMappings`) | P6 |
| +0x55aa6a0 | page lists written | P6 |
| +0x293b4b0 | chunk manager | P6 |
| +0x22434a0, +0x2478130 | settings applier, screen-size writer | P9a |
| +0x7e9426, +0x28f1e7e/87, +0x28ef6b2, +0x6300ba8 | upscaler choice and render scale | hooks.rs |
| +0x2215c27, +0x23cf683 | watchdog | hooks.rs |

Title patches keep the hooks.rs rule:

- a fixed RVA per known build;
- the exact original bytes checked before writing;
- a mismatch skipped with a warning;
- never a dump of the title's code in the repository.
