# Cubara

A modern, high-performance voxel engine and survival game. Built for practically
infinite render distance (with LOD), stable frame times, and an architecture that
scales from "chopping trees" to a full world with thousands of blocks, processes,
shaders, multiplayer, and mods.

> Cubara is an original project. It is not a clone and uses no assets, names, or
> code from existing games.

## Status

Pre-alpha — engine foundation under construction. See [`PLAN.md`](PLAN.md) for the
vision, architecture, and roadmap.

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
```

Stack: Rust + [`wgpu`](https://wgpu.rs) (Metal on macOS, DX12/Vulkan on Windows) +
`winit`. See `PLAN.md` for architecture and the milestone roadmap.
