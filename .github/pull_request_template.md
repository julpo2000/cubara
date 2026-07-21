<!-- Title: use Conventional Commits, e.g. `feat(render): ...`, `perf(world): ...`, `docs: ...` -->

## What

<!-- One paragraph: what this changes and why. Reference the issue it closes. -->

Closes #

## How

<!-- The key implementation points a reviewer needs. -->

## Verification

<!-- How you know it works — the engine values observing behaviour, not just tests: -->

- [ ] `cargo test --all`, clippy, rustfmt green
- [ ] `./scripts/check-architecture.sh && ./scripts/check-single-render-path.sh`
- [ ] Behaviour verified by an **automated check**, not only by looking at it
- [ ] Perf-relevant: BENCHMARKS.md row added, with the delta vs the previous row

<!-- Architecture (ARCHITECTURE.md): if this PR introduces a global, a second
implementation of something that exists, a GPU dependency in a data/sim crate, or
gameplay on the renderer — say why here. "The check passed" is not the same as
"this respects the boundaries"; the checks catch known shapes, not new ones. -->

<!-- Delete if N/A. CI must be green on all three required checks (clippy+build+test
on macOS and Windows, rustfmt) before merge. -->
