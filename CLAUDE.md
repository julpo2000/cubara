# Working agreement (Claude Code)

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
- A PR only merges once **3 required status checks** are green: `clippy +
  build + test (macos-latest)`, `clippy + build + test (windows-latest)`,
  `rustfmt`.
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
