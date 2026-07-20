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
mod mesher;
pub mod profiling;
mod render;
mod text;

pub use arena::ChunkArena;
pub use camera::FlyCamera;
pub use culling::{Aabb, Frustum};
pub use profiling::Profiler;
pub use render::{
    build_pipeline, camera_bind_group_layout, create_depth_view, gpu_driven_features,
    CameraUniform, Renderer,
};
pub use text::TextRenderer;
