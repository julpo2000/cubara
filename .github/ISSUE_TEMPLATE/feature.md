---
name: Feature / task
about: A unit of engine or gameplay work
title: ""
labels: ""
---

<!--
Standard: docs/ISSUE_STANDARD.md — read it before filing.

The bar: someone who has never seen this repo can pick this up, read only this
issue and the files it names, and build the right thing WITHOUT asking a single
question. Every section below is mandatory. "n/a" is a valid answer; blank is not.
-->

## Context

<!-- What exists today and why this is worth doing now. Name files WITH PATHS
(`crates/render/src/render.rs`, not "the renderer") and the types involved. Link
the milestone, the PLAN.md / ROADMAP.md section, and any issue this depends on or
unblocks. Assume the reader knows Rust and graphics, and nothing about Cubara. -->

## Goal

<!-- One or two sentences: the outcome as observable behaviour or capability
gained. Not the implementation. -->

## Design decisions

<!-- THE SECTION THAT MAKES THIS EXECUTABLE. Every choice already settled, stated
so nobody re-opens it: data formats + schema with a worked example, names of the
key types/functions, which crate owns the code and why, the intended algorithm,
and alternatives rejected with the reason.

Not decided yet? Write "Open question: ..." and either resolve it before assigning
this issue, or re-scope the issue to MAKING that decision. Never leave it implicit. -->

## Scope

<!-- Concrete checklist of what to build. One shippable PR. More than ~8 boxes or
more than two crates → split into a tracking issue + sub-issues. -->

- [ ]

## Out of scope

<!-- What this explicitly does NOT cover — especially adjacent things a reasonable
person would assume are included. Not optional: one issue, one PR, one concern. -->

## Done when

<!-- Checkable criteria, mechanical where possible. Numbers where numbers apply
("meshing stays under 2 ms at p99", not "fast enough"). -->

- [ ] `cargo test --all`, clippy, and rustfmt green
- [ ] Behaviour verified by an **automated check**, not only by looking at it
- [ ] Perf-relevant: `BENCHMARKS.md` row added with the delta vs the previous row

## References

<!-- Files, PLAN.md / ROADMAP.md sections, prior issues and PRs, external docs.
Spell them out: "PLAN.md §5 (chunk system)", not "§5". -->
