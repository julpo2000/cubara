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
