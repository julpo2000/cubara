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

## §3 Decision: an authoritative server. Lockstep is ruled out.

**Owner, 2026-08-31:** *"Player count moet niet uitmaken. Zowel 5 als 5000
players moet kunnen. Prive en public."*

That settles it, and it settles it against the recommendation the first draft of
this document made. §3.1 below is kept as written, because being able to see why
the wrong answer looked right is worth more than a clean document.

**Lockstep cannot do this, at any amount of effort.** Every peer simulates
everything and waits for the slowest, so the cost per player is O(all players)
and one bad connection stalls the world. It is excellent for two players on a
LAN and structurally incapable of five thousand. And public servers make it
worse: lockstep gives every client the full world state and the full simulation,
so there is no authority to cheat against — every client *is* the authority.

So: **an authoritative server, with client-side prediction** (§1.2).

### §3.0 The good news, and it is substantial

The prerequisite that usually hurts most is already done. **`ARCHITECTURE.md`
Rule 4 — the simulation runs with no GPU — means `cubara-sim` and `cubara-world`
have no `wgpu` or `winit` dependency and a headless dedicated server needs no
extraction work.** Rule 3 (dependencies point one way) is what keeps it true.
Rule 2 (no ambient state, no globals) is what lets one process host several
worlds. Those three rules were written for this.

Determinism (Rule 1) does not become useless either — it stops being the
*mechanism* and becomes leverage:

- client prediction reconciles **exactly** rather than approximately, because the
  client can run the same simulation the server will;
- the world-state hash becomes a **server-side desync/cheat detector**;
- a reported bug is reproducible from a seed and an input log.

### §3.1 What the first draft recommended, and why it was wrong

The original recommendation was lockstep, on the grounds that this codebase had
already paid its price — deterministic sim, `InputFrame` as a value, a world
hash, cross-platform agreement proven in CI. All of that is true.

It was wrong because it optimised for **cheapest path to two players** while the
actual requirement was **any number of players, including public**. The research
even said so — *"lockstep is not a step toward that; it is a different
destination"* — and then recommended it anyway because the near-term goal looked
small. Asking the player-count question before building is exactly what stopped
that from becoming a rewrite.

### §3.2 What five thousand actually demands

Worth being blunt: **5,000 concurrent players in one shared world is beyond what
this genre normally achieves.** Large public Minecraft servers run in the
hundreds. Games that reach thousands in one universe (EVE) do it by sharding
space across processes and slowing time when a region overloads.

Reaching it needs, roughly in order:

1. **Authoritative server**, headless. Rule 4 means this is available now.
2. **Interest management.** A client is sent only what it can perceive. This is
   the single largest determinant of whether 5,000 is possible: without it,
   bandwidth is O(players²).
3. **Delta encoding and a per-client view.** Send changes, not state.
4. **Persistence that is not `level.ron`.** One RON file and a region directory
   is right for one player and wrong for a live server with thousands.
5. **Region sharding across processes**, once one machine's tick budget runs out.
   This is the one that must be *designed for* early even if built late: it
   requires that no code assume a single `World` owns everything.
6. **Cheat handling**, which is what "public" really costs. Every client input
   becomes untrusted: reach, speed, and inventory all need server-side checks.

**1–3 are a large but ordinary netcode project. 5 is a distributed-systems
project.** The honest framing is that "5,000 players" is not a bigger version of
"5 players" — it is a different engineering commitment, and it should be entered
knowingly rather than discovered at step 5.

### §3.3 One architecture, two deployments

*"Prive en public"* does not need two designs. The standard answer, and
Minecraft's, is:

- **Singleplayer and private play** run the server **in-process**. The client
  talks to it over the same interface as a remote one.
- **Public play** runs the same server binary on its own.

This is worth stating because it has a consequence for today: **singleplayer
becomes a client talking to a local server**, so `Game` can no longer own the
world and edit it directly. That refactor is the real cost of entering this
architecture, and it is much cheaper now — at ten thousand lines and one player
— than later.

### §3.4 What the client is allowed to simulate

**Raised by the Windows session while checking this document**, and it was a real
gap: an authoritative server changes what Rule 1 is *for*. Determinism stops
being only a testability property and becomes the thing that lets a client
predict without drifting. That only pays if the split is stated.

| | Who simulates it | Why |
|---|---|---|
| **Terrain** | **The client, from the seed** | The big one — see below. |
| The client's own player | Client predicts; server corrects | Otherwise every step costs a round trip. |
| Block edits by this client | Client predicts optimistically | Mining must feel instant; the server may reject. |
| Other players | Interpolate received state | Never simulated locally: their inputs are not known. |
| Mobs | Server only | Same reason, plus they are the obvious cheat surface. |
| Block entities (furnaces) | Server owns; client *may* run forward | See below. |
| Item despawn, hunger, damage | Server only | Anything that can kill or destroy is authority's. |

