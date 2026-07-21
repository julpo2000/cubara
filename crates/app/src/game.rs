//! The game: world state, the player's camera, and what input does to them.
//!
//! Deliberately separate from the renderer (`ARCHITECTURE.md` Rule 3). The renderer
//! draws what it is given; it does not decide where the player is looking or which
//! block a click breaks. When those lived on `Renderer` it could place blocks, which
//! is the boundary error the rule names — and the pattern that makes the reference
//! anti-pattern codebase impossible to change one system at a time.
//!
//! Nothing here touches the GPU, so all of it is testable without an adapter.

use std::sync::Arc;

use cubara_render::FlyCamera;
use cubara_voxel::ChunkCoord;
use cubara_world::World;

use winit::keyboard::KeyCode;

/// How far the block-editing ray reaches, in blocks.
const EDIT_REACH: f32 = 6.0;

/// Everything the player *is* and *does*: the world they are in and their camera.
pub struct Game {
    /// The world being played. Behind an [`Arc`] so meshing jobs can carry the exact
    /// snapshot they were queued against; an edit publishes a new one.
    world: Arc<World>,
    camera: FlyCamera,
}

impl Game {
    /// Start above the terrain near the origin, looking out over it and slightly
    /// down (yaw ~35°, gentle downward pitch).
    pub fn new() -> Self {
        Self {
            world: Arc::new(World::new()),
            camera: FlyCamera::new(glam::vec3(0.0, 48.0, 0.0), 0.6, -0.3),
        }
    }

    pub fn world(&self) -> &Arc<World> {
        &self.world
    }

    pub fn camera(&self) -> &FlyCamera {
        &self.camera
    }

    /// Feed a key press/release to the camera. Returns whether it was consumed.
    pub fn key_input(&mut self, key: KeyCode, pressed: bool) -> bool {
        self.camera.key(key, pressed)
    }

    /// Feed a raw mouse-motion delta (pixels) to the camera's look.
    pub fn mouse_look(&mut self, dx: f32, dy: f32) {
        self.camera.mouse_look(dx, dy);
    }

    /// Advance the player by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        self.camera.update(dt);
    }

    /// Break (`place = false`) or place (`true`) the block the camera is looking at,
    /// within [`EDIT_REACH`]. Returns the [`ChunkCoord`] whose geometry is now stale
    /// so the caller can re-mesh it, or `None` if nothing was in reach.
    ///
    /// Placing puts the block against the hit face.
    pub fn edit_block(&mut self, place: bool) -> Option<ChunkCoord> {
        let origin = self.camera.pos.to_array();
        let dir = self.camera.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, EDIT_REACH)?;
        let target = if place {
            [
                hit.block[0] + hit.normal[0],
                hit.block[1] + hit.normal[1],
                hit.block[2] + hit.normal[2],
            ]
        } else {
            hit.block
        };
        // Publishes a fresh snapshot: workers holding the old Arc keep meshing the
        // pre-edit world, and their results are superseded by the re-mesh request.
        Some(Arc::make_mut(&mut self.world).set_block(target[0], target[1], target[2], place))
    }
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaking_clears_the_targeted_block() {
        // No GPU involved — this is why gameplay does not belong on the renderer.
        let mut game = Game::new();
        // Look straight down from above the terrain.
        game.camera = FlyCamera::new(glam::vec3(0.5, 60.0, 0.5), 0.0, -1.5);
        let hit = game
            .world()
            .raycast([0.5, 60.0, 0.5], [0.0, -1.0, 0.0], 100.0)
            .expect("ground below");

        // Out of reach from 60 blocks up: nothing changes.
        assert_eq!(game.edit_block(false), None);
        assert!(game
            .world()
            .is_solid_at(hit.block[0], hit.block[1], hit.block[2]));
    }

    #[test]
    fn editing_within_reach_marks_a_chunk_dirty() {
        let mut game = Game::new();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0)
            .expect("ground below");
        // Stand just above the surface, looking down — now it is within reach.
        let eye = glam::vec3(0.5, ground.block[1] as f32 + 3.5, 0.5);
        game.camera = FlyCamera::new(eye, 0.0, -1.5);

        let dirty = game.edit_block(false).expect("a block was in reach");
        assert!(
            !game
                .world()
                .is_solid_at(ground.block[0], ground.block[1], ground.block[2]),
            "the targeted block is now air"
        );
        let b = ground.block;
        assert_eq!(
            dirty,
            ChunkCoord::from_world_pos([b[0] as f32, b[1] as f32, b[2] as f32]),
            "the dirty chunk is the one containing the broken block"
        );
    }
}
