//! GPU renderer.
//!
//! Owns the wgpu device/surface, the render pipeline, and per-frame draw submission,
//! plus view-frustum [`culling`] and optional CPU [`profiling`]. Consumes meshes from
//! [`cubara_voxel`] -- nothing else; this crate never depends on `cubara_world`
//! (`ARCHITECTURE.md` §1). What to stream in is entirely the caller's decision
//! ([`ChunkArena::from_meshed`] for a one-shot scene, [`Renderer::apply_node_updates`]
//! for the live, incremental case); the shared building blocks (pipeline, depth
//! view, camera uniform, the [`ChunkArena`]) are public so headless paths
//! (benchmark, screenshot) render exactly what the window does. Receives a
//! [`CameraPose`] to render from -- it owns no camera state of its own, no input, no
//! movement (`ARCHITECTURE.md` Rule 3; camera movement is `cubara-sim`'s job as of
//! block 1.6).

mod arena;
pub mod culling;
pub mod headless;
pub mod materials;
pub mod panel;
pub mod profiling;
mod render;
mod scene;
mod text;

pub use arena::{ArenaUsage, ChunkArena, MeshedNode, NodeId};
pub use culling::{Aabb, Frustum};
pub use headless::{Frame, Shot};
pub use materials::{swatch_color, MeshAssets};
pub use panel::{InventoryPanel, PanelSlot, PanelSlotKind};
pub use profiling::Profiler;
pub use render::{
    build_pipeline, camera_bind_group_layout, create_depth_view, gpu_driven_features, grab_cursor,
    load_mesh_assets, load_registry, CameraPose, CameraUniform, Renderer,
};
pub use scene::{HotbarSlot, HotbarView, PanelView, SceneFrame, SceneRenderer};
pub use text::TextRenderer;
