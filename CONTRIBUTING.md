# Contributing to Cubara

Cubara is built with a deliberately professional process (PLAN.md §9): every change
lands through an issue and a reviewed, CI-green PR. This document is the short
version of how that works.

## The one rule behind the rules

**A requirement that isn't enforced by machinery is a wish.**

This project has proof. Every rule wired to something that says *no* — branch
protection, the three required checks, the PR template — has held without
exception. The rule that lived only in prose (REQUIREMENTS.md #3, "features must
not sit loosely side by side") was violated until the renderer had three divergent
copies of itself, because a paragraph has never blocked a merge.

So: when you want a new rule, your first question is *what fails when someone
breaks it?* If the answer is "nothing, but the docs say not to", write a check
instead — a test, a CI job, or a structure that makes the wrong thing impossible
to express. Prose is for explaining the check, not for replacing it.

## Workflow

1. **An issue first.** Features and tasks are tracked as GitHub issues (use the
   *Feature / task* template). Bugs use the *Bug report* template. Larger efforts
   are a tracking issue with linked sub-issues.

   Issues must meet [`docs/ISSUE_STANDARD.md`](docs/ISSUE_STANDARD.md): a reader
   who has never seen this repo should be able to implement it without asking a
   question. Cubara is built largely by AI agents, which cannot ask — faced with
   an ambiguity they guess, and a plausible wrong guess is expensive. **An issue
   missing its Design decisions or Out of scope section is not ready to assign.**
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

## Verify behaviour automatically

The engine values *observing* a change working, not only that tests pass. But
"observing" means **a check that runs in CI**, not a human looking at a window
once. A screenshot someone eyeballed proves nothing about the next commit.

- **Rendering changes** ship with a golden-image test: render a fixed scene
  headlessly, compare against a committed reference within tolerance, write a diff
  image on failure. Reference images are regenerated deliberately, never silently.
- **Logic** belongs in pure functions with unit tests, outside the GPU path.
  Anything that *can* be tested without a GPU must be.
- **Performance** is a `--bench` run and a `BENCHMARKS.md` row.

Manual driving is for exploration, not for evidence. If the only way to know a
feature works is to launch it and look, the feature is not finished.

### One scene, one render path

There is exactly **one** function that renders the scene. The window, `--bench`,
and `--screenshot` are thin callers of it, and a CI check enforces that they stay
that way. This is not a style preference: when those paths forked, features landed
in one and were invisible to the others, and `--screenshot` silently stopped
proving anything. Adding a fourth caller is fine; adding a second scene-render
implementation is not.

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
