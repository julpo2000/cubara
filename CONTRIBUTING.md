# Contributing to Cubara

Cubara is built with a deliberately professional process (PLAN.md §9): every change
lands through an issue and a reviewed, CI-green PR. This document is the short
version of how that works.

## Workflow

1. **An issue first.** Features and tasks are tracked as GitHub issues (use the
   *Feature / task* template — Goal / Scope / Done when). Bugs use the *Bug report*
   template. Larger efforts are a tracking issue with linked sub-issues.
2. **A feature branch.** `main` is protected — never commit to it directly. Branch
   off `main` (`feat/…`, `perf/…`, `fix/…`, `docs/…`, `chore/…`).
3. **A PR.** Open a pull request against `main` using the PR template. Reference the
   issue it closes (`Closes #123`).
4. **Green CI, then merge.** A PR merges only once all **required status checks**
   pass, then squash-merges and deletes the branch.

## Commits & PR titles

[Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`,
`perf:`, `docs:`, `chore:`, `refactor:`, `test:`, optionally scoped
(`feat(render): …`). The PR title follows the same convention (it becomes the squash
commit).

## Required checks

Three checks gate every merge (GitHub Actions):

- `clippy + build + test (macos-latest)` — Metal backend
- `clippy + build + test (windows-latest)` — DX12/Vulkan backend
- `rustfmt`

Run them locally before pushing:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test --all
```

## Verify behaviour, not just tests

The engine values *observing* a change working, not only that tests pass. Depending
on the change that means a `--screenshot`, a `--bench` run, or driving the affected
flow — see the built-in launch modes in [`README.md`](README.md).

## Performance discipline

Cubara targets a genuinely fast engine, so **every performance-relevant feature is
measured**, not just checked against the 1000-FPS gate:

- Run `cargo run --release -- --bench [radius]` and read the `SUMMARY:` line.
- Append a row to the machine's table in [`BENCHMARKS.md`](BENCHMARKS.md) (feature,
  chunks, tris, FPS, CPU/frame avg + p99, commit).
- Report the numbers **and the delta vs the previous row**, not just "gate met".

FPS is noisy at small/submit-bound scenes; lean on **CPU/frame** as the comparable
metric until a feature makes the scene genuinely CPU- or GPU-bound. See the notes at
the top of `BENCHMARKS.md`.

## Milestones

Each phase (M1, M2, … see PLAN.md §7) is a GitHub milestone; issues are filed under
the milestone they belong to. `platform:` labels mark whether work is cross-platform
(one wgpu code path) or specific to a backend.

## Copyright

Cubara is an original project. Do not copy assets, names, code, or branding from
existing games (PLAN.md §1). Mechanics may be inspired; content is our own.
