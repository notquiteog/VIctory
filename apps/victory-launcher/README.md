# victory-launcher

The window players start the game from: install, first-run setup, Play,
settings, shader and pipeline preparation with progress, and updates. It only
ever starts `victory.exe` with arguments and `VICTORY_*` environment variables.

VI is an unofficial community project. It is not affiliated with, endorsed by or
supported by Rockstar Games or Take-Two Interactive. It contains no game code or
data; you must own the game.

```powershell
cargo build --profile diag -p victory-launcher
cargo test -p victory-launcher
target\diag\victory-launcher.exe --victory-exe target\diag\victory-stub.exe   # try it without the game
```

`victory-stub.exe` (this crate, not shipped) stands in for `victory.exe`: it
prints what it was given and counts up as `shaders` and `warm` do
(`VICTORY_STUB_TOTAL`, `VICTORY_STUB_MS`, `VICTORY_STUB_FAIL`; for `run`,
`VICTORY_STUB_SECONDS` and `VICTORY_STUB_CRASH=1`, set in the Advanced box).

## Play is `scripts/play.ps1`

Play starts a session the way `scripts\play.ps1` does, with the same
`VICTORY_*` environment, arguments and steps; each setting is one of its flags:

| Setting | `play.ps1` |
|---|---|
| Console: Series S (default) / Series X | (default) / `-SeriesX` |
| World drawn at: half the picture (default) / a height | (default) / `-Resolution` |
| Ray tracing on (default) / off | (default) / `-NoRayTracing` |
| Preset: Recommended / Safe / Custom | (default) / `-Safe` / none of its set |
| Graphics card | `-Adapter` |
| NVIDIA Smooth Motion | `-SmoothMotion` (a copy named `victory_sm.exe` beside `victory.exe`) |
| Compiling pipelines ahead: used + rest after (default) / all before / never | (default) / `-WarmAll` / `-NoWarm` |
| Advanced lines | `-More` |

So a session is `victory warm --used`, `victory run <game> --execute
[--console series-s]`, then `victory warm` after the game closes (resumable;
Stop leaves the rest for next time). The launcher adds `--progress` to the
steps for its progress bar, sets `VICTORY_GAME_DIR`, `VICTORY_CACHE_DIR` and
`VICTORY_DATA_DIR` from its settings, and takes every other `VICTORY_*` of the
window it was started from away. `--print-env <file>` writes the whole
session; when `play.ps1` changes, compare the two and follow it
(`settings.rs`: `BASE`, `RECOMMENDED`, `SAFE`).

A settings file of 0.1.0 (which wrote Series X, its default, into every file)
is brought to the new defaults once, with a note in the window.

## The logo

The icon and the picture in the window are **drawn by `build.rs`**: a dark
rounded tile with the letters "VI" in plain strokes. That is what public builds
carry. No artwork is stored in this repository.

A picture of your own can replace it, in two ways. Either way the file is
yours: it is not part of this repository and must not be redistributed with it
unless you hold the rights to it.

- **At build time:** set `VICTORY_LAUNCHER_LOGO=<path to a PNG>` before
  `cargo build`. The program's icon and the picture in the window are made from
  that file. Do not publish a build made this way with a picture you may not
  distribute; `tools/package-release.ps1` refuses to pack while the variable is
  set.
- **At run time:** put `logo.png` into `%LOCALAPPDATA%\VIctory\` (the *Choose
  logo...* button on the Setup page does that) or beside the launcher. The
  window shows it, and Install makes the Desktop and Start-menu shortcuts' icon
  from it. It stays on that PC.

## Hidden switches (tests and scripts)

| | |
|---|---|
| `--victory-exe <path>` | a `victory.exe` elsewhere than beside the launcher |
| `--install`, `--uninstall --yes [--remove-data]` | without a window |
| `--make-setup <victory.exe> <out.exe>` | write the one-file setup program |
| `--print-env <file>` | the Play command and its environment, as text |
| `--tab <name> --auto <prepare\|shaders\|warm-all\|play\|check-update\|update> --screenshot <png> --after <s>` | drive the window and save a picture of it |
| `VICTORY_INSTALL_DIR_OVERRIDE` | a trial install: everything below that folder |
| `VICTORY_LAUNCHER_HOME` | settings, logs and default cache/saves below that folder |
| `VICTORY_UPDATE_FEED` | a URL of release JSON, or a folder with `latest.json` / `nightly.json` |
