# Cubara

A modern, high-performance voxel engine and survival game. Built for practically
infinite render distance (with LOD), stable frame times, and an architecture that
scales from "chopping trees" to a full world with thousands of blocks, processes,
shaders, multiplayer, and mods.

> Cubara is an original project. It is not a clone and uses no assets, names, or
> code from existing games.

## Status

Pre-alpha — engine foundation under construction.

| Document | What it answers |
|---|---|
| [`REQUIREMENTS.md`](REQUIREMENTS.md) | Why the project exists — the founding, non-negotiable wishes. |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | **The engineering standard** — the rules the codebase holds to, and what enforces each one. |
| [`ROADMAP.md`](ROADMAP.md) | **What ships, and when** — the three phases, their exit gates, and how autonomous work stays inside one. |
| [`docs/PHASE1_ARCHITECTURE.md`](docs/PHASE1_ARCHITECTURE.md) | The design phase 1 implements — budgets, block identity, LOD, the tick seam. |
| [`PLAN.md`](PLAN.md) | How the engine is built — technical decisions and the chunk/render approach. |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | How work lands: issues, PRs, required checks, verification. |
| [`docs/ISSUE_STANDARD.md`](docs/ISSUE_STANDARD.md) | How to write an issue someone can execute without asking questions. |
| [`BENCHMARKS.md`](BENCHMARKS.md) | Measured performance history, per machine, per feature. |

## Goals (in short)

- **Performance first** — 1000+ FPS in a simple world before we build gameplay.
- **A solid engine** — the engine is the foundation; everything builds on it.
- **Extensible & moddable** — data-driven blocks/items, clear module boundaries.
- **Multi-platform** — Windows + macOS now, more later.
- **Professional process** — git, issues, PRs, CI from day one.

## Build & run

Requires the [Rust toolchain](https://rustup.rs) (stable).

**Quick launch** — **double-click `run.command` (macOS) or `run.bat` (Windows)** in
your file explorer to build in release and start the game. Both also work from a
shell and pass any extra arguments straight through, so `./run.command --caps`,
`./run.command --bench 20`, `run.bat --screenshot out.png` all work too.

For the full set of modes, from the repo root:

```bash
# Run the app — opens a window rendering the current scene
cargo run --release

# Headless FPS benchmark — renders offscreen with no vsync, prints avg/p50/p99/1%-low
cargo run --release -- --bench

# With CPU profiling (puffin) — then connect the puffin_viewer app to 127.0.0.1:8585
cargo run --release --features profile -- --bench

# Render a single frame to a PNG (headless, no window)
cargo run --release -- --screenshot world.png

# Print GPU adapter capabilities (feature support for GPU-driven rendering)
cargo run --release -- --caps

# Run a world headlessly — no window, no GPU. `--help` lists the options.
cargo run --release --bin cubara-server -- --help
cargo run --release -- server --ticks 600      # the same thing, from the game binary
```

### The dedicated server

`cubara-server` runs a world with no window and no GPU: it loads the save, ticks
the simulation, autosaves, and shuts down cleanly. Furnaces smelt and dropped
items age out whether or not anyone is playing.

It is a **separate binary** as well as a `cubara server` subcommand, and the
difference matters on a host without a graphics stack — the subcommand still
links `wgpu` and `winit` even where it never opens a window:

| | Links | Size (release, macOS) |
|---|---|---|
| `cubara` | Metal, AppKit, QuartzCore, CoreVideo | 6.8 MB |
| `cubara-server` | `libSystem` only | 2.1 MB |

**There is no network transport yet.** This runs a world; it does not yet serve
one to anybody. See [`docs/RESEARCH_MULTIPLAYER.md`](docs/RESEARCH_MULTIPLAYER.md)
for the design it is being built towards.

Stack: Rust + [`wgpu`](https://wgpu.rs) (Metal on macOS, DX12/Vulkan on Windows) +
`winit`. See `PLAN.md` for architecture and the milestone roadmap.

## Contributing

Work lands via issues and CI-green PRs off `main`. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for the branch/commit/PR flow, the required
checks, and the performance-tracking discipline.
