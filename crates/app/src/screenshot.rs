//! Headless single-frame screenshot, for visual verification without a window.
//!
//! A thin wrapper over [`cubara_render::headless::render`] — the same code the
//! golden-image tests use, which in turn goes through the one scene-render path the
//! window uses (`ARCHITECTURE.md` Rule 5). This file deliberately contains no
//! rendering of its own; when it did, it drifted from the game and stopped proving
//! anything.
//!
//! Run with: `cargo run --release -- --screenshot out.png`, or from a place:
//! `--screenshot out.png --eye X,Y,Z --look DX,DY,DZ [--radius N] [--size WxH]`.

use cubara_render::materials::TextureLayers;
use cubara_render::{headless, load_registry, Lighting, Shot};
use cubara_voxel::ChunkCoord;
use cubara_world::mesh::{mesh_nodes, mesh_region};
use cubara_world::node::{desired_nodes_3d, schedule_for_radius};
use cubara_world::World;

use crate::streaming::{to_meshed_node, VERTICAL_LOD_SQUASH};

/// Where a screenshot looks from. `None` is the default shot: the orbit over
/// the old three-layer slab around the origin, as it always was.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: [f32; 3],
    pub look: [f32; 3],
    /// Render distance in chunks.
    pub radius: i32,
    pub size: (u32, u32),
}

/// Fog for `shot`'s own `region_radius` -- not `streaming::render_radius_blocks()`
/// (the *window*'s fixed schedule), since `--screenshot --radius N` can ask
/// for a region smaller or larger than the game streams: reusing a fixed
/// number here would put fog past this shot's own meshed region exactly the
/// way `FAR_PLANE` put it past the window's, for a different fixed value.
/// A screenshot is meant to show what the game looks like, fog included --
/// without this, `--screenshot` was the one entry point the review found
/// where it never did.
fn shot_with_fog(shot: Shot) -> Shot {
    let radius_blocks = (shot.region_radius * 16) as f32;
    let (fog_start, fog_end) = Lighting::fog_range(radius_blocks);
    Shot {
        lighting: Lighting {
            fog_start,
            fog_end,
            ..Lighting::default()
        },
        ..shot
    }
}

pub fn run(path: &str, view: Option<View>, menu: Option<String>) {
    let world = World::new();
    let registry = load_registry();
    let layers = TextureLayers::from_registry(&registry);
    let layer_of = |name: &str| layers.layer_of(name);
    let blocks = cubara_world::TerrainBlocks::from_registry(&registry)
        .with_oak(&crate::game::load_structure_registry(), &registry)
        .with_ores(&crate::game::load_ore_registry(), &registry);
    // The far terrain, for a shot from an eye: what the player would see
    // beyond the voxels (`docs/PROPOSAL_FAR_VIEW.md`). The orbit frames the
    // voxel region from outside and gets none, as the bench's orbit does.
    let mut far = Vec::new();
    let mut far_params = None;
    let (shot, built) = match view {
        None => {
            let shot = Shot::default();
            let schedule = schedule_for_radius(shot.region_radius);
            let built = mesh_region(
                &world,
                &registry,
                &layer_of,
                ChunkCoord::new(0, 0, 0),
                0..=2,
                &schedule,
                blocks,
            );
            (shot_with_fog(shot), built)
        }
        // The nodes the game streams around a player standing at `eye`.
        Some(v) => {
            let centre = ChunkCoord::from_world_pos(v.eye);
            let nodes =
                desired_nodes_3d(centre, VERTICAL_LOD_SQUASH, &schedule_for_radius(v.radius));
            let built = mesh_nodes(&world, &registry, &layer_of, nodes, blocks);
            let shot = Shot {
                width: v.size.0,
                height: v.size.1,
                region_radius: v.radius,
                camera: Some((glam::Vec3::from(v.eye), glam::Vec3::from(v.look))),
                ..Shot::default()
            };
            let (min, max) =
                cubara_world::node::covered_box_3d(centre, &schedule_for_radius(v.radius));
            let hole = cubara_world::far::Hole {
                min: min.map(|c| (c * 16) as f64),
                max: max.map(|c| (c * 16) as f64),
            };
            // This PC's quality, as the game would draw it.
            let quality = crate::settings::load(&crate::settings::settings_path())
                .unwrap_or_default()
                .far;
            let eye = v.eye.map(|c| c as f64);
            if let Some(view) = crate::far_streaming::far_view(eye, v.size.1, Some(hole), quality) {
                far = cubara_world::far::build(&cubara_world::WorldGen::new(world.seed()), &view);
            }
            far_params = Some(crate::far_streaming::to_far_params(
                hole,
                crate::far_streaming::far_materials(),
            ));
            // Fog at the far terrain's edge, as in the game.
            let (fog_start, fog_end) =
                Lighting::fog_range(crate::far_streaming::FAR_VIEW_RADIUS as f32);
            let shot = Shot {
                lighting: Lighting {
                    fog_start,
                    fog_end,
                    ..Lighting::default()
                },
                ..shot
            };
            (shot, built)
        }
    };
    let shot = Shot { menu, ..shot };
    let meshed = built.into_iter().filter_map(to_meshed_node);
    let patches: Vec<_> = far.iter().map(crate::far_streaming::to_far_patch).collect();
    let Some(frame) = headless::render_with_far(meshed, &patches, far_params, shot) else {
        log::error!("no suitable GPU adapter — cannot render a screenshot");
        return;
    };

    image::save_buffer(
        path,
        &frame.pixels,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    log::info!(
        "screenshot written to {path} ({}x{})",
        frame.width,
        frame.height
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_with_fog_stays_inside_its_own_region_regardless_of_radius() {
        // The bug class this exists to avoid repeating: a fixed fog range
        // reused across shots of different sizes lands outside a smaller
        // one's meshed geometry, same as `FAR_PLANE` did for the window.
        for region_radius in [3, 6, 32, 64] {
            let shot = shot_with_fog(Shot {
                region_radius,
                ..Shot::default()
            });
            let radius_blocks = (region_radius * 16) as f32;
            assert!(
                shot.lighting.fog_end < radius_blocks,
                "region_radius {region_radius}: fog_end must land inside this shot's own region"
            );
        }
    }
}
