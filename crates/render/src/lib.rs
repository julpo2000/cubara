//! GPU renderer.
//!
//! Owns the wgpu device/surface, the render pipeline, and per-frame draw submission,
//! plus view-frustum [`culling`] and optional CPU [`profiling`]. Consumes meshes from
//! [`cubara_voxel`] and the scene from [`cubara_world`]; the shared building blocks
//! (pipeline, depth view, camera uniform, the [`ChunkArena`]) are public so headless
//! paths (benchmark, screenshot) render exactly what the window does. Receives a
//! [`CameraPose`] to render from -- it owns no camera state of its own, no input, no
//! movement (`ARCHITECTURE.md` Rule 3; camera movement is `cubara-sim`'s job as of
//! block 1.6).

mod arena;
pub mod culling;
pub mod headless;
pub mod materials;
mod mesher;
pub mod profiling;
mod render;
mod scene;
mod text;

pub use arena::{ArenaUsage, ChunkArena};
pub use culling::{Aabb, Frustum};
pub use headless::{Frame, Shot};
pub use materials::MeshAssets;
pub use profiling::Profiler;
pub use render::{
    build_pipeline, camera_bind_group_layout, create_depth_view, gpu_driven_features, grab_cursor,
    load_mesh_assets, CameraPose, CameraUniform, Renderer,
};
pub use scene::SceneRenderer;
pub use text::TextRenderer;
