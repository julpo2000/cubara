//! Cracks on the block being dug, as triangles.
//!
//! The owner asked to see a block take damage before it breaks. Digging takes
//! time (block 2.4b), and with nothing on screen a long dig is indistinguishable
//! from a click that did nothing.
//!
//! # Why geometry, not a texture
//!
//! A crack overlay in the terrain shader would need the mesh to know which block
//! is being dug, and the terrain mesh is shared, cached and rebuilt only on
//! edits -- the wrong place for something that changes every tick. Dark cells
//! laid just outside the block's six faces go through the figure pipeline that
//! already draws depth-tested, CPU-built, coloured triangles, so this adds no
//! pipeline and no second path (Rule 5). A crack is at most a few hundred
//! vertices, next to the ~900,000 the terrain draws.
//!
//! Like [`crate::figure`], it is a pure function: where the cracks are, and that
//! they face outward, is testable with no GPU.
//!
//! # The pattern
//!
//! Original, drawn by this code: a handful of fixed crack lines walked out from
//! near the middle of a 16x16 face, each cell ranked by how far along its line
//! it is. Ten stages, as progress passes each tenth, reveal the ranks in order
//! -- so the crack *grows* outward rather than flickering between patterns.

use crate::figure::FigureVertex;

/// Cells per face edge, matching the 16x16 block textures.
const CELLS: usize = 16;

/// How far outside the face the cracks sit, in blocks. Enough to win the depth
/// test against the face at digging distance, too little to see as a gap.
const LIFT: f32 = 0.004;

/// Near-black, and a little warm, so it reads as a shadow in the stone rather
/// than as paint.
const COLOR: [f32; 3] = [0.07, 0.06, 0.05];

/// Stages a dig passes through. Stage 0 draws nothing.
pub const STAGES: u8 = 10;

/// Which stage `progress` (`0.0..1.0`) is at: `0` until the first tenth.
pub fn stage(progress: f32) -> u8 {
    ((progress.clamp(0.0, 1.0) * STAGES as f32) as u8).min(STAGES - 1)
}

/// One crack line: the cell it starts in, and each step after that.
type Line = ((i32, i32), &'static [(i32, i32)]);

/// For each cell of a face, the first stage it appears at, or 0 for never.
fn ranks() -> [[u8; CELLS]; CELLS] {
    // Each line: a start cell and eight steps -- nine cells for the nine
    // visible stages, so every stage adds something. Hand-placed so the crack
    // spreads across the whole face by the last stage; the steps wander, so it
    // reads as a fracture rather than as a star.
    const LINES: [Line; 4] = [
        (
            (7, 8),
            &[
                (-1, 0),
                (-1, -1),
                (-1, 0),
                (0, -1),
                (-1, -1),
                (-1, 0),
                (0, -1),
                (-1, -1),
            ],
        ),
        (
            (8, 7),
            &[
                (1, 0),
                (1, 1),
                (1, 0),
                (1, 1),
                (0, 1),
                (1, 0),
                (1, 1),
                (0, 1),
            ],
        ),
        (
            (8, 8),
            &[
                (0, 1),
                (-1, 1),
                (0, 1),
                (1, 1),
                (0, 1),
                (-1, 1),
                (0, 1),
                (-1, 0),
            ],
        ),
        (
            (7, 7),
            &[
                (0, -1),
                (1, -1),
                (0, -1),
                (1, -1),
                (1, 0),
                (0, -1),
                (1, -1),
                (1, 0),
            ],
        ),
    ];
    let mut rank = [[0u8; CELLS]; CELLS];
    for ((sx, sy), steps) in LINES {
        let (mut x, mut y) = (sx, sy);
        let last = steps.len();
        for i in 0..=last {
            // Stage 1 at the start of every line, the last stage at its end.
            let r = 1 + (i * (STAGES as usize - 2) / last) as u8;
            if (0..CELLS as i32).contains(&x) && (0..CELLS as i32).contains(&y) {
                let cell = &mut rank[y as usize][x as usize];
                if *cell == 0 || r < *cell {
                    *cell = r;
                }
            }
            if let Some(&(dx, dy)) = steps.get(i) {
                x += dx;
                y += dy;
            }
        }
    }
    rank
}

/// The six faces as (outward normal, u axis, v axis), with `u × v` equal to the
/// normal -- which is what makes the winding below front-facing from outside.
const FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
    ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
];

