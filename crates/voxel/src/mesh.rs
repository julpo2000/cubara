//! CPU-side mesh data shared between world generation and the renderer.

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

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32];

impl Vertex {
    pub const fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &VERTEX_ATTRS,
        }
    }
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
