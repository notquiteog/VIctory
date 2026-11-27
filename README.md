# VIctory for AMD and NVIDIA

A fork of [VIctory](https://github.com/Marvisak/VIctory), which runs an Xbox Series title on Windows from files you supply. This fork targets **AMD and NVIDIA cards**. The goal is a steady 30 fps with the title's own ray tracing on. It isn't there yet.

**No game files, code or assets are included.** You need your own copy of the game. This project is not affiliated with or endorsed by the game's developer or publisher.

## Status

- Native ray tracing is **on by default**. To turn it off, use `-NoRayTracing`.
- Walking outdoors as Series S at half resolution, frames take about 50–90 ms (11–20 fps), and the CPU is the limit.
- You need 64 GB of RAM: the game uses 25–45 GB.
- The first run after a new driver or build stutters while shaders are compiled. `victory warm` compiles them ahead of time.

## Run it

You need:

- Windows 10 or 11, x64
- a CPU with AVX2
- a D3D12 GPU with SM 6.6 and mesh shaders
- Rust and the MSVC build tools
- your own dump of the title

```powershell
git clone https://github.com/notquiteog/victory-nvidia
cd victory-nvidia
copy victory.example.toml victory.toml   # set your paths
cargo build --locked --profile diag -p victory --features diagnostics
target\diag\victory.exe shaders <shader archives>   # once, takes minutes
scripts\play-nvidia.ps1 -Exe <your copy's executable>   # NVIDIA
scripts\play.ps1 -Exe <your copy's executable>          # AMD
```

Useful flags for `play-nvidia.ps1` (`play.ps1` also takes `-NoRayTracing`):

| Flag | What it does |
|---|---|
| `-SeriesX` | Uses the Series X profile: sharper, about half the speed. |
| `-Resolution 720` | Sets the internal render height. |
| `-NoRayTracing` | Uses the ambient stand-in for indirect light instead of ray tracing. |

Every `VICTORY_*` setting is documented where the code reads it.

## Docs

- [docs/fork-ray-tracing.md](docs/fork-ray-tracing.md): how ray tracing works here, and its switches.
- [docs/findings.md](docs/findings.md): performance work and measurements.
- [docs/nvidia-rtx5060.md](docs/nvidia-rtx5060.md): settings for NVIDIA cards.
- [docs/replay.md](docs/replay.md): recording and replaying frames.
- [docs/README-fxceless.md](docs/README-fxceless.md): commands and debugging.

## Credits

This builds on other people's work. Commits taken from other forks name their source and author.

- [Marvisak/VIctory](https://github.com/Marvisak/VIctory) and its contributors: the core layer.
- [avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX), [fxceless/victory-fork](https://github.com/fxceless/victory-fork), [DexNom/VIctory](https://github.com/DexNom/VIctory), [Belobug](https://github.com/Belobug/VIctory-Belobug-AMD), [DoubleGhosty/cool_fox](https://github.com/DoubleGhosty/cool_fox), [trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA), myokex, FQY55kai, Geckomacabre and others.
- Made with [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE) and [docs/asset-policy.md](docs/asset-policy.md).
