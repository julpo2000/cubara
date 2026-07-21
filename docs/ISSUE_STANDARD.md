# How to write a Cubara issue

An issue is a **work order**, not a reminder. The test it has to pass:

> Someone — a contributor, or an AI agent — who has never seen this repo picks up
> the issue, reads only it and the files it points at, and produces the right
> change without asking a single clarifying question.

If a question would have to be asked, the issue is not finished. Every ambiguity
left in an issue is a decision that gets made later, badly, by whoever is holding
the keyboard at the time.

## Why this is strict

Cubara is built largely by AI agents. An agent cannot walk over and ask what you
meant; faced with an ambiguity it will guess, and a plausible wrong guess costs
more than a missing feature. The fix is not smarter agents — it is issues that
have no room for a guess. Vague issues are how the renderer ended up with three
divergent copies of the same code.

## Required sections

Use the *Feature / task* template. All sections are mandatory; "n/a" is a valid
answer but a blank is not.

### Context

What exists today, and why this is worth doing now. Name the files and types the
work will touch, with paths — `crates/render/src/render.rs`, not "the renderer".
Link the milestone, the `PLAN.md` section, and any issue this depends on or
unblocks. Assume the reader knows Rust and graphics, and knows nothing about
Cubara.

### Goal

One or two sentences on the outcome, in terms of observable behaviour or a
capability gained. Not the implementation.

### Design decisions

**The section that makes an issue executable.** Every choice already settled,
stated as a decision, so the implementer does not re-open it:

- Data formats, file layout, and schema — with a worked example.
- Names of the key types/functions being introduced.
- Which crate the code belongs in, and why that one.
- The algorithm or approach, when a specific one is intended.
- Trade-offs already considered and rejected, with the reason.

If a decision genuinely is not made yet, say **"Open question:"** and either
resolve it before assigning the issue, or scope the issue to *making* that
decision. Never leave it implicit.

### Scope

A concrete checklist of what to build. One shippable PR where possible. If it
does not fit in one PR, it is a tracking issue with linked sub-issues instead.

### Out of scope

What this issue explicitly does **not** cover, especially adjacent things a
reasonable person would assume are included. This is the anti-scope-creep clause
and it is not optional — one issue, one PR, one concern.

### Done when

Criteria a reviewer can check, mechanically where possible. Prefer an automated
check over an eyeball. Every issue carries the standing bar:

- [ ] `cargo test --all`, clippy, and rustfmt green
- [ ] Behaviour verified by an automated check, not only by looking at it
- [ ] Perf-relevant: a `BENCHMARKS.md` row with the delta vs the previous row

Plus criteria specific to the work — with numbers where numbers apply. "Fast
enough" is not a criterion; "meshing a chunk stays under 2 ms at p99" is.

### References

Files, `PLAN.md`/`ROADMAP.md` sections, prior issues and PRs, external papers or
docs. Spell them out: `PLAN.md §5 (chunk system)`, not `§5`.

## Naming

Conventional-commit-shaped, so the issue title can become the PR title:
`feat(render): shader pack loader`, `perf(world): incremental relighting`,
`refactor(app): single scene-render path`. Keep the issue number reference
(`[#123]`) out of the title — GitHub adds it.

## Sizing

- **One PR.** If Scope has more than roughly eight checkboxes, or spans more than
  two crates, split it.
- **Tracking issues** hold the arc and link sub-issues; they are not themselves
  implemented.
- A **spike** (investigate, decide, write it down) is a valid issue whose Done
  when is "a decision recorded in `PLAN.md`" — not code.

## Anti-patterns

| Smell | Why it fails | Fix |
|---|---|---|
| "Improve X" / "Clean up Y" | No definition of done — it can never be closed. | State the end state. |
| "See §5" and nothing else | Forces a doc hunt and invites divergent readings. | Quote the decision into the issue. |
| "RON/TOML" | An unmade decision disguised as a detail; two implementers pick differently. | Pick one. Say why. |
| Scope that grows in comments | The PR stops matching the issue, review breaks down. | New concern → new issue. |
| "Done when: it works" | Unreviewable, unautomatable. | A command someone can run and a result they can compare. |
| No Out-of-scope section | The adjacent nice-to-have gets built and the PR doubles. | Name what you are not doing. |

## Worked example

A thin issue, and the same issue written to standard:

> **Before —** *"Add a texture atlas so blocks can have textures."*

Unanswerable questions: which crate; atlas or array texture; what size; who owns
the mapping from block id to texture; what happens to the existing flat-coloured
shader; is mipmapping in scope; how does anyone verify it worked?

> **After —** see [#43](../../issues/43) for the full shape: Context naming
> `crates/render/src/mesher.rs` and the current flat-colour path in
> `shaders/mesh.wgsl`; a Design decisions section fixing *array texture, 16×16
> tiles, `TextureRegistry` in `cubara-render`, block id → face indices owned by
> the block registry from #54*; Out of scope explicitly excluding mipmapping and
> animated textures; and Done when requiring a golden-image test rather than a
> screenshot someone looked at once.
