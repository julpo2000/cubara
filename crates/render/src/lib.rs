//! GPU renderer.
//!
//! Owns the wgpu device/surface, the render pipeline, camera, and per-frame draw
//! submission, plus view-frustum [`culling`] and optional CPU [`profiling`]. Consumes
//! meshes from [`cubara_voxel`] and the scene from [`cubara_world`]; the shared
//! building blocks (pipeline, depth view, camera uniform, world upload) are public
//! so headless paths (benchmark, screenshot) render exactly what the window does.

pub mod culling;
pub mod profiling;
mod render;

pub use culling::{Aabb, Frustum};
pub use profiling::Profiler;
pub use render::{
    build_pipeline, camera_bind_group_layout, chunks_bounds, create_depth_view, upload_chunk,
    upload_region, CameraUniform, ChunkGpu, Renderer,
};
