# Cubara — Benchmark Results

Log of `cargo run --release -- --bench` runs (see [`README.md`](README.md)). The
M1 gate from [`PLAN.md`](PLAN.md) is **1000+ FPS** in the standard benchmark
scene; this file tracks how comfortably we clear it across the machines/
platforms we actually develop on (currently a Windows desktop/laptop and a
macOS M3 machine), so we can spot platform-specific regressions early.

Scene is whatever `World::generate()` currently builds — chunk count and
triangle count will drift as worldgen changes, so they're recorded per row
rather than assumed constant.

## How to record a run

```bash
cargo run --release -- --bench
```

Copy the `GPU:`, `world:`, and `BENCHMARK RESULT` lines from the log into a new
row below. Include the commit hash (`git rev-parse --short HEAD`) so results
stay comparable across engine changes.

## Results

| Date | Platform | GPU | Backend | Chunks | Triangles | FPS (sustained) | CPU/frame avg | CPU/frame p99 | Commit |
|---|---|---|---|---|---|---|---|---|---|
| 2026-07-18 | Windows 11, i7-12650H | NVIDIA GeForce RTX 4060 Laptop GPU | Vulkan | 137 | 22,788 | 8097 | 0.083 ms | 0.350 ms | `0ab6034` |

### 2026-07-18 — Windows 11 desktop/laptop (RTX 4060 Laptop GPU)

```
GPU: AdapterInfo { name: "NVIDIA GeForce RTX 4060 Laptop GPU", vendor: 4318, device: 10400, device_type: DiscreteGpu, driver: "NVIDIA", driver_info: "581.42", backend: Vulkan }
world: 137 chunks meshed, 22788 triangles
rendering 1920x1080, 137 chunk draw calls
=========== BENCHMARK RESULT ===========
frames            : 2000
throughput        : 8097 FPS (sustained, pipelined)
CPU submit / frame: avg 0.083 ms | p50 0.064 | p99 0.350
chunks drawn      : avg 137.0 / 137 (frustum-culled)
========================================
goal 1000+ FPS: MET
```

**Notes:** first benchmark run after setting up the toolchain (Git + rustup)
fresh on this machine. 8.1k FPS is ~8x the M1 gate — CPU submit cost is
essentially noise at 0.08 ms/frame, so at this scene size we're nowhere near
CPU- or GPU-bound. Next interesting data point: the same commit on the M3 Mac
(8 GB RAM) to see how much of that headroom survives on a much smaller/
integrated-GPU machine.
