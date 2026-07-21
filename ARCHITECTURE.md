# Cubara — Architecture

The founding requirements ask for an engine that is *solid* and *easy to build on*
([`REQUIREMENTS.md`](REQUIREMENTS.md) #2, #3). This document says what that means
concretely, as rules that can be checked rather than intentions that can be
admired.

## The standard

The reference codebase for this project is **Factorio**, not the mainstream voxel
game. That is a statement about engineering, not gameplay: a deterministic
simulation, data-oriented layouts, stable performance under scale, and a modding
surface that is the same machinery the game itself is built on.

The mainstream voxel game is the anti-pattern, and specifically for these reasons:
static mutable world state, god objects that own rendering *and* input *and*
gameplay, simulation tangled into the render loop, and no real extension points —
which is why modding it requires patching bytecode. Every rule below exists to
avoid one of those outcomes.

## What "easy to build on" actually means

Not: pre-built slots for features we might want. Guessing what a future system
needs produces dead fields and false abstractions, and we throw the guess away
anyway.

But: **any subsystem can be deleted and rebuilt without the rest falling over.**
That property comes from three things and nothing else —

1. Its interface is small and explicit.
2. Nothing depends on its internals.
3. Tests define what "still works" means, so a rewrite has a target.

Adding a field to a block a year from now is expected and fine. Adding it should
not be able to break lighting, meshing, saving, or the network. That is the bar.

---

## Rule 1 — The simulation is deterministic

**Same initial state plus same input sequence produces the same resulting state,
bit for bit, on every machine and at any thread count.**

This is the keystone. It is what makes multiplayer lockstep possible instead of
state syncing, makes saves and replays trustworthy, makes a bug reproducible from
a report, and makes the simulation testable at all. It is also the only rule here
that cannot be retrofitted — determinism added later is a rewrite, so it is
adopted now, before there is a simulation to rewrite.

Consequences, all binding on simulation code:

- No wall-clock time. The sim advances by **tick number**, never by elapsed
  seconds. `Instant::now()` belongs to the renderer and the profiler, not the sim.
- No unordered iteration. Iterating a `HashMap`/`HashSet` and letting the order
  affect results is forbidden; use ordered collections or sort explicitly.
- No unseeded randomness. RNG state is part of world state, seeded and saved.
- No thread-scheduling dependence. Parallelism is allowed only where results are
  order-independent or explicitly merged in a fixed order.
- No floating-point where integers or fixed-point will do. Cross-platform float
  determinism is not free; sim quantities that must match exactly are integers.

**Enforced by:** a replay test that runs a fixed input script twice — once
single-threaded, once multi-threaded — and compares a hash of the resulting world
state. Plus a source check that bans the above constructs from simulation crates.
A determinism failure is a CI failure, not a warning.

## Rule 2 — No ambient state

**There is no global mutable state. Every system receives what it operates on.**

No `static mut`, no `static OnceLock<RwLock<T>>` holding world data, no
singletons, no "just reach in and grab it". A system that cannot be instantiated
twice cannot be tested in isolation, cannot be run twice in one process (a server
hosting two worlds), and quietly couples every caller to it.

**Enforced by:** a source check rejecting module-level `static` containing
interior-mutable containers, and by the test suite — tests that share global state
are order-dependent, which the determinism replay test surfaces immediately.

## Rule 3 — Dependencies point one way

**Crates form a DAG, and the arrows point from specific to general.** Lower
layers never learn about higher ones: the voxel data structures know nothing
about rendering; the simulation knows nothing about the window; nothing outside
`app` knows about both.

In particular, the renderer does not own gameplay. If the renderer can place a
block, the boundary is already wrong.

**Enforced by:** Cargo itself — a cycle is a compile error, and a dependency that
should not exist is visible in one `Cargo.toml`. This rule costs nothing to
enforce and is therefore non-negotiable.

## Rule 4 — The simulation runs without a GPU

**A headless build simulates the world with no window, no adapter, no wgpu.**

This is what makes the simulation testable in CI on machines with no GPU, what
makes a dedicated server possible without a rewrite, and what proves Rule 3 holds
in practice rather than on paper.

**Enforced by:** the simulation crates do not depend on `wgpu` — checked by Cargo
— and by tests that tick a world with no GPU present.

## Rule 5 — One implementation per concern

**Two pieces of code that answer the same question are a bug.**

The renderer had three separate scene-render paths. Features landed in one and
were invisible to the others, and the headless screenshot silently stopped
rendering what the game renders — which destroyed its value as verification. That
is not a tidiness problem; it made a whole class of change unverifiable.

**Enforced by:** [`scripts/check-single-render-path.sh`](scripts/check-single-render-path.sh),
run in CI.

## Rule 6 — Behaviour is pinned before it is rewritten

**A subsystem may only be confidently replaced if tests say what it must still
do.** Since the point of this architecture is that things *can* be ripped out and
rebuilt, the tests that make that safe are part of the deliverable, not a
follow-up.

Logic that can be tested without a GPU must live outside the GPU path so that it
can be. Rendering is pinned by golden-image tests: render a fixed scene headlessly
and compare against a committed reference within tolerance.

**Enforced by:** CI, and by the review question "what test currently pins this?"

## Rule 7 — Performance is a tracked invariant

Already project practice: every performance-relevant change records a
[`BENCHMARKS.md`](BENCHMARKS.md) row with the delta. The 1000-FPS gate is
re-validated as the engine grows; a regression blocks the milestone rather than
being noted and forgotten.

---

## Why these are enforced mechanically

This project has already run the experiment. Every rule wired to machinery —
branch protection, the required checks, the PR template — held without exception.
The rule that existed only as prose (REQUIREMENTS.md #3, "features must not sit
loosely side by side") was violated until the renderer had three copies of itself.

So: a rule in this document without an enforcement mechanism is a defect in this
document. When adding one, the question is not "is this good practice" but **"what
fails when someone breaks it?"**

## When a rule blocks real work

Rules that cannot be followed get broken silently, which is worse than not having
them. If one of these genuinely obstructs something necessary:

1. Say so in the PR, with the concrete case.
2. Change the rule here, in that PR, with the reasoning.
3. Update the enforcement to match.

What is not acceptable is an exception that lives only in someone's head, or a
check disabled to make a build pass.
