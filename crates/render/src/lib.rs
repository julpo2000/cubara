//! GPU renderer.
//!
//! Owns the wgpu device/surface, the render pipeline, camera, and per-frame draw
//! submission, plus view-frustum [`culling`] and optional CPU [`profiling`]. Consumes
//! meshes from [`cubara_voxel`] and the scene from [`cubara_world`]; the shared
//! building blocks (pipeline, depth view, camera uniform, the [`ChunkArena`]) are
//! public so headless paths (benchmark, screenshot) render exactly what the window
//! does.

mod arena;
mod camera;
pub mod culling;
pub mod headless;
mod mesher;
pub mod profiling;
mod render;
mod scene;
mod text;

pub use arena::ChunkArena;
pub use camera::FlyCamera;
pub use culling::{Aabb, Frustum};
pub use headless::{Frame, Shot};
pub use profiling::Profiler;
pub use render::{
    build_pipeline, camera_bind_group_layout, create_depth_view, gpu_driven_features, grab_cursor,
    CameraUniform, Renderer,
};
pub use scene::SceneRenderer;
pub use text::TextRenderer;
