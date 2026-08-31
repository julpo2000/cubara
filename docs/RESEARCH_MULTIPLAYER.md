# Research: how multiplayer is actually done, and which shape fits Cubara

**Owner's request, 2026-08-31:** multiplayer, wanted in phase 2, *"dan is het
iig optijd goed"* — research first, then build.

This document is research and a recommendation. It is **not** a scheduled block;
`ROADMAP.md`'s admission rules and sequencing still apply, and the owner decides
when.

---

## §1 The three shapes, and what each actually costs

### §1.1 Deterministic lockstep

Every peer runs the *same* simulation and exchanges **only inputs**. Nobody
sends world state after the initial handshake. Used by RTS games and, most
relevantly here, **Factorio** — which `REQUIREMENTS.md` #4 already names as this
project's reference for dormant-chunk timers.

| | |
|---|---|
| **Bandwidth** | Tiny, and *independent of world size*. A few bytes per player per tick. |
| **Needs** | Bit-exact determinism on every platform, forever. |
| **Joining** | Requires a full world snapshot — you cannot replay from tick 0. |
| **Latency** | Every peer waits for the slowest. Input is delayed by worst-case RTT. |
| **Failure mode** | A desync is fatal and silent unless you detect it. |
| **Scale** | A handful of players. Everyone simulates everything. |

### §1.2 Authoritative server with client prediction

One process owns the truth. Clients send inputs, receive **state**, predict
locally, and reconcile when the server disagrees. This is Minecraft, and most
shooters.

| | |
|---|---|
| **Bandwidth** | Proportional to what each client can see. Needs interest management. |
| **Needs** | Delta encoding, prediction, reconciliation, per-client view state. |
| **Joining** | Natural — a joiner is just a client that gets sent its surroundings. |
| **Latency** | Only the acting client feels it, and prediction hides most of it. |
| **Failure mode** | Rubber-banding. Ugly, not fatal. |
| **Scale** | Good. The genre standard for a reason. |

### §1.3 Peer-to-peer with a host

One peer runs the authoritative server in-process; others connect to it. This is
Minecraft's "Open to LAN". It is §1.2 with a deployment shortcut, not a third
architecture — and it carries NAT traversal, host advantage, and "the game ends
when the host quits".

Full mesh P2P, with no authority, is not used for this genre: without an
authority you need consensus on every conflicting edit, which is strictly harder
than either option above.

---

## §2 What this codebase already has

This is the part that decides the recommendation, and it is unusual.

**The project has been building for multiplayer since phase 1, deliberately.**

- **`ARCHITECTURE.md` Rule 1:** *"This is the keystone. It is what makes
  multiplayer lockstep possible."* Determinism is not aspirational here — it is
  tick-driven, ordered-iteration, seeded-RNG-in-world-state, and enforced.
- **`InputFrame` is already a value**, and its own doc comment says it is
  *"the shape netcode eventually wants (send the input, not the result)"*.
- **A world-state hash already exists** (`WorldHash`), covering player, inventory,
  crafting, chunks, block entities and dropped items.
- **Cross-platform determinism is already proven in CI.** The fixture-hash test
  runs on **both `macos-latest` and `windows-latest`** on every merged PR. The
  two platforms agree on a full simulation hash today.
- **A save format exists** that captures exactly the state a joining player would
  need.
- **Dormancy and bounded catch-up** (blocks 2.6/2.7a) mean simulating a large
  world is already cheap when nobody is in it.

Each of those is a prerequisite for lockstep that would otherwise have to be
built. Together they are most of it.

---

## §3 Recommendation: lockstep first, over a host transport

**Deterministic lockstep**, with one peer hosting the connection.

The argument is not that lockstep is better in general — §1.2 scales further and
degrades more gracefully. It is that **this codebase has already paid lockstep's
price and has not paid the authoritative server's.**

What lockstep needs that does not exist yet:

1. A transport, and a tick barrier: nobody simulates tick *N* until every peer's
   `InputFrame` for tick *N* has arrived.
2. Input delay — peers run *N* ticks behind the newest input so the network has
   time. Two or three ticks on a LAN.
3. A join handshake: send the save, then stream inputs from that tick on.
4. **Desync detection**, which is nearly free: exchange the world hash every *N*
   ticks and halt loudly on a mismatch. Most games cannot do this; this one can,
   today.

