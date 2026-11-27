# Starts the title with this fork's settings, on any AMD or NVIDIA card (README.md, "Run it").
#   scripts\play.ps1 -Exe <path of your own copy's executable>
#
# By default: the Series S profile (1920x1080 output), the world drawn at half that height (540p) and brought up by
# the title's own upscaler, with native ray tracing on. These are the settings measured fastest.
#
# -SeriesX       the Series X profile (3840x2160 output) instead of Series S: sharper, about half as fast
# -Resolution    the height the world is drawn at before the upscaler: 2160, 1440, 1080, 720, 540 or 480.
#                Default: half the output (540 on Series S, 1080 on Series X). Series S does not start at its full
#                1080 (the title stops at a breakpoint of its own), so 720 is the most there.
# -NoRayTracing  without the title's own ray tracing (docs\fork-ray-tracing.md): the structure builder off, the passes
#                that would trace skipped (scripts\rt_off_hashes.txt, diag build only), the ambient stand-in instead
# -Safe          without the newer, faster submission paths: the first thing to try when something is wrong
# -Adapter       on a PC with more than one card: part of its name or its index (VICTORY_D3D12_ADAPTER)
# -SmoothMotion  NVIDIA only: start a copy of the build named victory_sm.exe, so that a Smooth Motion profile you set
#                for that name in the NVIDIA App applies (the driver's frame generation; nothing is downloaded)
# -Build         target\diag (the default, the build all measurements are of) or target\release
# -More          other settings: -More @{VICTORY_PANEL='1'}
# -RayTracing    accepted and ignored (ray tracing is the default); kept so older command lines still work
#
# Run it from a normal window, not as administrator: the driver then keeps its shader cache elsewhere.
param(
    [Parameter(Mandatory)][string]$Exe,
    [switch]$SeriesX,
    [ValidateSet('2160', '1440', '1080', '720', '540', '480')][string]$Resolution,
    [switch]$NoRayTracing,
    [switch]$Safe,
    [string]$Adapter,
    [switch]$SmoothMotion,
    [ValidateSet('diag', 'release')][string]$Build = 'diag',
    [hashtable]$More = @{},
    [switch]$RayTracing
)
$settings = @{
    VICTORY_TEXTURE_NO_WAIT = '1'; VICTORY_RELAY = '2'; VICTORY_WIDE_BACK = '1'; VICTORY_CARD_REHASH_RUNS = '1'
    VICTORY_JOIN_COPIES = '1'; VICTORY_INDIRECT_WINDOW = '1'; VICTORY_RELAY_COPY = '1'; VICTORY_CARD_STEPS = '2'
    VICTORY_INDIRECT_BATCH = '2'; VICTORY_MAP_THREADS = '1'; VICTORY_TITLE_PRIORITIES = '0'; VICTORY_CARD_AWAKE = '1'
    VICTORY_SHOW_AFTER = '1'; VICTORY_AMBIENT = '0.8'; VICTORY_AMBIENT_MIN = '0.08'; VICTORY_D3D12_DRED = 'light'
    VICTORY_SKIP_PIPELINES = 'none'; VICTORY_PIPELINE_BOOK = '1'
    # The title's own temporal upscaler on every profile (Series S would pick FSR, whose programs are not translated:
    # white blocks in the picture), and its render scale fixed (victory-loader hooks.rs).
    VICTORY_OWN_UPSCALER = '1'; VICTORY_RENDER_SCALE = '0.5'
    # Series S presents the interface as a second plane: laid over the game (compose.rs), else black before gameplay.
    VICTORY_COMPOSE = '1'
}
if ($Safe) {
    $settings.VICTORY_UNDER_WIDE = '0'; $settings.VICTORY_LAND_ECHO = '0'
} else {
    $settings.VICTORY_THIN = '1'; $settings.VICTORY_JOIN_PARTS = '8'
}
if ($NoRayTracing) {
    $settings.VICTORY_BVH_BUILDER = '0'
    $rtList = Join-Path $PSScriptRoot 'rt_off_hashes.txt'
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
$rays = if ($settings.VICTORY_BVH_BUILDER -eq '0') { 'off' } else { 'on' }
Write-Host "Series $console, the world drawn at $([int]($outHeight * $scale))p, ray tracing $rays"

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
