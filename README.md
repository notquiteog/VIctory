# VIctory for AMD and NVIDIA

A fork of [VIctory](https://github.com/Marvisak/VIctory), which runs an Xbox Series title on Windows from files you supply. This fork targets **AMD and NVIDIA cards**. The goal is a steady 30 fps with the title's own ray tracing on. It isn't there yet.

**No game files, code or assets are included.** You need your own copy of the game. This project is not affiliated with or endorsed by the game's developer or publisher.

## Status

Measured walking outdoors on an NVIDIA card, Series S profile at half resolution (the defaults):

| | Frame time |
|---|---|
| Ray tracing on (default) | about 67–85 ms (12–15 fps); the GPU's ray-tracing structure builder is the main cost |
| Ray tracing off (`-NoRayTracing`) | about 45–58 ms (17–22 fps); the CPU is the main cost |

- Ray tracing is the title's own: real bounced light, reflections and clouds. A tree canopy sometimes shows as dark squares for a few seconds.
- You need 64 GB of RAM: the game uses 25–45 GB.
- Pipelines are compiled ahead of time. The game creates nearly all of its pipelines while loading, and every run records them.
  - Before each start, `play.ps1` compiles the ones earlier runs used. This takes about a second when nothing changed.
  - After the game closes, it compiles all the others. Ctrl+C stops it, and it picks up where it left off next time.
  - Only the very first run compiles as it goes.

## Run it

Without building anything: download **VI-Setup.exe** from [Releases](https://github.com/notquiteog/VIctory/releases), start it, press Install, and choose the folder of your copy of the game. Its Play button starts the game the way `scripts\play.ps1` does below, with the same defaults; the flags are on its Settings tab ([apps/victory-launcher](apps/victory-launcher/README.md)).

To build it yourself, you need:

- Windows 10 or 11, x64
- a CPU with AVX2
- a D3D12 GPU with SM 6.6 and mesh shaders
- Rust and the MSVC build tools
- your own dump of the title

```powershell
git clone https://github.com/notquiteog/VIctory
cd VIctory
copy victory.example.toml victory.toml   # set your paths
cargo build --locked --profile diag -p victory --features diagnostics
target\diag\victory.exe shaders <shader archives>   # once, takes minutes
scripts\play.ps1 -Exe <your copy's executable>
```

`scripts\play.ps1` works on AMD and NVIDIA cards. Its flags:

| Flag | What it does |
|---|---|
| `-SeriesX` | Uses the Series X profile: sharper, about half the speed. |
| `-Resolution 720` | Sets the internal render height (default: half the output, 540 on Series S). |
| `-NoRayTracing` | Uses the ambient stand-in for indirect light instead of ray tracing. |
| `-Safe` | Turns off the newer submission paths. Try it first if something breaks. |
| `-Adapter <name>` | Picks a card on a PC with more than one. |
| `-WarmAll` | Compiles every pipeline the game has ever created, not only the ones earlier runs used. Takes a long time the first time. |
| `-NoWarm` | Doesn't compile pipelines ahead of time, before the start or after the game closes. |
| `-SmoothMotion` | NVIDIA only: starts a copy named `victory_sm.exe`, so a Smooth Motion profile you set for that name in the NVIDIA App applies. |

Every `VICTORY_*` setting is documented where the code reads it.

## Docs

- [docs/fork-ray-tracing.md](docs/fork-ray-tracing.md): how ray tracing works here, and its switches.
- [docs/findings.md](docs/findings.md): performance work and measurements.
- [docs/replay.md](docs/replay.md): recording and replaying frames.
- [docs/README-fxceless.md](docs/README-fxceless.md): commands and debugging.

## Credits

This builds on other people's work. Commits taken from other forks name their source and author.

- [Marvisak/VIctory](https://github.com/Marvisak/VIctory) and its contributors: the core layer.
- [avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX), [fxceless/victory-fork](https://github.com/fxceless/victory-fork), [DexNom/VIctory](https://github.com/DexNom/VIctory), [Belobug](https://github.com/Belobug/VIctory-Belobug-AMD), [DoubleGhosty/cool_fox](https://github.com/DoubleGhosty/cool_fox), [trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA), myokex, FQY55kai, Geckomacabre and others.
- Made with [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE) and [docs/asset-policy.md](docs/asset-policy.md).
