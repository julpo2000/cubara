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

### §3.5 What crosses the wire, and why the toolchain pin becomes load-bearing

**Raised by the Windows session**, and it is the sharpest constraint on §4's
fixed-point work.

Determinism currently holds partly because both machines run the same compiler —
which is now pinned (§5.1). Fixed-point removes floating point from the
*simulation*, but that is only half the surface. **If any value that crosses the
wire, or that a client uses to reconcile, is still `f32`/`f64`, the pin stops
being a lint convenience and becomes load-bearing for correctness.**

So the rule for the netcode block:

> **Nothing that crosses the wire is a float.** Positions, velocities and any
> value a client reconciles against are integers — fixed-point where a fraction
> is needed.

Floats may still exist *after* the seam: interpolation for rendering between two
received states is a display concern (§9 of `PHASE1_ARCHITECTURE.md` already
draws that line for the local camera), and a wrong last bit there shows as a
sub-pixel difference rather than a divergence.

**Positions are not the whole float surface, and it is worth naming the rest
before the work starts.** `WorldHash::write_sim` currently folds in six `f32`
position/velocity components *and* `yaw` and `pitch`. Angles are the awkward
ones: they feed `look_dir()` through `sin`/`cos`, and the resulting ray decides
**which block gets mined** — an edit, and therefore authority. `sin` and `cos`
are among the least portable functions in any standard library.

So the migration has two halves, and only the first is scheduled:

