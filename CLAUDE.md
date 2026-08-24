# Working agreement (Claude Code)

## Read this before writing code: the architecture standard

[`ARCHITECTURE.md`](ARCHITECTURE.md) defines what "a solid engine" means here, as
seven rules. **Read it before the first edit of a session**, not after a PR is
open. It is short.

This section exists because of a measured failure. The performance discipline
below was followed on *every* feature that landed — because it is written here,
in the file that gets read at the start of the work. The architecture
requirement lived only in `REQUIREMENTS.md`, which nothing loads before coding,
and it was violated until the renderer had three divergent copies of itself, the
world's state was a global, and the pure data crate depended on `wgpu`. Same
project, same author, same good intentions; the only difference was *where the
rule was written*.

The load-bearing points:

- **Rule 1 — the simulation is deterministic.** Tick-driven, ordered iteration,
  seeded RNG as world state. The keystone, and the only rule that cannot be
  retrofitted.
- **Rule 2 — no ambient state.** No globals, no singletons. Pass state in.
- **Rule 3 — dependencies point one way.** The renderer does not own gameplay.
- **Rule 4 — the simulation runs with no GPU.** Data/sim crates never depend on
  `wgpu` or `winit`.
- **Rule 5 — one implementation per concern.** One scene-render path.
- **Rule 6 — behaviour is pinned by tests before it is rewritten.**

`scripts/check-architecture.sh` and `scripts/check-single-render-path.sh` enforce
these and run in CI as the `architecture rules` check. **Run them locally before
pushing.** They are greps and take under a second:

```bash
./scripts/check-architecture.sh && ./scripts/check-single-render-path.sh
```

They catch the violation *shapes* seen so far. They cannot catch a novel one, so
the standing review question is still: **what fails when someone breaks this?**
If a new rule has no answer, write a check rather than a paragraph.

## Then read this: what we are building right now

[`ROADMAP.md`](ROADMAP.md) says which of three phases is active, the ordered
blocks inside it, and the **exit gate** that decides when it is finished. Phase 1
also has a full design in
[`docs/PHASE1_ARCHITECTURE.md`](docs/PHASE1_ARCHITECTURE.md) — read it before
touching blocks, meshing, LOD or the tick loop; it has already made those
decisions, with the reasons. Phase 2's blocks 2.1 – 2.4 have the same thing in
[`docs/PHASE2_ARCHITECTURE.md`](docs/PHASE2_ARCHITECTURE.md) — items, inventory,
crafting, drops and tool tiers, trees, and the furnace. **The active phase has a
design doc; read that one.**

This is written here for the same reason the architecture standard is: the file
that gets read at the start of the work is the only place a rule actually holds.

The part that binds an autonomous session:

- **Work the phase's blocks in order.** Pick the next unblocked one; do not
  freelance a nearby improvement into a phase it does not belong to.
- **Do not move the exit gate.** If it cannot be met, report the measured number
  and the levers tried. Changing the gate is the owner's call, never the agent's.
- **A phase ends with a report, not with the next phase** — gate output,
  benchmark deltas, what was built, what was left undone. Then the owner plays it.

### "Go ahead" — the autonomous session procedure

When the owner starts a session with nothing more than *go ahead* / *ga maar*,
that means: **work the active phase, block by block, until the phase ends or you
hit a stop condition.** Concretely, and in this order:

1. `./scripts/next-block.sh` — prints the next block and the issues in it. This
   is not a suggestion; it is the order. Do not re-derive it.
2. Read the issue. Its **Design decisions** section is binding. If implementing
   it requires a decision the issue does not contain, that is a defect in the
   issue — fix the issue first, or stop and ask if it is a gameplay decision.
3. Branch, implement, test, `cargo fmt`, clippy, `./scripts/check-architecture.sh
   && ./scripts/check-single-render-path.sh`, PR, wait for CI, merge, delete
   branch. Close the issue.
