# RTX 5060 wave-size diagnosis

The Series X profile reached the opening credits on an RTX 5060 (Windows driver `32.0.16.2124`, Windows build `26300.9974`). The original session ran DexNom commit `2e01ab4`. D3D12 reported 32 lanes for both `WaveLaneCountMin` and `WaveLaneCountMax`. [Microsoft's D3D12 options reference](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_feature_data_d3d12_options1)

## Compute pipeline refusals

The third session logged `0x80070057` for these six compute pipelines. A separate run of `victory translate --pipelines --list --validate` on the same PC validated the DXIL for all six with the Windows SDK validator. All six still declare a fixed wave size of 64 after translation. The tool now detects the host's maximum automatically, reports the wave-size mismatch, and created pipelines for all 12 comparison shaders selected from successful creations in the same log.

| Compute shader hash | Refused in session 3 (UTC) |
| --- | --- |
| `369cbe906ea72764` | 18:12:02 |
| `02384a60fda6f2d0` | 18:13:26 |
| `2da4beb386509b77` | 18:13:54 |
| `2cc4c197ee29e291` | 18:13:58 |
| `5dc3b25986622949` | 18:19:27 |
| `6f225b641c5b0708` | 18:19:28 |

The runtime now records the adapter name and PCI IDs and skips creation of compute pipelines that declare a wave size above the device's reported maximum. This makes skipped dispatches easier to trace. A wave32 implementation still needs to preserve the order of shared-memory reads and writes inside branches. For groups of multiple console waves, any synchronization that uses a group barrier also needs every thread in the group to participate. Earlier experiments with `VICTORY_WAVE_WIDE=1` reported device loss on another NVIDIA system. [RTX 3060 Ti report](https://github.com/DexNom/VIctory/pull/14) · [RTX 4070 report](https://github.com/DexNom/VIctory/pull/15)

To repeat the offline check from a diagnostics build, substitute the archive from your local game installation:

```powershell
$env:VICTORY_TRANSLATE_ONLY = '369cbe906ea72764,02384a60fda6f2d0,2da4beb386509b77,2cc4c197ee29e291,5dc3b25986622949,6f225b641c5b0708,13dfdd3b1136e407'
& .\target\x86_64-pc-windows-msvc\release\victory.exe translate --pipelines --validate --list --jobs 1 'D:\Path\To\Game\shaders.rpf'
```

## Frame time and termination

The third session started with an existing pipeline library of about 87 MB. The screenshots show roughly 1–2 FPS during the opening credits. One late log sample recorded 1,633.9 ms per frame, including 1,035.5 ms of preparation, 253.8 ms of readback, and 17.0 ms of GPU execution. The runtime also reached its texture budget and reported recurring failures to rebase 12-byte typed buffer views. The effect of those view failures on the image remains unmeasured.

At 18:21:19 UTC, about 9 min 19 s after launch, multiple threads in the title hit the same breakpoint and the process exited with `0x80000003`. The causal link between the missing compute pipelines and the breakpoint remains unverified.

For a controlled test of upload workers and parallel preludes, use the same save and scene with equally warmed caches. Set both variables before starting the launcher so its child process inherits them.

```powershell
$env:VICTORY_PARALLEL_PRELUDES = '1'
$env:VICTORY_FILL_WORKERS = '4'
& "$env:LOCALAPPDATA\DexNom Runtime\victory-play.exe"
```
