//! CPU-side mesh data shared between world generation and the renderer.
//!
//! Pure data: this crate knows nothing about the GPU (`ARCHITECTURE.md` Rule 3/4),
//! so the world can be generated and meshed headlessly, with no adapter and no
//! `wgpu`. The matching `wgpu` vertex layout for [`Vertex`] lives with the code
//! that owns pipelines, in `cubara-render`.

/// A single mesh vertex: object-space position, normal, and an ambient-occlusion
/// term in `0.0..=1.0` (1 = fully lit, 0 = fully occluded), baked per vertex by the
/// mesher and interpolated across each face.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub ao: f32,
}

/// An indexed triangle mesh ready to be uploaded to the GPU.
#[derive(Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Shift every vertex by `offset` (used to place a chunk in the world).
    pub fn translate(&mut self, offset: [f32; 3]) {
        for v in &mut self.vertices {
            v.position[0] += offset[0];
            v.position[1] += offset[1];
            v.position[2] += offset[2];
        }
    }
}