4. Perf-relevant block? `cargo run --release -- --bench 64`, append the
   `BENCHMARKS.md` row with the delta, in the same PR.
5. Back to 1. Repeat until `next-block.sh` says there are none left.
6. Then run the gate and **report**. Do not start the next phase.

**Stop conditions — end the session and report, rather than pushing through:**

- A gameplay or content decision the roadmap and design docs do not settle.
- The exit gate is unreachable and the levers in the design doc are exhausted.
- A block turns out to need a decision that contradicts
  the active phase's design doc. Say so in the PR and change the design doc in
  that PR, with reasoning — do not silently diverge from it.
- CI fails for a reason you do not understand. Flag it; do not disable the check.

**Never:** regenerate a golden reference image to make a test pass without saying
so explicitly in the PR and explaining what changed and why the new image is
correct. A silently regenerated golden is the same as having no test.

## Design decisions are the project owner's

Engineering process — crate layout, refactors, test strategy, CI — is yours to
decide and act on without asking.

**Gameplay and roadmap are not.** What the game contains, which mechanics it
uses, what a release includes, and in what order: ask, and design it *as a
system* rather than one feature at a time. Do not invent a roadmap, a feature
list, or a mechanic and write it into the repo as though it were settled. A
plausible-sounding invention is worse than a question, because it looks decided.

## Verification means an automated check

A screenshot someone looked at once proves nothing about the next commit.
Rendering changes ship with a golden-image test; logic lives in pure functions
with unit tests, outside the GPU path. See `CONTRIBUTING.md`.

Do not drive the app with synthetic OS-level input (AppleScript keystrokes and
similar) to "verify" something — it lands on the user's real desktop, not
reliably in the app, and proves nothing either way.

## Installing software

**Never install anything (winget, cargo tools, global npm/pip packages, etc.)
without asking first and getting explicit confirmation** — even when it seems
like the obvious next step to unblock a task. Ask, wait for a yes, then
install. This applies every time, not just once per tool.

## GitHub workflow — handle it yourself

Once code is ready, **own the full GitHub lifecycle without asking at each
step**: create the branch, commit, push, open the PR, watch CI, and merge once
it's green. Don't come back to ask permission for each of those individual
git/gh actions — that's the point of this agreement. Do still flag anything
unusual (failing CI, conflicts, anything destructive/irreversible) rather than
pushing through it silently.

Repo facts that make this work:
- `main` is a **protected branch**: direct pushes are rejected. Everything
  lands via a PR.
- A PR only merges once the **required status checks** are green: `clippy +
  build + test (macos-latest)`, `clippy + build + test (windows-latest)`,
  `rustfmt`, and `architecture rules`.
- `gh` (GitHub CLI) is installed and authenticated as `julpo2000` — use it for
  PR creation/merging (`gh pr create`, `gh pr checks`, `gh pr merge`) instead
  of asking the user to click through the GitHub web UI.
- Pass PR bodies via `gh pr create --body-file <path>` (a scratchpad file), not
  inline `--body`: PowerShell mangles multi-line/parenthesised inline strings.

## Performance tracking — report perf per feature

The goal is a genuinely fast engine, so **every feature that lands is measured,
not just checked against the 1000-FPS gate**. After a feature is done:
- Run `cargo run --release -- --bench` and read its `SUMMARY:` line.
- Append a row to the machine's table in [`BENCHMARKS.md`](BENCHMARKS.md)
  (milestone/feature, chunks, tris, FPS, CPU/frame avg + p99, commit).
- When reporting the feature back to the user, **give the numbers and the delta
  vs the previous row**, not just "gate MET".

Measurement caveats to keep honest: at small/submit-bound scenes **FPS is noisy
and ramps upward across back-to-back runs** (CPU/GPU clock boost — the 200-frame
warmup is too short to reach steady clocks), so lean on **CPU/frame** as the
comparable metric until a feature makes the scene heavy enough to be CPU/GPU
bound. Record Windows numbers automatically; the macOS M3 row is added when the
user runs it there.
