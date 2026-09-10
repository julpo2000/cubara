//! A blocky human figure, as triangles.
//!
//! Towards the owner's goal: *the game on both screens, each showing the
//! character running on the other laptop.* This is what a player looks like.
//!
//! # What was decided, and by whom
//!
//! The owner asked for Minecraft Steve's look, then for **a red and a green
//! shirt**. So: Steve's *proportions*, which are measurements anybody may use,
//! and this project's own colours.
//!
//! The distinction is deliberate and it is not pedantry. A blocky human of six
//! boxes with those proportions is the generic shape every voxel game uses.
//! Steve's actual texture — that face, the cyan shirt, the brown hairline — is
//! Mojang's artwork, and copying it into a game that might be shared is a real
//! problem rather than a formality. Red and green are not their palette, and
//! asking for them removed the question entirely.
//!
//! # Why the geometry is built on the CPU
//!
//! A figure is six boxes and there will be a handful of players. Building the
//! triangles per frame costs 216 vertices each, which is nothing beside the
//! ~900,000 the terrain draws — and it buys something worth more than the
//! saving: [`figure_vertices`] is a pure function, so where a player's arm ends
//! up is testable with no GPU, no window and no adapter. Every other rendering
//! claim in this project has to be checked against an image; this one can be
//! checked against a number.
//!
//! # Proportions
//!
//! In Minecraft pixels, where sixteen pixels make one block: head 8×8×8, torso
//! 8×4×12, each arm and leg 4×4×12. Two blocks tall in total.

/// One player, as the renderer needs them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerView {
    /// Eye position, which is what the simulation tracks — the figure is built
    /// downward from it, so a player's head is where their camera is.
    pub eye: [f32; 3],
    /// Heading, radians. 0 looks toward −Z, matching `Angle`.
    pub yaw: f32,
    /// Shirt colour: torso and arms.
    pub shirt: [f32; 3],
}

/// A vertex of a figure: a world-space position and a colour.
///
/// Pre-transformed on the CPU, so the shader is a passthrough and there is no
/// per-figure uniform to bind. With six boxes per player that is the cheaper
/// arrangement as well as the simpler one.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FigureVertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
}

/// **A figure is drawn no bigger than the body the simulation collides with.**
///
/// These mirror `cubara_sim::physics`, and `cubara-render` cannot import them:
/// it depends on neither the sim nor the world crate, and taking a dependency
/// so a placeholder can read three numbers would be the wrong trade. They are
/// written out here and pinned by a test in `cubara-app`, which sees both.
///
/// The first version of this file used Minecraft's model dimensions directly —
/// 32 pixels tall, 16 across the shoulders, which is 2.0 by 1.0 blocks. That is
/// bigger than this project's player *in both directions*, and a metre wide
/// against a 0.6-wide collision box means a figure visibly overlapping a wall
/// it could walk past. The owner said they looked too big; measuring said the
/// width was the real error.
pub const PLAYER_HEIGHT: f32 = 1.8;
pub const PLAYER_WIDTH: f32 = 0.6;
pub const EYE_HEIGHT: f32 = 1.62;

/// The model is laid out in Minecraft pixels — 32 tall, 16 across — and then
/// scaled to the box above. Vertical and horizontal scales differ, so the
/// figure is slimmer than Steve rather than the same shape at a smaller size.
/// That is deliberate: fitting the collision box is a property worth having,
/// and looking exactly like somebody else's character is not.
const SCALE_Y: f32 = PLAYER_HEIGHT / 32.0;
const SCALE_XZ: f32 = PLAYER_WIDTH / 16.0;

/// How far below the crown the eye sits, in the model's own pixels.
const EYE_BELOW_CROWN: f32 = PLAYER_HEIGHT - EYE_HEIGHT;

/// Skin, for the parts the owner did not name a colour for.
const SKIN: [f32; 3] = [0.86, 0.71, 0.56];
/// Trousers.
const LEGS: [f32; 3] = [0.24, 0.30, 0.55];

/// A box, in Minecraft pixels, relative to the crown of the head.
struct Part {
    /// Centre offset from the crown: +x right, +y up, +z back.
    centre: [f32; 3],
    /// Half-extents.
    half: [f32; 3],
    color: Color,
}

