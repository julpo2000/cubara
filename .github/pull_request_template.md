<!-- Title: use Conventional Commits, e.g. `feat(render): ...`, `perf(world): ...`, `docs: ...` -->

## What

<!-- One paragraph: what this changes and why. Reference the issue it closes. -->

Closes #

## How

<!-- The key implementation points a reviewer needs. -->

## Verification

<!-- How you know it works — the engine values observing behaviour, not just tests: -->

- [ ] `cargo test --all`, clippy, rustfmt green
- [ ] Behaviour checked (screenshot / bench / driven flow), not only unit tests
- [ ] Perf-relevant: BENCHMARKS.md row added, with the delta vs the previous row

<!-- Delete if N/A. CI must be green on all three required checks (clippy+build+test
on macOS and Windows, rustfmt) before merge. -->
