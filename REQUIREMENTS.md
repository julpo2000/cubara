# Cubara — Founding Requirements

These are the original wishes for the project, written down as the north star.
Everything in [`PLAN.md`](PLAN.md) exists to serve these. If a future decision
conflicts with one of these, we revisit it here first.

**What ships, and when**, is in [`ROADMAP.md`](ROADMAP.md) — it defines the 1.0
scope, the milestone ladder, and the gates each phase has to clear.

## Motivation

Build the "definitive" voxel game. The mainstream voxel game is great, but its
existing implementations fall short — one edition runs poorly, the other isn't the
answer. Cubara aims to be a voxel game done right — great to play, great to build on.

## Hard requirements

1. **Performance above all.**
   - The game must run exceptionally well.
   - Goal: practically **infinite render distance**, implemented with **LOD**.
   - Concrete gate: hit **1000+ FPS in a simple world** before building the actual
     game. Performance is proven first, gameplay second.

2. **A solid, well-thought-out engine.**
   - The engine is the foundation of everything and must be designed carefully.

3. **Easy to work on and extend.**
   - We will later add **multiplayer**, **shaders**, and **extra items/content**.
   - **Modding must be easy.**
   - Architecture should keep features from sitting loosely side by side.

4. **A new chunk system.**
   - **16×16×16** chunks (cubic chunks).
   - Specific regions can be **loaded and kept loaded**.
   - **Timers for while you're away**, so crops keep growing, etc. — inspired by how
     **Factorio** handles off-screen simulation.

5. **Good progression and synergy.**
   - Start with the basics. **Alpha = a playable survival world** where you can chop
     trees and at least mine iron.
   - Then gradually fold in all modern voxel-game features, but keep them **cohesive**
     rather than loose (a weakness of the current market leader).

6. **No copyright trouble.**
   - The project is **not** branded as a sequel to or clone of an existing game.
     Name: **Cubara**.
   - We make our own **textures**, names, and content. Mechanics may be inspired;
     assets and branding are original.

7. **GitHub integration & version control.**
   - Development runs **professionally**: real commits, features via **issues**,
     pull requests, and so on.

8. **Multi-platform support.**
   - Developed on **Windows and macOS**, later possibly ported to more platforms.

## Sequencing note

Game features and gameplay tweaks are discussed **later**. First we build a genuinely
good engine for rendering blocks, and we do not move on to the real game until the
1000 FPS bar in a simple world is met.