enum Color {
    Skin,
    Shirt,
    Legs,
}

/// The six boxes, in a fixed order so two machines build the same vertex list.
fn parts() -> [Part; 6] {
    // Measured downward from the crown: head 8 tall, torso 12, legs 12.
    [
        Part {
            centre: [0.0, -4.0, 0.0],
            half: [4.0, 4.0, 4.0],
            color: Color::Skin,
        },
        Part {
            centre: [0.0, -14.0, 0.0],
            half: [4.0, 6.0, 2.0],
            color: Color::Shirt,
        },
        Part {
            centre: [-6.0, -14.0, 0.0],
            half: [2.0, 6.0, 2.0],
            color: Color::Shirt,
        },
        Part {
            centre: [6.0, -14.0, 0.0],
            half: [2.0, 6.0, 2.0],
            color: Color::Shirt,
        },
        Part {
            centre: [-2.0, -26.0, 0.0],
            half: [2.0, 6.0, 2.0],
            color: Color::Legs,
        },
        Part {
            centre: [2.0, -26.0, 0.0],
            half: [2.0, 6.0, 2.0],
            color: Color::Legs,
        },
    ]
}

/// The triangles for one player, in world space.
///
/// Pure: same input, same vertices, on any machine. That is what lets the tests
/// below assert where an arm is without an adapter.
pub fn figure_vertices(view: PlayerView, out: &mut Vec<FigureVertex>) {
    let (sin, cos) = view.yaw.sin_cos();
    let crown = [view.eye[0], view.eye[1] + EYE_BELOW_CROWN, view.eye[2]];

    for part in parts() {
        let color = match part.color {
            Color::Skin => SKIN,
            Color::Shirt => view.shirt,
            Color::Legs => LEGS,
        };
        // Corners in the figure's own frame, then turned by yaw about its
        // vertical axis and placed in the world.
        let mut corner = [[0.0f32; 3]; 8];
        for (i, c) in corner.iter_mut().enumerate() {
            let sx = if i & 1 == 0 { -1.0 } else { 1.0 };
            let sy = if i & 2 == 0 { -1.0 } else { 1.0 };
            let sz = if i & 4 == 0 { -1.0 } else { 1.0 };
            let lx = (part.centre[0] + sx * part.half[0]) * SCALE_XZ;
            let ly = (part.centre[1] + sy * part.half[1]) * SCALE_Y;
            let lz = (part.centre[2] + sz * part.half[2]) * SCALE_XZ;
            *c = [
                crown[0] + lx * cos - lz * sin,
                crown[1] + ly,
                crown[2] + lx * sin + lz * cos,
            ];
        }

        // Two triangles per face, six faces. Indices into `corner`, wound so
        // the outside is front-facing.
        const FACES: [[usize; 6]; 6] = [
            [0, 2, 3, 0, 3, 1], // -z
            [5, 7, 6, 5, 6, 4], // +z
            [4, 6, 2, 4, 2, 0], // -x
            [1, 3, 7, 1, 7, 5], // +x
            [2, 6, 7, 2, 7, 3], // +y
            [4, 0, 1, 4, 1, 5], // -y
        ];
        for face in FACES {
            for i in face {
                out.push(FigureVertex {
                    pos: corner[i],
                    color,
                });
            }
        }
    }
}

/// How many vertices one figure contributes: six boxes, six faces, two
/// triangles each.
pub const VERTICES_PER_FIGURE: usize = 6 * 6 * 6;

#[cfg(test)]
mod tests {
    use super::*;

    fn view(yaw: f32) -> PlayerView {
        PlayerView {
            eye: [0.0, 0.0, 0.0],
            yaw,
            shirt: [1.0, 0.0, 0.0],
        }
    }

