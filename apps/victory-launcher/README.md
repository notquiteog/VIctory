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
prints what it was given and counts up as `shaders` and `warm` do.

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
| `--tab <name> --auto <prepare\|shaders\|play\|check-update\|update> --screenshot <png> --after <s>` | drive the window and save a picture of it |
| `VICTORY_INSTALL_DIR_OVERRIDE` | a trial install: everything below that folder |
| `VICTORY_LAUNCHER_HOME` | settings, logs and default cache/saves below that folder |
| `VICTORY_UPDATE_FEED` | a URL of release JSON, or a folder with `latest.json` / `nightly.json` |