| | Status |
|---|---|
| Positions and velocities → [`Fixed`] | **done** (#184, #185) |
| Angles → fixed-point, with integer trig | **done** — see §3.6 |

Angles are deliberately second: positions are the larger surface and the one
that also fixes a real precision limit, while angles only matter once two
machines must agree on what a third player is looking at. But shipping netcode
with `f32` angles in the authority hash would be exactly the kind of thing this
document exists to catch early.

The practical test is the one already in the repo: two platforms agreeing on a
world-state hash. If that hash is computed only from integers, it cannot drift
with a compiler version, and the pin returns to being about lints.

**Worth stating because it is easy to get backwards:** the goal is not "avoid
floats". It is that *authority* is integer and *presentation* may be float.

### §3.6 The one question this leaves open

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

---

## §8 The client/server seam, concretely

§7 step 2 is *"split `Game` into client and server halves, with singleplayer
running the server in-process"*. This section is what that means against the code
that exists, written before the work rather than during it.

### §8.1 What `Game` owns today, and which side each field belongs to

`Game` is currently one struct holding twenty-four fields. Sorting them is most
of the design:

| Field | Side | Why |
|---|---|---|
| `world`, `sim` | **both, separately** | See §8.2 — this is the interesting one. |
| `blocks_registry`, `items`, `recipes`, `smelting`, `terrain` | **both** | Content definitions. Both sides need them; the server is authoritative about what they *mean*. |
| `sim_centre` | server | Which chunks simulate is authority. |
| `mining`, `breaking` | **client predicts, server decides** | Progress is display; the break is an edit. |
| `inventory_open`, `open_furnace` | client | Screen state. A server does not care what you have open — §11 already calls `inventory_open` screen state rather than world state. |
| `prev_player`, `accumulator` | client | Interpolation and frame pacing are presentation. |
| `forward`…`look_delta`, `jump_pending` | client | Raw input, collapsed into an `InputFrame` and sent. |

Two of those are already correct and worth noticing: `InputFrame` exists as a
*value* precisely so it can be sent, and `Crafting`/`Inventory` already live on
the player rather than on the screen.

### §8.2 Both sides have a `World`, and that is the point

The instinct is to share one `World` in singleplayer. **That defeats the
exercise**: the seam only tells you something if the client cannot reach into
the server's state, and an in-process shortcut is exactly the shortcut that will
not exist over a socket.

So both hold a `World`. This is affordable only because of §3.4: terrain is a
pure function of the seed, so the client's copy is *generated*, not received.
What the server sends is the edit overlay — which is already how `World` is
built (`worldgen` + `edits`), and already what the save format persists.

**The client's `World` is a replica, not a cache.** It may be wrong, briefly,
between an optimistic prediction and the server's correction. Nothing may treat
it as authority.

### §8.3 The messages

Small, and deliberately not a generic RPC:

```
client -> server   InputFrame (per tick)
                   Action::{Break, Place, ClickSlot, Interact}

server -> client   Edits(Vec<(pos, BlockId)>)
                   PlayerState(position, velocity, health, inventory, …)
                   Entities(spawned, moved, despawned)
                   BlockEntities(changed)
                   Tick(u64)
```

`Action` is separate from `InputFrame` on purpose. An input is *what the player
did with the controls*; an action is *what they are asking the world to do*.
Sending "mouse button down" and letting the server raycast means the server
decides what was hit, which is what stops a client claiming it mined something
across the map — the §3.4 rule, "may never be believed", made structural.

### §8.4 What lands first, and what must not

**First:** the seam, in-process, with no networking and no prediction. The
client sends actions, the server applies them, the client applies the returned
edits. Singleplayer will feel identical because the round trip is a function
call.

**Not first, and not in this block:** prediction and reconciliation. They are
what makes a *remote* client feel good and they are pure latency compensation —
building them before there is latency means building them against a round trip
that is always zero, which cannot show whether they work.

**The test that says it worked:** every existing gameplay test still passes,
unchanged. If splitting the seam requires changing what a test asserts about
breaking a block or opening a furnace, the split has changed behaviour and is
wrong.

### §8.5 The risk worth naming

This is the largest refactor the project has attempted — larger than the render
extraction in phase 1, because it cuts through the middle of the type every
gameplay test drives. Its whole value is that it is **cheaper now than later**:
at one player, ten thousand lines, and no netcode, the seam can be moved by
changing which struct owns a field. After netcode exists it cannot.


### §8.6 The crate split, and what a dedicated server proved

**Landed.** `Server` moved out of `crates/app` into its own crate,
`cubara-server`, which depends on `cubara-voxel`, `cubara-world` and
`cubara-sim` and on nothing else.

While it lived in the app it was correct by *content* — it imported no `wgpu` —
and wrong by *construction*: anything linking it also linked a windowing library
and a graphics API. The distinction is not academic, and the binaries say so:

| | Links | Size (release, macOS) |
|---|---|---|
| `cubara` | Metal, AppKit, QuartzCore, CoreVideo | 6.8 MB |
| `cubara-server` | `libSystem` only | 2.1 MB |

That is §3.3's standalone deployment becoming real rather than notional: a
headless host runs a world without installing a GPU stack for a process that
will never draw a pixel.

**Two entry points, one loop.** `cubara-server` and `cubara server` parse the
same arguments and call the same `headless::run` (Rule 5). The subcommand exists
because a machine that already has the game should not need a second download to
host; the binary exists because a server host should not need the game.

**What it made testable.** A headless tick loop is testable *to the tick*, which
is the argument for building it before the transport (§8.5). The claims that now
have tests rather than prose:

- a world runs with no window, no adapter and no client;
- a furnace smelts with nobody playing — the world does not stop when the player
  does;
- two servers opened on the same seed and ticked the same number of times
  produce the same `WorldHash` (Rule 1, from the server's own entry point);
- a world survives a restart, through `Session` rather than through `Game`, so
  it holds for a host that has no client at all.

**The architecture check grew with it.** `crates/server` is now in
`check-architecture.sh`'s Rule 3/4 list, so `cubara-server` gaining a GPU
dependency fails CI rather than failing on someone's headless box months later.
It is also in the Rule 1 list, with `clock.rs` excluded **by name** — a
dedicated server has to turn seconds into ticks somewhere, and naming the one
file that may means a second one is a CI failure rather than a precedent.

**Still not built: the transport.** Nothing listens on a port; no client can
connect. `cubara-server` runs a world, it does not yet serve one. The next steps
remain §8.2's client-side replica world, and then a socket.


### §8.7 The replica, and what it caught

**Landed.** The client holds its own `World`. §8.2's rule — *both sides have a
`World`, and that is the point* — is now a fact about the types rather than an
intention: `Game::world()` returns the client's, and nothing in the client reads
the server's at all.

**Terrain is generated, never sent.** The client is given a seed and builds the
same world from it. What crosses the seam is the edit overlay and the block
entities, which is already how a `World` is built and already what the save
format persists. Measured at radius 64: CPU/frame 0.622 → 0.628 ms, inside this
scene's run-to-run scatter (`BENCHMARKS.md`). Two worlds cost about one, because
the second one is a seeded noise function and an empty `BTreeMap`.

**`Dirty(chunk)` is gone, and its absence is the interesting part.** It only ever
worked because both sides shared one world — the client had nothing of its own to
mark dirty. Now the server reports `Edit { pos, block }`, the client applies it to
its own world, and the stale chunk is whatever its own `set_block` hands back. A
remote client would have to derive it exactly that way, because the server has no
idea how its chunks are laid out on screen.

**The furnace screen is the block-entity message doing real work.** A furnace
smelting away updates the panel because the server journals a `BlockEntity`
effect on every tick it changes and the client applies it — not because the
client is looking at the server's furnace. Whole values, not deltas: a furnace is
three slots and two counters, so sending what it *is* costs less than describing
what happened to it and cannot desynchronise the way a missed delta can.

**`Action::ClickFurnace`, and the slot vocabulary.** Moving items between a hand
and a block entity is world state, however much it looks like UI. The client
translates `PanelSlotKind` (where a slot is drawn) into `FurnaceSlot` (what it
is), because a server that spoke in panel layouts would be a server that knew
what a screen looks like. It is the one action that names its target, for a
reason the raycast rule does not cover — the player is not *looking* at the slot
— and the server validates the position rather than believing it.

**The snapshot.** A replica that has seen nothing cannot be patched with a delta,
because there is no delta from a world it has never seen. `Server::snapshot`
emits every edit and every block entity as ordinary effects; a load uses it,
and it is the shape the join handshake will take.

#### What the split caught

Thirteen existing tests failed the moment the replica landed, and **every one of
them for the same reason**: they set the world up by writing straight into
`server.world`, then asserted through the client. That is precisely the
in-process shortcut §8.2 says the exercise exists to remove — invisible while one
`World` was serving both sides.

The fix was to make the setup go through the server (`set_block`, `add_furnace`,
`set_furnace`), which journals. **No assertion changed.** Two moved which object
they read: the chunk-lifecycle tests now ask the server, because which chunks
simulate is authority (§8.1) and a replica has no lifecycle — it is told about
edits, not about what is ticking.

That is the split doing the job it was built for, one step before there is a
socket to find it over.

#### Still not split

Inventory and crafting clicks still mutate `sim.player` directly. They are
authority too, and they are `ClickSlot` in §8.3's list — but they touch no world
state, so they cannot desynchronise a replica. They travel with the transport.

**And there is still no transport.** Nothing listens on a port.


### §3.6 Angles, and the last float out of the authority hash

**Landed.** `cubara_voxel::Angle` is a **binary angle**: a full turn is 2³², so
an `i32` covers exactly one turn.

That single choice does most of the work. Wrapping is `wrapping_add` — exact, with
no `% 2π` to get subtly wrong and no constant that is not representable. Every
angle in the range is representable, so there is no edge to clamp at. Comparison
is integer comparison, which is what pitch's clamp needs. Radians in [`Fixed`]
would have had none of these: π is not representable, so wrapping would
accumulate error, which is the exact failure this exists to prevent.

`a_session_of_turning_returns_exactly_to_where_it_started` is that as a test:
216,000 ticks of turning right and the same back lands on the identical integer.

#### Integer trigonometry, and why a polynomial rather than a table

`sin` and `cos` are gone from the simulation entirely. In their place, an odd
polynomial `a₁z + a₃z³ + a₅z⁵ + a₇z⁷` in `z ∈ [0, 1]`, evaluated in 30-bit fixed
point with `i128` intermediates, on a quarter turn folded out of the full turn by
integer masking — the fold introduces no error of its own, because a quarter turn
is a power of two.

The coefficients are **fitted, not truncated**. The Taylor series to the same
degree is off by 11 `Fixed` ULP: it is built to be exact near zero and spends its
accuracy there rather than across the quarter turn. Fitting gets to **1 ULP with
the same four multiplies**; Taylor needs five terms to match it. One ULP is the
floor worth aiming at, because below it the precision is rounded away by `Fixed`.

No table, so no build script, no 4KB of magic numbers in a source file, and no
lazily-initialised static (which would be ambient state — Rule 2).

`sine_matches_the_reference_within_one_ulp` checks it against `f64::sin` over
200,000 angles spread across the turn, and the cardinal directions come out
*exactly*: `sin(0) = 0`, `sin(¼ turn) = 1`, `cos(½ turn) = −1`.

**It costs nothing.** CPU/frame 0.628 → 0.630 ms at radius 64, which is scatter —
trigonometry runs twice per tick, 120 times a second, against ~3,000 chunk nodes
of meshing per frame.

#### What this changes downstream

- **`WorldHash::write_sim` contains no floating-point value at all.** `write_f32`
  is deleted, because nothing calls it. That is what returns the toolchain pin
  to being about lints rather than being load-bearing for correctness.
- **`InputFrame::look_delta` is an `Angle`, not pixels.** An `InputFrame` is the
  first thing that will cross a socket, so it cannot carry a float. The
  pixels-to-angle conversion moved to the client, which is where it belonged
  anyway: sensitivity is a setting on the machine holding the mouse, not a fact
  about the world.
- **`level.ron` is at `FORMAT_VERSION` 4.** Same field names, different meaning,
  which is exactly what a version number is for. The committed fixture was
  re-blessed; the diff is four lines and is documented in `save_load.rs`, and
  every file under `region/` is unchanged.
- **A new architecture check.** `crates/**/src` in the simulation crates may not
  call `.sin()`, `.cos()`, `.tan()`, `.atan2()`, `.asin()` or `.acos()`;
  `angle.rs` is excluded by name, because its tests check our integer
  trigonometry against the platform's. Verified by breaking it on purpose.

#### What is still a float, honestly

- **The raycast itself.** `World::raycast` takes `[f32; 3]` and steps in `f32`.
  Basic arithmetic is correctly rounded and deterministic under IEEE 754; what
  was *not* deterministic was the transcendental functions, and those are gone.
  Moving the raycast to fixed-point is a separate piece of work, and this one
  does not depend on it.
- **`InputFrame::move_axes`** is still `[f32; 3]`, holding −1, 0 or 1. It will
  cross the wire, so it should be integers — a small change, not named in §3.5,
  and it travels with the transport.
- **Free-fly movement** converts through `f32`. It is a debug mode and never
  authoritative.