    fn bounds(v: &[FigureVertex]) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for x in v {
            for a in 0..3 {
                lo[a] = lo[a].min(x.pos[a]);
                hi[a] = hi[a].max(x.pos[a]);
            }
        }
        (lo, hi)
    }

    #[test]
    fn a_figure_fits_the_body_the_simulation_collides_with() {
        let mut v = Vec::new();
        figure_vertices(view(0.0), &mut v);
        assert_eq!(v.len(), VERTICES_PER_FIGURE);

        let (lo, hi) = bounds(&v);
        let height = hi[1] - lo[1];
        assert!(
            (height - PLAYER_HEIGHT).abs() < 1e-5,
            "a figure should be exactly as tall as the collision box \
             ({PLAYER_HEIGHT}), got {height}"
        );
        let width = hi[0] - lo[0];
        assert!(
            width <= PLAYER_WIDTH + 1e-5,
            "a figure {width} wide does not fit a body {PLAYER_WIDTH} wide -- it \
             would visibly overlap a wall it could walk past"
        );

        // The eye is at y = 0, so the feet sit exactly `EYE_HEIGHT` below it.
        // Getting this wrong makes everybody float or sink, which reads as a
        // physics bug rather than a drawing one.
        assert!(
            (lo[1] + EYE_HEIGHT).abs() < 1e-5,
            "the feet are at {}, not {EYE_HEIGHT} below the eye",
            lo[1]
        );
    }

    /// Turning the figure turns it, and turning it a quarter swaps its width
    /// and depth.
    ///
    /// A figure is wider than it is deep — 8 pixels across the shoulders, 4
    /// front to back — so this is the cheapest assertion that yaw is applied at
    /// all *and* applied about the right axis.
    #[test]
    fn yaw_turns_the_figure_about_its_own_axis() {
        let mut facing = Vec::new();
        figure_vertices(view(0.0), &mut facing);
        let (flo, fhi) = bounds(&facing);

        let mut turned = Vec::new();
        figure_vertices(view(std::f32::consts::FRAC_PI_2), &mut turned);
        let (tlo, thi) = bounds(&turned);

        let wide = fhi[0] - flo[0];
        let deep = fhi[2] - flo[2];
        assert!(
            wide > deep,
            "shoulders should be wider than the body is deep"
        );

        assert!(
            ((thi[2] - tlo[2]) - wide).abs() < 1e-4,
            "a quarter turn should put the shoulders across Z"
        );
        assert!(
            ((thi[1] - tlo[1]) - PLAYER_HEIGHT).abs() < 1e-5,
            "turning changed the figure's height, so it is not rotating about Y"
        );
    }

    /// The shirt colour reaches the torso and arms, and nothing else.
    #[test]
    fn the_shirt_colours_the_torso_and_arms_only() {
        let mut v = Vec::new();
        figure_vertices(
            PlayerView {
                eye: [0.0, 0.0, 0.0],
                yaw: 0.0,
                shirt: [1.0, 0.0, 0.0],
            },
            &mut v,
        );
        let shirt = v.iter().filter(|x| x.color == [1.0, 0.0, 0.0]).count();
        // Three of the six boxes wear it.
        assert_eq!(
            shirt,
            VERTICES_PER_FIGURE / 2,
            "the shirt should cover exactly the torso and two arms"
        );
        assert!(
            v.iter().any(|x| x.color == SKIN),
            "the head is not skin-coloured"
        );
        assert!(v.iter().any(|x| x.color == LEGS), "the legs have no colour");
    }

    /// Two players with different shirts produce different geometry, and the
    /// same player produces the same geometry twice.
    #[test]
    fn the_same_player_builds_the_same_vertices() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        figure_vertices(view(1.2), &mut a);
        figure_vertices(view(1.2), &mut b);
        assert_eq!(a, b);

        let mut green = Vec::new();
        figure_vertices(
            PlayerView {
                shirt: [0.0, 1.0, 0.0],
                ..view(1.2)
            },
            &mut green,
        );
        assert_ne!(a, green, "two shirts produced identical geometry");
    }

    /// A figure is placed at its player, not at the origin.
    #[test]
    fn a_figure_follows_its_player() {
        let mut here = Vec::new();
        figure_vertices(view(0.0), &mut here);
        let mut there = Vec::new();
        figure_vertices(
            PlayerView {
                eye: [100.0, 40.0, -25.0],
                ..view(0.0)
            },
            &mut there,
        );
        let (hlo, _) = bounds(&here);
        let (tlo, _) = bounds(&there);
        assert!((tlo[0] - hlo[0] - 100.0).abs() < 1e-3);
        assert!((tlo[1] - hlo[1] - 40.0).abs() < 1e-3);
        assert!((tlo[2] - hlo[2] + 25.0).abs() < 1e-3);
    }
}
