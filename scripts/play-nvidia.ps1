# Starts the title on an NVIDIA card as the smaller console, drawn at part of its size: the settings measured fastest
# on an RTX 5060 Ti 16 GB (about 13-15 frames a second where scripts\play.ps1's give 6.5). After
# trustno1-coder/VIctory-NVIDIA's scripts/play-nvidia.ps1 (a1849fac2, d3cd650c1).
#   scripts\play-nvidia.ps1 -Exe <path of your own copy's executable>
# -SeriesX       the Series X profile (3840x2160 output) instead of Series S (1920x1080): sharper, about half as fast
# -Resolution    the height the world is drawn at before the title's own upscaler: 2160, 1440, 1080, 720, 540 or 480.
#                Default: half the output (540 on Series S, 1080 on Series X). Series S at its full 1080 does not
#                start (the title stops at a breakpoint of its own), so the most there is 720.
# -NoRayTracing  without the title's own ray tracing (on by default since 2026-11-26, docs\fork-ray-tracing.md):
#                the structure builder off (VICTORY_BVH_BUILDER=0), the passes that would trace skipped
#                (scripts\rt_off_hashes.txt; the skip list needs the diag build), the ambient stand-in in their place
# -RayTracing    (the default now; kept so older command lines still work)
# -SmoothMotion  start a copy of the build named victory_sm.exe, so that a Smooth Motion profile you set for that
#                name in the NVIDIA App applies (the driver's frame generation; nothing is downloaded)
# -Adapter       on a PC with more than one card: part of its name or its index (VICTORY_D3D12_ADAPTER)
# -Build         target\diag (the default) or target\release
# -More          other settings: -More @{VICTORY_PANEL='1'}
# Run it from a normal window, not as administrator: the driver then keeps its shader cache elsewhere.
param(
    [Parameter(Mandatory)][string]$Exe,
    [switch]$SeriesX,
    [ValidateSet('2160', '1440', '1080', '720', '540', '480')][string]$Resolution,
    [switch]$RayTracing,
    [switch]$NoRayTracing,
    [switch]$SmoothMotion,
    [string]$Adapter,
    [ValidateSet('diag', 'release')][string]$Build = 'diag',
    [hashtable]$More = @{}
)
$settings = @{
    # scripts\play.ps1's own set, without VICTORY_UPLOAD_AHEAD (the intro video plays right without it)
    VICTORY_TEXTURE_NO_WAIT = '1'; VICTORY_RELAY = '2'; VICTORY_WIDE_BACK = '1'; VICTORY_CARD_REHASH_RUNS = '1'
    VICTORY_JOIN_COPIES = '1'; VICTORY_INDIRECT_WINDOW = '1'; VICTORY_RELAY_COPY = '1'; VICTORY_CARD_STEPS = '2'
    VICTORY_INDIRECT_BATCH = '2'; VICTORY_MAP_THREADS = '1'; VICTORY_TITLE_PRIORITIES = '0'; VICTORY_CARD_AWAKE = '1'
    VICTORY_SHOW_AFTER = '1'; VICTORY_AMBIENT = '0.8'; VICTORY_D3D12_DRED = 'light'; VICTORY_SKIP_PIPELINES = 'none'
    VICTORY_THIN = '1'; VICTORY_JOIN_PARTS = '8'; VICTORY_PIPELINE_BOOK = '1'
    # The title's own temporal upscaler on every profile (Series S picks FSR, whose programs are not translated:
    # white blocks in the picture), and its render scale fixed (victory-loader hooks.rs).
    VICTORY_OWN_UPSCALER = '1'; VICTORY_RENDER_SCALE = '0.5'
    # Series S presents the interface as a second plane: laid over the game (compose.rs), else black before gameplay.
    VICTORY_COMPOSE = '1'
}
# Without ray tracing: the structure builder off, so every ray would miss; the passes that trace are skipped.
$rtList = Join-Path $PSScriptRoot 'rt_off_hashes.txt'
if ($NoRayTracing) {
    $settings.VICTORY_BVH_BUILDER = '0'
    if ($Build -eq 'diag' -and (Test-Path $rtList)) { $settings.VICTORY_SKIP_PIPELINES = (Get-Content $rtList -Raw).Trim() }
}
$outHeight = if ($SeriesX) { 2160 } else { 1080 }
$console = if ($SeriesX) { 'X' } else { 'S' }
if ($Resolution) {
    if ([int]$Resolution -gt $outHeight -or (-not $SeriesX -and [int]$Resolution -eq 1080)) {
        Write-Host "Series $console cannot be drawn at ${Resolution}p here (Series S: 720 at most; Series X: 2160)."
        return
    }
    $settings.VICTORY_RENDER_SCALE = ([int]$Resolution / $outHeight).ToString('0.####', [Globalization.CultureInfo]::InvariantCulture)
}
if ($Adapter) { $settings.VICTORY_D3D12_ADAPTER = $Adapter }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }
$scale = [double]::Parse($settings.VICTORY_RENDER_SCALE, [Globalization.CultureInfo]::InvariantCulture)
Write-Host "Series $console, the world drawn at $([int]($outHeight * $scale))p (render scale $($settings.VICTORY_RENDER_SCALE))"

$victory = Join-Path $PSScriptRoot "..\target\$Build\victory.exe"
if (-not (Test-Path $victory)) { Write-Host "No $victory. Build it first (README.md)."; return }
if (-not (Test-Path $Exe)) { Write-Host "No $Exe."; return }
if ($SmoothMotion) {
    # A copy under the name the NVIDIA App's profile is set for, refreshed when the build changes.
    $sm = Join-Path (Split-Path $victory) 'victory_sm.exe'
    if (-not (Test-Path $sm) -or (Get-Item $sm).LastWriteTime -lt (Get-Item $victory).LastWriteTime) {
        Copy-Item $victory $sm -Force
    }
    $victory = $sm
}
# (Other settings left over in this window go; where your files are stays.)
$paths = 'VICTORY_CONFIG', 'VICTORY_GAME_DIR', 'VICTORY_CACHE_DIR', 'VICTORY_DATA_DIR'
Get-ChildItem Env:VICTORY_* | Where-Object { $_.Name -notin $paths } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
foreach ($k in $settings.Keys) { Set-Item "Env:$k" $settings[$k] }
$Exe = (Resolve-Path $Exe).Path
$runArgs = @('run', $Exe, '--execute')
if (-not $SeriesX) { $runArgs += @('--console', 'series-s') }
Push-Location (Join-Path $PSScriptRoot '..')   # victory.toml is looked for here
try { & $victory @runArgs } finally { Pop-Location }