What the authoritative server would need on top: interest management, delta
encoding, per-client visible-state tracking, prediction and reconciliation, and
a client world that is explicitly *not* authoritative. That is a much larger
change to a codebase whose whole shape is "one deterministic world".

### §3.1 The honest downsides, stated up front

- **Input latency is shared.** Every player feels the worst connection. On a LAN
  this is nothing; over the internet it is noticeable.
- **One slow machine slows everyone.** There is no "the server is fine, that
  client is lagging".
- **It does not scale.** Every peer simulates every active chunk. Fine for two
  players; wrong for twenty.
- **A desync ends the session.** Detectable immediately, but not recoverable
  without a resync (which is a state transfer — i.e. borrowing §1.2's machinery).

**If the game ever wants many players or public servers, §1.2 is where it ends
up.** Lockstep is not a step toward that; it is a different destination. That
trade should be made knowingly, and it is the owner's to make. What makes it
defensible now is that two players on a LAN is the actual near-term goal, and
lockstep reaches it in a fraction of the work.

---

## §4 Floats, and the owner's fixed-point suggestion

> *"kunnen we geen ints gebruiken voor locaties en float voor het gedeelte
> achter de komma?"*

**This is the right instinct, and it solves two separate problems at once.**

1. **Precision.** Player position is `Vec3<f32>`. At y = 1,000,000 the smallest
   representable step is ~0.06 blocks; past ~8,400,000 an `f32` cannot represent
   consecutive integers at all. The world is unbounded but positions are not.
2. **Determinism, which matters far more for lockstep.** Floating point is *the*
   classic desync source across platforms and compilers. Cubara is currently in
   good shape — CI proves macOS and Windows agree — but that is a property that
   must hold forever, across every future compiler and CPU, and it is checked
   only at the granularity of the fixture test.

The standard fix is exactly what was suggested: **fixed-point.** A position
becomes an integer block coordinate plus a fractional part, e.g. `i32` blocks and
a `u16`/`i32` sub-block fraction (1/65536 of a block). All arithmetic is integer,
so it is bit-identical everywhere by construction, and precision is uniform
rather than degrading with distance.

**Cost:** it touches every position in physics, and the renderer must convert to
float per frame (which is fine — the renderer already works in camera-relative
space and never needs absolute precision).

**Recommendation:** do it **before** lockstep, not after. Retrofitting fixed-point
into a working netcode means re-validating every determinism guarantee; doing it
first means lockstep is built on arithmetic that cannot desync. It is also
independently useful, and it is the honest fix for §4.1's precision limit rather
than documenting a cap.

---

## §5 Testing across two machines

The owner has a Windows laptop as well as this Mac, and asked how genuinely
simultaneous tests would run.

**Verified: this account has Remote Control, and peer sessions are visible from
here.** Running Claude Code on the Windows laptop makes it addressable by name
from this session, so one side can drive both halves of a test — start a host
here, join from there, and compare world hashes — rather than a human relaying
between two terminals.

That matters more than convenience: **a desync test is only meaningful if both
ends run the same scripted input at the same wall-clock moment**, and a
cross-platform desync is exactly the failure lockstep must catch. The two
machines are also the two platforms CI already covers, so a desync between them
is a real signal rather than an artefact.

**Concrete plan, when the time comes:**

1. Both machines run the same commit.
2. Host on one, join from the other, over the LAN.
3. Drive a fixed scripted `InputFrame` sequence on both — the same harness the
   phase-2 gate's survival replay wants anyway.
4. Compare world hashes every N ticks. Any divergence fails the test and names
   the tick.

Step 3 is worth noting: **the survival replay harness the phase 2 gate is
blocked on and the multiplayer desync harness are the same machinery.** Building
one gets most of the other.

---

## §6 What this does not decide

- **Whether multiplayer belongs in phase 2.** `ROADMAP.md` lists it under
  phase 3's engine work. Moving it is the owner's call, and this document is the
  research that call was asked for — not the decision.
- **Player count, and whether public servers are a goal.** That answer decides
  §3 versus §1.2, and it is a product question rather than a technical one.
- **What a second player *is*** — mobs do not exist, the player is not an entity
  (§10.3), and "another player" is the first thing that makes that distinction
  matter.