**Terrain is the one that matters most, and this project is unusually well placed
for it.** `WorldGen::density` is a pure function of `(seed, x, y, z)`, and
generation being bit-identical across platforms is *already proven on every
merged PR* by the fixture-hash test running on both CI runners. So:

> **The server never sends terrain. It sends the seed once, and edits thereafter.**

A joining client generates the world itself and applies an edit overlay — which
is exactly what `World` already is (`worldgen` + `edits`). Bandwidth then scales
with *how much players have changed the world*, not with how much world they can
see. For a 5,000-player target that is not an optimisation, it is the difference
between feasible and not.

**Block entities are the interesting case**, and block 2.7a already built the
answer. `Furnace::advance` takes an elapsed tick count and is proved equal to
ticking one at a time, so a client that knows a furnace's contents at tick *N*
can display its state at tick *N+k* without asking. That is a *display*
prediction, not authority: the server's value always wins on the next update.
It is the same property dormancy needed, reused.

**The rule underneath the table:** a client may simulate anything it can derive
from data it already has, and may never be *believed* about any of it. Prediction
is for latency, never for truth.

### §3.5 The one question this leaves open

**5,000 in a single shared world, or 5,000 across servers?** They are different
projects: the second is items 1–4 and is a normal (large) netcode effort; the
first adds item 5 and is genuinely hard. Both are served by the same first steps,
which is why work can start before it is answered — but it should be answered
before interest management is designed.

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

**Recommendation:** do it **before** the netcode, not after. Retrofitting
fixed-point into working netcode means re-validating every determinism guarantee.

The reason survives the change from lockstep to an authoritative server (§3), and
gains one:

- **Prediction reconciles exactly.** A client predicting with the same integer
  arithmetic the server uses agrees with it bit for bit, so reconciliation only
  ever corrects for *missing information*, never for arithmetic drift. With
  floats, some correction is always noise.
- **It is smaller on the wire.** Positions are the most-sent value in any netcode,
  and a fixed-point position quantises and delta-compresses far better than three
  `f32`s. At 5,000 players, bandwidth per position is not a detail.
- It remains the honest fix for the precision cap found while checking the
  vertical world, rather than documenting a limit.

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

### §5.1 The toolchain is not pinned, and that already cost a CI failure

**Found by the Windows session** while comparing versions across the two
machines. `.github/workflows/ci.yml` uses `dtolnay/rust-toolchain@stable` in both
jobs and there is no `rust-toolchain.toml`, so **CI runs whatever stable is on
the day** — currently 1.98, against 1.97.1 on the Windows laptop and clippy
0.1.97 on the Mac.

That is not a curiosity. `chunks_exact_to_as_chunks` is denied by CI's clippy and
**does not exist in 1.97**, so it passed locally on both machines and failed on
both CI runners. It will keep happening, and it will drift further.

`CONTRIBUTING.md`'s standing instruction is to run the checks before pushing;
that instruction is only worth anything if the checks are the same checks.

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
  phase 3's engine work. Moving it is the owner's call.
- **Single shared world, or many servers** (§3.4). Answerable later, but before
  interest management is designed.
- **What a second player *is*.** Mobs do not exist, and §10.3 deliberately kept
  the player *out* of the ECS because there was only one of it. **A second player
  is the thing that reverses that argument**, and it should be revisited when the
  server lands rather than treated as settled.

## §7 Suggested order of work

Each step is useful on its own and does not require the next one to exist:

1. **Fixed-point positions** (§4). Independently fixes the precision cap, and
   everything after it is cheaper on integer arithmetic.
2. **Split `Game` into client and server halves**, with singleplayer running the
   server in-process (§3.3). No networking yet — the seam is the point, and it is
   the change that gets more expensive every week it waits.
3. **A transport, and one remote player.** Two machines on a LAN; the Windows
   laptop is the second platform *and* the second CI platform (§5).
4. **Interest management** (§3.2 item 2). The step that decides whether the
   player-count target is reachable, and the one that wants §3.4 answered first.
5. **Untrusted clients.** What "public" actually costs.

Step 1 has a prerequisite worth naming: **local checks must match CI.** The
toolchain drift found while writing this (§5.1) means a lint CI enforces may not
exist on either developer machine, which makes "clippy is clean locally"
unreliable — and netcode is where a missed lint costs most.

Steps 1 and 2 are worth starting regardless of when the rest is scheduled,
because both get harder as the codebase grows and neither commits to a
player-count answer.
