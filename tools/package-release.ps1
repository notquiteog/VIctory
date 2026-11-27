# Builds the two programs and packs what a release of VI consists of, into -OutDir:
#   VI-Setup.exe                   the one-file download: the launcher with victory.exe packed inside it.
#                                  Double-click, Install. (The primary asset.)
#   VI-<version>-windows-x64.zip   VI-Setup.exe (the launcher alone), victory.exe, LICENSE, README.txt.
#                                  What the launcher's updater downloads.
#   SHA256SUMS                     the SHA-256 of both; the updater refuses a zip that does not match.
#   build.json                     { "version", "commit", "date" }: which commit this was built from.
#                                  The "Latest build" update channel compares it with the installed one.
#   release-notes.md               the notice every release carries, to go on top of the release's notes.
# and prints the `gh release create` command for a person to run. It publishes nothing itself.
# The same script is run by .github/workflows/build-release.yml.
#
#   tools\package-release.ps1 [-OutDir <folder>] [-Jobs 6] [-SkipBuild] [-Plain] [-TargetDir <cargo target dir>]
#
# victory.exe is built as the fork is played and measured: the release profile with the `diagnostics`
# feature (the same code as `--profile diag --features diagnostics`). -Plain leaves the feature out.
#
# The files hold programs and text only: no game data, no vendor (NVIDIA/AMD) libraries, no logs, no settings.
# The Stable channel offers a release whose tag is a later version than the installed launcher's, and
# that is [workspace.package] version in Cargo.toml: raise it first, then tag v<the same number>.
param(
    [string]$OutDir,
    [string]$TargetDir,
    [int]$Jobs = 0,
    [switch]$SkipBuild,
    [switch]$Plain,
    [string]$Repo = 'notquiteog/VIctory'
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$notice = 'VI is an unofficial community project. It is not affiliated with, endorsed by or supported by Rockstar Games or Take-Two Interactive. It contains no game code or data; you must own the game.'

# A public package carries the drawn default logo, never a builder's own picture
# (apps/victory-launcher/README.md).
if ($env:VICTORY_LAUNCHER_LOGO) { throw 'VICTORY_LAUNCHER_LOGO is set: a build with a logo of your own is not for publishing. Clear it and run again.' }

$cargoToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { throw 'No [workspace.package] version in Cargo.toml.' }
$version = $Matches[1]

# The commit the programs are built from (the build scripts read the same two variables).
Push-Location $root
try {
    if (-not $env:GIT_COMMIT) { $env:GIT_COMMIT = (& git rev-parse HEAD).Trim() }
    if (-not $env:GIT_COMMIT_DATE) { $env:GIT_COMMIT_DATE = (& git show -s --format=%cs HEAD).Trim() }
} finally { Pop-Location }
if ($env:GIT_COMMIT -notmatch '^[0-9a-f]{40}$') { throw "Could not tell the commit (got '$($env:GIT_COMMIT)'). Run from a git checkout or set GIT_COMMIT and GIT_COMMIT_DATE." }

if (-not $TargetDir) { $TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' } }
if (-not $OutDir) { $OutDir = Join-Path $TargetDir 'dist' }

if (-not $SkipBuild) {
    $cargoArgs = @('build', '--locked', '--release', '-p', 'victory', '-p', 'victory-launcher', '--bin', 'victory', '--bin', 'victory-launcher')
    if (-not $Plain) { $cargoArgs += @('--features', 'victory/diagnostics') }
    if ($Jobs -gt 0) { $cargoArgs += @('-j', "$Jobs") }
    Push-Location $root
    try {
        $env:CARGO_TARGET_DIR = $TargetDir
        # (cargo reports on stderr; with output redirected, Windows PowerShell would take that for an error.)
        $ErrorActionPreference = 'Continue'
        & cargo @cargoArgs
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw "cargo build failed ($code)." }
    } finally { Pop-Location }
}

$bin = Join-Path $TargetDir 'release'
$launcher = Join-Path $bin 'victory-launcher.exe'
$victory = Join-Path $bin 'victory.exe'
foreach ($exe in $launcher, $victory) {
    if (-not (Test-Path $exe)) { throw "No $exe. Build first (run without -SkipBuild)." }
}
# The programs say which commit they are of: do not pack a build of another one.
$says = (& $victory --version | Out-String).Trim()
if ($says -notlike "*$($env:GIT_COMMIT)*") { throw "$victory says '$says', not commit $($env:GIT_COMMIT). Build again (run without -SkipBuild)." }

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$name = "VI-$version-windows-x64"
$stage = Join-Path $OutDir $name
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item $launcher (Join-Path $stage 'VI-Setup.exe')
Copy-Item $victory $stage
Copy-Item (Join-Path $root 'LICENSE') $stage
@"
VI $version  (commit $($env:GIT_COMMIT.Substring(0, 7)), $($env:GIT_COMMIT_DATE))

VI runs your own Xbox Series copy of the game on a Windows PC.

$notice

What you need
  Your own, legally obtained copy of the game's files. This download contains no game data, and
  none of NVIDIA's or AMD's libraries (DLSS, FidelityFX): if you want those, you supply them.
  Windows 10 or 11 (64-bit), a Direct3D 12 graphics card, and about 64 GB of RAM plus page file.

To install
  Start VI-Setup.exe and press Install. It installs for your Windows user only (no administrator
  rights), puts a "VI" shortcut on the Desktop and in the Start menu, and asks for the folder of
  your copy of the game. Your game files are only read: never copied, moved or changed.

To uninstall
  Windows Settings > Apps > Installed apps > VI > Uninstall, or the Install tab of VI. Your saved
  games and settings are kept unless you tick the box to delete them.

Where things are
  Program:              %LOCALAPPDATA%\Programs\VI
  Settings, saves, cache: %LOCALAPPDATA%\VIctory
  Logs:                 %LOCALAPPDATA%\VIctory\logs  (the "Open the logs folder" button on the Play tab)

Source and licence (GPL-3.0-or-later, see LICENSE): https://github.com/$Repo
"@ | Set-Content (Join-Path $stage 'README.txt') -Encoding ascii

# Programs and text only: anything else in the folder is a mistake.
$allowed = 'VI-Setup.exe', 'victory.exe', 'LICENSE', 'README.txt'
$found = @(Get-ChildItem $stage -Recurse -Force | ForEach-Object { $_.Name })
$extra = @($found | Where-Object { $_ -notin $allowed })
if ($extra.Count -gt 0 -or $found.Count -ne $allowed.Count) { throw "The release folder should hold exactly $($allowed -join ', '); it holds $($found -join ', ')." }

$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal

# The one-file setup program: the launcher writes itself with victory.exe packed behind it.
$setup = Join-Path $OutDir 'VI-Setup.exe'
if (Test-Path $setup) { Remove-Item $setup -Force }
$made = Start-Process -FilePath $launcher -ArgumentList '--make-setup', "`"$victory`"", "`"$setup`"" -Wait -PassThru
if ($made.ExitCode -ne 0 -or -not (Test-Path $setup)) { throw "Making $setup failed (exit code $($made.ExitCode))." }

$ascii = New-Object System.Text.ASCIIEncoding
$sums = Join-Path $OutDir 'SHA256SUMS'
$lines = foreach ($file in $zip, $setup) { "$((Get-FileHash $file -Algorithm SHA256).Hash.ToLowerInvariant())  $(Split-Path $file -Leaf)" }
# (As sha256sum writes it: no byte-order mark, line feeds.)
[System.IO.File]::WriteAllText($sums, (($lines -join "`n") + "`n"), $ascii)

$build = Join-Path $OutDir 'build.json'
[System.IO.File]::WriteAllText($build, "{ `"version`": `"$version`", `"commit`": `"$($env:GIT_COMMIT)`", `"date`": `"$($env:GIT_COMMIT_DATE)`" }`n", $ascii)

$notes = Join-Path $OutDir 'release-notes.md'
[System.IO.File]::WriteAllText($notes, "> $notice`n`nDownload **VI-Setup.exe**, start it and press Install. Built from commit $($env:GIT_COMMIT) ($($env:GIT_COMMIT_DATE)).`n", $ascii)

Remove-Item $stage -Recurse -Force

$mb = { param($f) "$([math]::Round((Get-Item $f).Length / 1MB, 1)) MB" }
''
"VI $version, commit $($env:GIT_COMMIT) of $($env:GIT_COMMIT_DATE)"
"  $setup  ($(& $mb $setup))"
"  $zip  ($(& $mb $zip))"
"  $sums"
"  $build"
"  $notes"
''
'To publish (not done by this script):'
"  gh release create v$version `"$setup`" `"$zip`" `"$sums`" `"$build`" --repo $Repo --target $($env:GIT_COMMIT) --title `"VI $version`" --notes-file `"$notes`" --generate-notes"