/// Append the cracks for `block` at `progress` to `out`.
pub fn crack_vertices(block: [i32; 3], progress: f32, out: &mut Vec<FigureVertex>) {
    let stage = stage(progress);
    if stage == 0 {
        return;
    }
    let ranks = ranks();
    let base = [block[0] as f32, block[1] as f32, block[2] as f32];
    let cell = 1.0 / CELLS as f32;
    for (n, u, v) in FACES {
        // The face plane: the block's min corner, pushed to its far side for a
        // positive normal, then lifted just outside.
        let mut origin = base;
        for k in 0..3 {
            if n[k] > 0.0 {
                origin[k] += 1.0;
            }
            origin[k] += n[k] * LIFT;
        }
        let at = |a: f32, b: f32| {
            [
                origin[0] + u[0] * a + v[0] * b,
                origin[1] + u[1] * a + v[1] * b,
                origin[2] + u[2] * a + v[2] * b,
            ]
        };
        for (j, row) in ranks.iter().enumerate() {
            for (i, &r) in row.iter().enumerate() {
                if r == 0 || r > stage {
                    continue;
                }
                let (a0, b0) = (i as f32 * cell, j as f32 * cell);
                let (a1, b1) = (a0 + cell, b0 + cell);
                let (p00, p10, p11, p01) = (at(a0, b0), at(a1, b0), at(a1, b1), at(a0, b1));
                for pos in [p00, p10, p11, p00, p11, p01] {
                    out.push(FigureVertex { pos, color: COLOR });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    fn cracks(progress: f32) -> Vec<FigureVertex> {
        let mut v = Vec::new();
        crack_vertices([10, 20, -5], progress, &mut v);
        v
    }

    #[test]
    fn nothing_shows_before_the_first_tenth() {
        assert!(cracks(0.0).is_empty());
        assert!(cracks(0.09).is_empty());
        assert!(!cracks(0.1).is_empty());
    }

    #[test]
    fn the_crack_only_ever_grows() {
        let mut last = 0;
        for s in 1..STAGES {
            // Mid-stage: 0.7 in f32 is 0.6999..., which is stage 6, not 7.
            let n = cracks((s as f32 + 0.5) / STAGES as f32).len();
            assert!(n > last, "stage {s} drew {n}, stage {} drew {last}", s - 1);
            last = n;
        }
        assert_eq!(cracks(1.0).len(), last, "done is the last stage, not more");
    }

    /// Culling is on for this pipeline, so a crack wound the wrong way is
    /// invisible from outside -- the one direction anyone looks from.
    #[test]
    fn every_triangle_faces_out_of_the_block() {
        let centre = [10.5, 20.5, -4.5];
        let v = cracks(0.95);
        assert!(!v.is_empty());
        for tri in v.chunks(3) {
            let (a, b, c) = (tri[0].pos, tri[1].pos, tri[2].pos);
            let normal = cross(sub(b, a), sub(c, a));
            let outward = sub(a, centre);
            let dot = normal[0] * outward[0] + normal[1] * outward[1] + normal[2] * outward[2];
            assert!(dot > 0.0, "a triangle at {a:?} faces into the block");
        }
    }

    #[test]
    fn cracks_sit_just_outside_the_block_on_all_six_faces() {
        let v = cracks(0.95);
        let mut seen = [false; 6];
        for x in &v {
            let p = x.pos;
            // Every vertex is on one face plane, lifted by exactly LIFT.
            let on = |k: usize, value: f32| (p[k] - value).abs() < 1e-4;
            let planes = [
                on(0, 11.0 + LIFT),
                on(0, 10.0 - LIFT),
                on(1, 21.0 + LIFT),
                on(1, 20.0 - LIFT),
                on(2, -4.0 + LIFT),
                on(2, -5.0 - LIFT),
            ];
            let hit = planes.iter().position(|&h| h);
            let face = hit.unwrap_or_else(|| panic!("a vertex off every face: {p:?}"));
            seen[face] = true;
        }
        assert_eq!(seen, [true; 6], "a face with no cracks");
    }
}
