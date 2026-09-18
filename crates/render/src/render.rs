//! GPU bring-up and per-frame rendering.
//!
//! Owns the wgpu surface/device/queue and the render pipeline. All resident
//! node geometry lives in a shared [`ChunkArena`], drawn with a single
//! indirect submit. The renderer does not decide what to stream in --
//! `ARCHITECTURE.md` §1: its inputs are meshes, origins and a camera, nothing
//! that knows what a chunk or a `World` is. The caller (`cubara-app`) works
//! out which nodes are wanted (via `cubara_world`), meshes them, and hands
//! the results to [`Renderer::apply_node_updates`]. The shared building
//! blocks (pipeline, depth view, camera) are public so the headless
//! bench/screenshot paths build the same scene.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use winit::window::{CursorGrabMode, Window};

use cubara_voxel::{BlockRegistry, ChunkCoord, Vertex};

use crate::arena::{ChunkArena, MeshedNode, NodeId};
use crate::culling::Frustum;
use crate::materials::{self, MeshAssets};
use crate::scene::{SceneFrame, SceneRenderer};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Camera near/far planes. The far plane covers radius 64's diagonal
/// (64 chunks x 16 blocks = 1,024, so ~1,448 corner to corner) with room over.
const NEAR_PLANE: f32 = 0.1;
const FAR_PLANE: f32 = 2000.0;

/// Depth cleared at the *far* plane, since [`reverse_z`] puts it at 0.
pub const DEPTH_CLEAR: f64 = 0.0;

/// Flip a projection's depth so it runs 1 at the near plane down to 0 at the
/// far plane -- "reversed-Z".
///
/// Paired with a float depth buffer (`DEPTH_FORMAT` is `Depth32Float`) this is
/// close to the best depth precision available, and it is nearly free. A
/// float's precision is concentrated near zero. The conventional mapping
/// spends that precision on the *near* plane, where it is least needed --
/// everything there is close and large -- and leaves almost none for the
/// distance. Reversing it puts the far plane at zero instead.
///
/// That matters here specifically because the near/far ratio is
/// 0.1 : 2,000, i.e. 20,000:1, and block 1.10 made the far end of that range
/// somewhere the player actually looks.
///
/// Built by flipping a standard projection's depth (`z' = w - z`) rather than
/// by swapping the near and far arguments to `perspective_rh`, which produces
/// the same matrix far less obviously.
pub fn reverse_z(proj: glam::Mat4) -> glam::Mat4 {
    let flip = glam::Mat4::from_cols(
        glam::vec4(1.0, 0.0, 0.0, 0.0),
        glam::vec4(0.0, 1.0, 0.0, 0.0),
        glam::vec4(0.0, 0.0, -1.0, 0.0),
        glam::vec4(0.0, 0.0, 1.0, 1.0),
    );
    flip * proj
}

/// The `(A, B)` such that `view_depth = A / (ndc_z + B)` inverts
/// [`reverse_z`]'s depth -- i.e. `@builtin(position).z` in the fragment
/// stage, which is exactly `ndc_z` (`perspective_rh`'s clip space is already
/// wgpu's 0..1 range, no further remap). `mesh.wgsl`'s fog reads this pair
/// straight from `FrameUniform` instead of carrying its own view-depth
/// varying: derivation below (`reverse_z` only touches the z output row, so
/// start from the un-reversed projection's standard NDC z):
///
/// - Un-reversed `perspective_rh` gives `ndc_z = C - A/d` where `d` is view
///   depth, `C = far/(far-near)`, `A = near*far/(far-near)`.
/// - `reverse_z`'s flip computes `new_clip.z = old_clip.w - old_clip.z` with
///   `new_clip.w` unchanged, so `new_ndc_z = 1 - ndc_z = (1-C) + A/d`.
/// - `1 - C = -near/(far-near) = -B`, so `new_ndc_z = A/d - B` with
///   `B = near/(far-near)` -- solving for `d` gives the `A/(ndc_z+B)` above.
///
/// Computed once from the same [`NEAR_PLANE`]/[`FAR_PLANE`] the projection
/// itself uses, so there is no second place these can drift apart from it;
/// `reverse_z_depth_constants_invert_the_projection_at_several_depths` pins
/// the pair against the actual matrix rather than trusting the algebra above
/// unchecked.
fn reverse_z_depth_constants() -> (f32, f32) {
    let a = NEAR_PLANE * FAR_PLANE / (FAR_PLANE - NEAR_PLANE);
    let b = NEAR_PLANE / (FAR_PLANE - NEAR_PLANE);
    (a, b)
}

/// Load the real `assets/blocks/*.ron` registry, validated against
/// `assets/textures/` -- the GPU-free half of [`load_mesh_assets`], for a
/// caller that needs to mesh nodes (`cubara_world::mesh`, resolving
/// `tex_layer` via `materials::TextureLayers::from_registry`) before, or
/// entirely without, ever building the actual texture array -- e.g.
/// `cubara-app`'s `--screenshot` mode, or a headless test meshing ahead of a
/// separate call that builds the array later. `CARGO_MANIFEST_DIR` is
/// `crates/render`, so `../..` reaches the repo root regardless of the
/// caller's working directory.
pub fn load_registry() -> BlockRegistry {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registry =
        BlockRegistry::load(&repo_root.join("assets/blocks")).expect("assets/blocks must load");
    registry
        .validate_textures(&repo_root.join("assets/textures"))
        .expect("assets/textures must cover every material's faces");
    registry
}

/// [`load_registry`] plus the GPU texture array built from it -- the same
/// materials every entry point (window, `--bench`, `--screenshot`, golden
/// tests) meshes and renders against.
///
/// Returns the CPU-side [`MeshAssets`] (what meshing needs -- ready to `Arc`
/// and share with worker threads) plus the texture array's view and sampler
/// (what [`SceneRenderer`] needs to bind it); these travel separately because
/// they're consumed in different places, not because they're built
/// separately -- `materials::build` does both in one pass.
pub fn load_mesh_assets(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (MeshAssets, wgpu::TextureView, wgpu::Sampler) {
    let registry = load_registry();
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let textures_dir = repo_root.join("assets/textures");
    let (view, sampler, layers) = materials::build(device, queue, &registry, &textures_dir);
    (MeshAssets { registry, layers }, view, sampler)
}

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Uint32, 1 => Uint32, 2 => Uint32];

/// The GPU vertex layout for [`Vertex`], which is plain data in `cubara-voxel` and
/// knows nothing about the GPU (`ARCHITECTURE.md` Rule 3/4). The layout lives here,
/// with the code that owns pipelines. Three `u32` words, unpacked in the shader --
/// see `docs/PHASE1_ARCHITECTURE.md` §5.2 for the bit layout.
///
/// It must stay in step with the field order of [`Vertex`]; `vertex_layout_matches_vertex`
/// below pins the stride so adding a field there fails here instead of silently
/// mis-reading the buffer on the GPU.
pub const fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRS,
    }
}

/// Cap on node geometry uploads per frame. A streaming update can hand over a
/// whole ring's worth of newly-meshed nodes at once; spreading the GPU
/// uploads over a few frames avoids the resulting frame-time spike (nodes pop
/// in a hair later, imperceptibly). *What* to stream is the caller's
/// decision (`ARCHITECTURE.md` §1); this is purely about not spiking the
/// frame while applying it, which is why it stays here rather than moving
/// out with the streaming policy.
const MAX_UPLOADS_PER_FRAME: usize = 32;

/// Uniform block shared with `mesh.wgsl`: one column-major view*projection matrix.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

impl CameraUniform {
    /// Orbit `center` at `radius`, framerate-independent via virtual time `t`.
    pub fn new(aspect: f32, t: f32, center: [f32; 3], radius: f32) -> Self {
        Self::from_matrix(Self::view_proj_matrix(aspect, t, center, radius))
    }

    /// The raw orbit view*projection matrix, exposed so callers can also build a
    /// [`Frustum`] from the exact same camera used for the uniform.
    pub fn view_proj_matrix(aspect: f32, t: f32, center: [f32; 3], radius: f32) -> glam::Mat4 {
        let eye = glam::Vec3::from(Self::orbit_eye(t, center, radius));
        Self::look_view_proj(aspect, eye, glam::Vec3::from(center) - eye)
    }

    /// Where the orbit camera [`view_proj_matrix`](Self::view_proj_matrix)
    /// looks from at time `t` -- one definition, for anything that needs to
    /// know where that camera is.
    pub fn orbit_eye(t: f32, center: [f32; 3], radius: f32) -> [f32; 3] {
        let center = glam::Vec3::from(center);
        let angle = t * 0.15;
        (center + glam::vec3(radius * angle.cos(), radius * 0.45, radius * angle.sin())).to_array()
    }

    /// View*projection for a camera at `eye` looking along `look_dir`, with
    /// reversed-Z depth (see [`reverse_z`]).
    pub fn look_view_proj(aspect: f32, eye: glam::Vec3, look_dir: glam::Vec3) -> glam::Mat4 {
        let proj = reverse_z(glam::Mat4::perspective_rh(
            60f32.to_radians(),
            aspect,
            NEAR_PLANE,
            FAR_PLANE,
        ));
        let view = glam::Mat4::look_at_rh(eye, eye + look_dir, glam::Vec3::Y);
        proj * view
    }

    pub fn from_matrix(m: glam::Mat4) -> Self {
        Self {
            view_proj: m.to_cols_array_2d(),
        }
    }
}

/// Everything about a frame that is not the camera or the geometry: the sun,
/// the ambient floor, the AO floor, and (from block 2 of this package on) the
/// distance fog. One place these live, read by both `mesh.wgsl` and
/// `figure.wgsl`, rather than each shader hard-coding its own sun (they used
/// to disagree: `mesh.wgsl` had `(0.4, 1.0, 0.3)`, `figure.wgsl` had
/// `(0.4, 0.9, 0.25)`).
///
/// `Default` encodes exactly the literals `mesh.wgsl` hard-coded before this
/// existed, and `fog_end <= fog_start` (both `0.0`) is deliberately "fog
/// off" -- the shader treats that pair as a special case rather than relying
/// on distances happening to fall outside some very large range, so a
/// caller that never sets fog gets pixel-identical output to before this
/// type existed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lighting {
    /// Normalized on the CPU; the shader does not renormalize it.
    pub sun_dir: glam::Vec3,
    pub sun_color: glam::Vec3,
    /// How much the sun's `max(dot(n, sun_dir), 0)` term contributes,
    /// separate from `sun_color` so a coloured sun and a dim sun are two
    /// different knobs.
    pub diffuse_weight: f32,
    /// Hemispheric ambient floor/ceiling: ground-facing / sky-facing.
    pub ambient_low: f32,
    pub ambient_high: f32,
    /// Baked ambient occlusion never darkens a surface past this.
    pub ao_floor: f32,
    pub fog_color: glam::Vec3,
    /// Distance at which fog starts blending in, and finishes (fully
    /// `fog_color`). `fog_end <= fog_start` means "no fog", not "fog
    /// starting behind the camera" -- see the type's doc comment.
    pub fog_start: f32,
    pub fog_end: f32,
    /// `0.0..1.0` around a day; unused (fixed at `0.0`) until a later
    /// package turns on the day/night cycle this only carries the plumbing
    /// for.
    pub time_of_day: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            sun_dir: glam::vec3(0.4, 1.0, 0.3).normalize(),
            sun_color: glam::Vec3::ONE,
            diffuse_weight: 0.75,
            ambient_low: 0.28,
            ambient_high: 0.42,
            ao_floor: 0.4,
            // Matches `scene::CLEAR_COLOR` -- fog fading into anything else
            // would draw a visible ring where geometry gives way to sky
            // instead of the two disappearing into each other.
            fog_color: glam::vec3(0.45, 0.62, 0.80),
            fog_start: 0.0,
            fog_end: 0.0,
            time_of_day: 0.0,
        }
    }
}

impl Lighting {
    /// `(fog_start, fog_end)` for a render radius of `radius_blocks`, so fog
    /// dissolves the render-distance ring instead of drawing a hard edge --
    /// `end` a little inside the edge (`- 64.0`, one chunk), `start` at 60%
    /// of the way to it, so the fade has room instead of snapping on in the
    /// last few blocks. Clamped at `0.0`: a radius smaller than 64 blocks
    /// (a bench region tinier than one chunk, say) collapses to
    /// `(0.0, 0.0)`, which is exactly [`Lighting::default`]'s "fog off"
    /// rather than fog starting behind the camera.
    pub fn fog_range(radius_blocks: f32) -> (f32, f32) {
        let end = (radius_blocks - 64.0).max(0.0);
        (0.6 * end, end)
    }
}

/// The uniform actually bound at `@group(0) @binding(0)`: the camera plus
/// [`Lighting`], std140-safe (every field a full `vec4`, so nothing needs
/// manual padding to hit 16-byte alignment). `figure.wgsl` declares the
/// matching `Frame` struct and reads through `fog`; `outline.wgsl` and
/// `mesh.wgsl` both declare only the leading `view_proj` they actually use
/// -- WGSL doesn't require a shader to describe a whole bound buffer, only
/// the prefix it reads, so a smaller struct there stays correct as long as
/// `view_proj` stays first. `mesh.wgsl` gets its lighting and fog from
/// pipeline `override` constants instead ([`mesh_pipeline_constants`]), not
/// this uniform -- see that function's doc comment for why.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FrameUniform {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    sun_dir: [f32; 4],
    /// `.w` is [`Lighting::diffuse_weight`].
    sun_color: [f32; 4],
    /// `.x` [`Lighting::ambient_low`], `.y` `ambient_high`, `.z` `ao_floor`.
    ambient: [f32; 4],
    fog_color: [f32; 4],
    /// `.x` [`Lighting::fog_start`], `.y` `fog_end`, `.z` `time_of_day`.
    fog: [f32; 4],
}

impl FrameUniform {
    pub fn new(view_proj: glam::Mat4, eye: glam::Vec3, lighting: Lighting) -> Self {
        Self {
            view_proj: view_proj.to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, 0.0],
            sun_dir: [
                lighting.sun_dir.x,
                lighting.sun_dir.y,
                lighting.sun_dir.z,
                0.0,
            ],
            sun_color: [
                lighting.sun_color.x,
                lighting.sun_color.y,
                lighting.sun_color.z,
                lighting.diffuse_weight,
            ],
            ambient: [
                lighting.ambient_low,
                lighting.ambient_high,
                lighting.ao_floor,
                0.0,
            ],
            fog_color: [
                lighting.fog_color.x,
                lighting.fog_color.y,
                lighting.fog_color.z,
                0.0,
            ],
            fog: [
                lighting.fog_start,
                lighting.fog_end,
                lighting.time_of_day,
                0.0,
            ],
        }
    }
}

/// A camera position and facing to render from -- the renderer's *entire*
/// idea of "the camera": no input, no movement, no keys held
/// (`ARCHITECTURE.md` Rule 3 -- if the renderer could move the player, the
/// boundary would be wrong). `cubara-app` computes one of these each frame
/// by interpolating the sim's previous and current tick
/// (`docs/PHASE1_ARCHITECTURE.md` §9) and hands it in; headless callers
/// (bench, screenshot, golden tests) build one directly.
#[derive(Clone, Copy, Debug)]
pub struct CameraPose {
    pub eye: glam::Vec3,
    pub look_dir: glam::Vec3,
}

impl CameraPose {
    pub fn view_proj(&self, aspect: f32) -> glam::Mat4 {
        CameraUniform::look_view_proj(aspect, self.eye, self.look_dir)
    }
}

/// Uniform block shared with `outline.wgsl`: the highlighted voxel's
/// world-space min corner. `origin` is `[f32; 4]`, not `[f32; 3]` -- WGSL's
/// uniform address space requires 16-byte alignment for a `vec3<f32>`
/// member, so a plain `vec3` here would need hand-rolled padding to match;
/// a `vec4` with `.w` unused sidesteps that entirely.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OutlineUniform {
    origin: [f32; 4],
}

impl OutlineUniform {
    pub fn new(origin: [f32; 3]) -> Self {
        Self {
            origin: [origin[0], origin[1], origin[2], 0.0],
        }
    }
}

const OUTLINE_VERTEX_ATTRS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];

pub const fn outline_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: (3 * std::mem::size_of::<f32>()) as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &OUTLINE_VERTEX_ATTRS,
    }
}

/// The 12 edges of a unit cube (`0.0..1.0` on each axis, matching one
/// voxel's extent), as 24 line-list vertex positions local to the targeted
/// block. Uploaded once ([`SceneRenderer::new`](crate::scene::SceneRenderer::new));
/// only [`OutlineUniform::origin`] varies per frame.
pub const OUTLINE_CUBE_EDGES: [[f32; 3]; 24] = [
    // Bottom face (y = 0).
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 0.0, 1.0],
    [1.0, 0.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 0.0],
    // Top face (y = 1).
    [0.0, 1.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 1.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
    [0.0, 1.0, 0.0],
    // The four vertical edges connecting them.
    [0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 0.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.0, 1.0, 1.0],
];

/// The wgpu features the GPU-driven path wants, intersected with what `adapter`
/// actually offers — pass the result as `required_features` when requesting the
/// device. Also returns whether `MULTI_DRAW_INDIRECT` made the cut, which selects
/// the arena's fast indirect draw path over the `draw_indexed` fallback (see the
/// #26 spike: both target backends support it, but not all do).
///
/// Deliberately does **not** request `INDIRECT_FIRST_INSTANCE`: block 1.4a
/// tried building per-node origin lookup on `first_instance` +
/// `@builtin(instance_index)` and found it unreliable in *both* directions
/// across real CI backends -- broken with `multi_draw_indexed_indirect` on one
/// software DX12 adapter, broken in the plain `draw_indexed` fallback on
/// another virtualized Metal adapter, with no combination that was safe
/// everywhere. `node_index` is a plain vertex attribute instead (§5.3), so
/// this feature is unused now; see the design doc for the full story.
pub fn gpu_driven_features(adapter: &wgpu::Adapter) -> (wgpu::Features, bool) {
    // wgpu 27 removed the `MULTI_DRAW_INDIRECT` feature flag: plain
    // `multi_draw_indexed_indirect` (not the GPU-decided-count variant,
    // `MULTI_DRAW_INDIRECT_COUNT`, which is a separate feature and still
    // unused here) moved to a downlevel capability instead of an opt-in
    // feature -- the WebGPU spec's baseline apparently grew to expect it,
    // where wgpu 24 didn't.
    let multi_draw = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION);
    // The GPU-timing feature `bench.rs` wants for GPU/frame -- requested here
    // (window, bench, headless all call this) so the feature set is the same
    // everywhere rather than bench alone having a device the others don't.
    //
    // Just the base `TIMESTAMP_QUERY`. Two narrower tiers exist --
    // `TIMESTAMP_QUERY_INSIDE_ENCODERS` (`CommandEncoder::write_timestamp`
    // outside a pass: this used it at first, and it reads back a silent,
    // permanent 0ms on Metal, since wgpu-hal's encoder-level writes there
    // both sample the same "stage boundary") and `TIMESTAMP_QUERY_INSIDE_PASSES`
    // -- but both gate `RenderPass`/`ComputePass::write_timestamp`, the
    // imperative *in-pass* call for writing more than one timestamp per pass
    // (`wgpu-core`'s `command/render.rs`/`compute.rs`, function
    // `write_timestamp`, `require_features(TIMESTAMP_QUERY_INSIDE_PASSES)`).
    // What's actually used here -- `RenderPassDescriptor`/
    // `ComputePassDescriptor`'s own `timestamp_writes` field, set once when
    // the pass begins (`scene.rs`) -- is a different code path in wgpu-core
    // with no extra `require_features` check beyond the base feature.
    // Requesting `INSIDE_PASSES` anyway (an earlier version of this comment
    // did, having misread which function the check belonged to) meant the
    // bench read `n/a` on the M3, which supports base `TIMESTAMP_QUERY` and
    // `INSIDE_ENCODERS` but not `INSIDE_PASSES` -- exactly the machine this
    // number needs to work on.
    let timestamps = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    (timestamps, multi_draw)
}

/// All GPU + window state. Created once the event loop has `resumed`.
///
/// Owns no `World`, no streaming policy, no mesh-generation pool -- what to
/// stream in is decided entirely by the caller (`cubara-app`, via
/// `cubara_world`), which hands finished node geometry to
/// [`apply_node_updates`](Self::apply_node_updates). This is what makes the
/// renderer rebuildable on its own: its whole vocabulary is meshes, origins,
/// and a camera (`ARCHITECTURE.md` §1).
pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    /// The one scene-render path, shared with `--bench` and `--screenshot`.
    scene: SceneRenderer,
    frustum: Frustum,

    /// All resident node geometry in shared buffers, drawn with one indirect submit.
    arena: ChunkArena,
    /// Which node ids are currently meant to be resident (uploaded, or queued
    /// to become so) -- lets [`drain_uploads`](Self::drain_uploads) skip a
    /// queued upload for a node a newer [`apply_node_updates`](Self::apply_node_updates)
    /// call already unloaded, without this crate needing to know what a node
    /// id actually means.
    desired: HashSet<NodeId>,
    /// Finished meshes waiting to be uploaded, drained at most
    /// [`MAX_UPLOADS_PER_FRAME`] per frame to avoid upload spikes.
    upload_queue: VecDeque<MeshedNode>,

    last_frame: Instant,
    visible_chunks: usize,
    frames: u32,
    last_report: Instant,

    /// Whether the F3 debug overlay is shown.
    show_debug: bool,
    /// Smoothed frame time in ms, for a stable on-screen FPS reading.
    frame_ms: f32,
    /// How far out geometry actually streams in, in blocks -- the caller's
    /// (`cubara-app`'s) idea of its own render radius, handed in each
    /// [`render`](Self::render) call rather than this crate guessing at one
    /// (`ARCHITECTURE.md` Rule 3: this crate knows nothing about
    /// `cubara_world`'s streaming schedule). Used to fade distance fog out
    /// at the actual edge of what is drawn -- `FAR_PLANE` alone would put
    /// the fog past where anything is, leaving the old hard edge exactly as
    /// visible as before this existed.
    render_radius_blocks: f32,
}

impl Renderer {
    /// Bring up the window's GPU surface/device/pipelines and return the
    /// renderer alongside the [`MeshAssets`] its texture array was built
    /// from -- the caller needs those to mesh nodes against the same
    /// registry/texture-layer mapping (`cubara_world::mesh`'s `registry`/
    /// `layer_of` parameters), and building the texture array twice would
    /// waste a real upload, so this is the one place it happens.
    ///
    /// Starts with nothing resident -- no `World`, no priming region. The
    /// caller streams the initial view in via
    /// [`apply_node_updates`](Self::apply_node_updates) exactly like every
    /// later frame.
    pub fn new(window: Arc<Window>, camera: CameraPose) -> (Self, MeshAssets) {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("no suitable GPU adapter");

        log::info!("GPU: {:?}", adapter.get_info());

        let (features, multi_draw) = gpu_driven_features(&adapter);
        log::info!("multi_draw_indirect: {multi_draw}");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cubara-device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Uncapped so we can actually measure FPS against the 1000-FPS
            // goal -- and **named explicitly** rather than asked for.
            //
            // This was `AutoNoVsync`, which is a *request*: the backend resolves
            // it to Mailbox, Immediate or Fifo and never says which. Reading
            // `config.present_mode` back gives you `AutoNoVsync` again, so a log
            // line there prints your own question. On this project's Mac the
            // window sat at exactly 60 while `--bench` reached 1,100 -- and
            // `--bench` never presents at all (`bench.rs` asks for an adapter
            // with `compatible_surface: None`), so the two were never measuring
            // the same thing.
            //
            // Choosing from what the surface actually offers means the answer is
            // in `present_mode` instead of hidden behind a word.
            present_mode: chosen_present_mode(&caps),
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            // `Auto` reproduces wgpu's pre-30 behaviour exactly (srgb, or
            // ExtendedSrgbLinear for an fp16 surface) -- no HDR/wide-gamut
            // opt-in here.
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        log::info!(
            "surface present modes offered: {:?}; chose {:?}",
            caps.present_modes,
            config.present_mode
        );
        surface.configure(&device, &config);

        let (mesh_assets, tex_view, tex_sampler) = load_mesh_assets(&device, &queue);
        let scene = SceneRenderer::new(
            &device,
            &queue,
            format,
            config.width,
            config.height,
            &tex_view,
            &tex_sampler,
        );

        let aspect = config.width as f32 / config.height as f32;
        let frustum = Frustum::from_view_proj(camera.view_proj(aspect));

        let arena = ChunkArena::new(&device, multi_draw);

        let renderer = Self {
            window,
            surface,
            device,
            queue,
            config,
            scene,
            frustum,
            arena,
            desired: HashSet::new(),
            upload_queue: VecDeque::new(),
            last_frame: Instant::now(),
            visible_chunks: 0,
            frames: 0,
            last_report: Instant::now(),
            // Off by default (F3 to show it, `toggle_debug`) -- the overlay
            // is a debug HUD, not something every player should see on
            // launch, and `--bench`'s new `--overlay` flag (default off, for
            // the same reason) is what actually exercises this text draw
            // path off the GPU-bound-vs-submit-bound question it was added
            // for.
            show_debug: false,
            frame_ms: 0.0,
            // `0.0`, not a guessed distance: `Lighting::fog_range(0.0)` is
            // `(0.0, 0.0)`, i.e. fog off, which is the honest state before
            // the first `render()` call tells this how far streaming
            // actually reaches.
            render_radius_blocks: 0.0,
        };
        (renderer, mesh_assets)
    }

    /// The device and queue backing this renderer -- what a caller needs to
    /// mesh nodes against the same GPU context (e.g. resolving texture
    /// layers via the [`MeshAssets`] returned alongside this renderer by
    /// [`new`](Self::new)), without this crate needing to know why.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Apply a batch of streaming updates the caller has already decided on:
    /// drop `to_unload`'s geometry immediately, and queue `meshed`'s for
    /// upload (paced at [`MAX_UPLOADS_PER_FRAME`] per frame by
    /// [`drain_uploads`](Self::drain_uploads), called every [`render`](Self::render)).
    /// This is the entire streaming surface the renderer exposes -- *which*
    /// nodes to load/unload is the caller's decision (`ARCHITECTURE.md` §1);
    /// an edit is no different from ordinary streaming from here, just a
    /// single-node update.
    pub fn apply_node_updates(
        &mut self,
        to_unload: impl IntoIterator<Item = NodeId>,
        meshed: impl IntoIterator<Item = MeshedNode>,
    ) {
        for id in to_unload {
            self.desired.remove(&id);
            self.arena.remove(id);
        }
        for node in meshed {
            self.desired.insert(node.id);
            self.upload_queue.push_back(node);
        }
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// The surface's current size in pixels -- what screen-space layout needs.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.scene
                .resize(&self.device, self.config.width, self.config.height);
        }
    }

    /// Upload queued nodes from [`apply_node_updates`](Self::apply_node_updates) —
    /// at most [`MAX_UPLOADS_PER_FRAME`] per frame, so a caller handing over a
    /// whole ring's worth of newly-streamed nodes at once doesn't spike the
    /// frame time. Called every [`render`](Self::render); a node's old
    /// geometry (if any) stays drawn until its queued replacement's turn
    /// comes up.
    fn drain_uploads(&mut self) {
        puffin::profile_function!();
        let mut uploaded = 0;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let Some(node) = self.upload_queue.pop_front() else {
                break;
            };
            // Skip if unloaded while it waited in the queue.
            if !self.desired.contains(&node.id) {
                continue;
            }
            self.arena.remove(node.id); // free any prior slot first
            self.arena.insert(
                &self.queue,
                node.id,
                node.origin,
                node.scale,
                &node.mesh,
                node.aabb,
            );
            uploaded += 1;
        }
    }

    /// `hud` is plain data the caller reduces its state to -- this crate never
    /// learns what an item is, or what hurt the player (Rule 3). `render_radius_blocks`
    /// is how far the caller's own streaming actually reaches (`ARCHITECTURE.md`
    /// Rule 3 again: this crate has no schedule of its own to derive it from) --
    /// used only to fade distance fog out at that real edge.
    pub fn render(
        &mut self,
        camera: CameraPose,
        selected_block: Option<[i32; 3]>,
        cracking: Option<([i32; 3], f32)>,
        players: &[crate::figure::PlayerView],
        hud: crate::scene::Hud<'_>,
        render_radius_blocks: f32,
    ) {
        let crate::scene::Hud {
            hotbar,
            panel,
            health,
            crosshair,
        } = hud;
        crate::profiling::Profiler::new_frame();
        puffin::profile_function!();
        self.render_radius_blocks = render_radius_blocks;
        self.update(camera);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Surface lost/outdated/occluded/timed out (e.g. during resize) —
            // reconfigure and skip.
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Validation => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // CPU frustum-cull + upload the indirect draw list before the pass begins.
        let draw_count = self.arena.prepare(&self.queue, &self.frustum);
        self.visible_chunks = self.arena.visible_nodes() as usize;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            puffin::profile_scope!("encode-pass");
            let overlay = self.show_debug.then(|| self.debug_text(camera));
            self.scene.encode_scene(
                &self.device,
                &self.queue,
                &mut encoder,
                &view,
                SceneFrame {
                    arena: &self.arena,
                    draw_count,
                    selected_block,
                    cracking,
                    players,
                    overlay: overlay.as_deref(),
                    hotbar,
                    panel,
                    health,
                    crosshair,
                    // The window doesn't report a GPU/frame reading (only
                    // `--bench` does, `bench.rs`); the feature is requested
                    // here too (`gpu_driven_features`) only so it's available
                    // if that changes later.
                    gpu_timestamps: None,
                },
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);

        self.report_fps();
    }

    /// Give the HUD its item icons; see [`crate::SceneRenderer::set_icons`].
    pub fn set_icons(&mut self, icons: &[Option<Vec<u8>>]) {
        self.scene.set_icons(&self.device, &self.queue, icons);
    }

    /// Toggle the F3 debug overlay.
    pub fn toggle_debug(&mut self) {
        self.show_debug = !self.show_debug;
    }

    /// Build this frame's debug text. The overlay's drawing (including its drop
    /// shadow) belongs to the shared scene path, so this only produces the string.
    fn debug_text(&self, camera: CameraPose) -> String {
        let p = camera.eye;
        let d = camera.look_dir;
        let facing = if d.x.abs() > d.z.abs() {
            if d.x > 0.0 {
                "east (+x)"
            } else {
                "west (-x)"
            }
        } else if d.z > 0.0 {
            "south (+z)"
        } else {
            "north (-z)"
        };
        let fps = if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        };
        let c = ChunkCoord::from_world_pos(p.to_array());
        let (fog_start, fog_end) = Lighting::fog_range(self.render_radius_blocks);
        format!(
            "Cubara  (F3)\n\
             {fps:.0} fps  ({ms:.2} ms)\n\
             xyz  {x:.1} / {y:.1} / {z:.1}\n\
             chunk  {cx} {cy} {cz}\n\
             facing  {facing}\n\
             nodes  {vis} drawn / {res} resident\n\
             fog  {fog_start:.0}-{fog_end:.0}",
            ms = self.frame_ms,
            x = p.x,
            y = p.y,
            z = p.z,
            cx = c.x,
            cy = c.y,
            cz = c.z,
            vis = self.visible_chunks,
            res = self.arena.len(),
        )
    }

    /// Upload whatever's been queued since last frame and refresh the camera
    /// matrix + frustum. The camera's own motion doesn't happen here -- it's
    /// `cubara-sim`'s job (block 1.6); this just tracks frame time for the
    /// on-screen FPS reading and applies streaming the caller already decided
    /// on via [`apply_node_updates`](Self::apply_node_updates).
    fn update(&mut self, camera: CameraPose) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        // Exponentially-smoothed frame time for a steady on-screen FPS reading.
        let ms = dt * 1000.0;
        self.frame_ms = if self.frame_ms == 0.0 {
            ms
        } else {
            self.frame_ms * 0.9 + ms * 0.1
        };
        self.drain_uploads();

        let vp = camera.view_proj(self.scene.aspect());
        self.frustum = Frustum::from_view_proj(vp);
        // `self.render_radius_blocks` came in with this frame's `render()`
        // call -- `cubara-app`'s idea of how far streaming actually reaches
        // (Rule 3: this crate has no schedule to derive it from itself).
        let (fog_start, fog_end) = Lighting::fog_range(self.render_radius_blocks);
        self.scene.set_camera(
            &self.device,
            &self.queue,
            vp,
            camera.eye,
            Lighting {
                fog_start,
                fog_end,
                ..Default::default()
            },
        );
    }

    /// Report frames-per-second roughly once per second.
    fn report_fps(&mut self) {
        self.frames += 1;
        let elapsed = self.last_report.elapsed();
        if elapsed.as_secs_f32() >= 1.0 {
            let fps = self.frames as f32 / elapsed.as_secs_f32();
            log::info!(
                "{fps:.0} FPS | drawn {}/{} resident nodes",
                self.visible_chunks,
                self.arena.len()
            );
            self.frames = 0;
            self.last_report = Instant::now();
        }
    }
}

/// Grab + hide the cursor for first-person look, or release it. Best-effort:
/// `Locked` isn't supported on every platform, so fall back to `Confined`, and
/// never panic if the platform refuses.
pub fn grab_cursor(window: &Window, grab: bool) {
    if grab {
        if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
            let _ = window.set_cursor_grab(CursorGrabMode::Confined);
        }
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

pub fn camera_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("camera-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            // Vertex-only until `Lighting`/`FrameUniform`: `mesh.wgsl` and
            // `figure.wgsl`'s fragment stages now read the sun/ambient/fog
            // fields too. `outline.wgsl`'s fragment stage still doesn't
            // touch this group at all, which is fine -- a pipeline is never
            // required to use every stage a layout makes visible.
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// One world-space origin per resident chunk ("node"), read in the vertex
/// shader (`@group(1)` in `mesh.wgsl`) by the `node_index` baked into each
/// [`Vertex`] -- what turns a node-local packed vertex into a world position
/// without a CPU-side translate. [`ChunkArena`] owns the actual buffer/bind
/// group (it's tied to chunk residency); this layout is shared between that
/// and the pipeline so the two stay structurally compatible.
pub fn origins_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("node-origins-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// `@group(1)` in `outline.wgsl`: the highlighted voxel's world-space
/// origin. A separate, tiny bind group rather than folding into the camera
/// one (`@group(0)`, reused unchanged) -- it varies with the selection, the
/// camera uniform doesn't, and the outline pipeline has no use for the mesh
/// pipeline's origins/texture groups at all.
pub fn outline_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("outline-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

pub fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// [`mesh.wgsl`]'s `override` pipeline constants, derived from a [`Lighting`]
/// and the current viewport size. mesh.wgsl's fragment stage reads *none* of
/// `FrameUniform` -- every lighting/fog value, plus the (A, B) depth-recovery
/// pair and the viewport/FOV terms radial fog needs, are pipeline-compile-time
/// constants instead of per-frame uniform reads. That trade only makes sense
/// because it is exactly this: something that changes rarely (a lighting
/// update, a window resize), not something read every frame -- see the
/// occupancy-cliff research in `BENCHMARKS.md`'s package-4 footnote for the
/// measurements this is built on (touching this uniform buffer at all cost
/// the M3 ~400 FPS; going through overrides instead recovered nearly all of
/// it). A caller that changes `Lighting` calls [`crate::SceneRenderer::set_lighting`],
/// which rebuilds the pipeline with these in the background rather than every
/// frame.
pub fn mesh_pipeline_constants(
    lighting: &Lighting,
    width: u32,
    height: u32,
) -> Vec<(&'static str, f64)> {
    let (depth_a, depth_b) = reverse_z_depth_constants();
    vec![
        ("ambient_low", lighting.ambient_low as f64),
        ("ambient_high", lighting.ambient_high as f64),
        ("ao_floor", lighting.ao_floor as f64),
        ("diffuse_weight", lighting.diffuse_weight as f64),
        ("sun_dir_x", lighting.sun_dir.x as f64),
        ("sun_dir_y", lighting.sun_dir.y as f64),
        ("sun_dir_z", lighting.sun_dir.z as f64),
        ("sun_color_r", lighting.sun_color.x as f64),
        ("sun_color_g", lighting.sun_color.y as f64),
        ("sun_color_b", lighting.sun_color.z as f64),
        ("fog_color_r", lighting.fog_color.x as f64),
        ("fog_color_g", lighting.fog_color.y as f64),
        ("fog_color_b", lighting.fog_color.z as f64),
        ("fog_start", lighting.fog_start as f64),
        ("fog_end", lighting.fog_end as f64),
        ("depth_a", depth_a as f64),
        ("depth_b", depth_b as f64),
        ("viewport_width", width as f64),
        ("viewport_height", height as f64),
        ("aspect", width as f64 / height as f64),
    ]
}

/// `mesh.wgsl`'s shader module, parsed from WGSL once and reused for every
/// pipeline rebuild -- [`build_mesh_pipeline_from_module`] is the part that
/// actually changes when only the `override` constants change, so a rebuild
/// never needs this again. Split out from the old single `build_pipeline`
/// specifically so [`crate::SceneRenderer::set_lighting`]'s background
/// rebuilds reuse it, per the same measurement that showed re-parsing WGSL
/// on every rebuild cost ~10x more than it needed to.
pub fn build_mesh_shader(device: &wgpu::Device) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mesh-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh.wgsl").into()),
    })
}

/// `mesh.wgsl`'s pipeline layout -- fixed by the bind group layouts, not by
/// lighting, so this is built once and reused the same way the shader module
/// is.
pub fn build_mesh_layout(
    device: &wgpu::Device,
    camera_bgl: &wgpu::BindGroupLayout,
    origins_bgl: &wgpu::BindGroupLayout,
    textures_bgl: &wgpu::BindGroupLayout,
) -> wgpu::PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh-layout"),
        bind_group_layouts: &[Some(camera_bgl), Some(origins_bgl), Some(textures_bgl)],
        immediate_size: 0,
    })
}

/// The part of building the mesh pipeline that actually changes with
/// `constants`: specializing `shader`/`layout` (both fixed, built once) with
/// this particular set of `override` values. Called both for the initial
/// pipeline ([`build_pipeline`]) and for every background rebuild
/// ([`crate::SceneRenderer::set_lighting`]).
pub fn build_mesh_pipeline_from_module(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    constants: &[(&str, f64)],
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mesh-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(vertex_layout())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Faces are wound CCW/outward, so cull the back faces.
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            // Reversed-Z: nearer fragments have *greater* depth.
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Build the mesh shader, layout and an initial pipeline together -- what
/// [`crate::SceneRenderer::new`] wants once, at construction. Every rebuild
/// after that goes through [`build_mesh_pipeline_from_module`] directly,
/// reusing the shader/layout this returns.
pub fn build_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    origins_bgl: &wgpu::BindGroupLayout,
    textures_bgl: &wgpu::BindGroupLayout,
    constants: &[(&str, f64)],
) -> (
    wgpu::ShaderModule,
    wgpu::PipelineLayout,
    wgpu::RenderPipeline,
) {
    let shader = build_mesh_shader(device);
    let layout = build_mesh_layout(device, camera_bgl, origins_bgl, textures_bgl);
    let pipeline = build_mesh_pipeline_from_module(device, format, &shader, &layout, constants);
    (shader, layout, pipeline)
}

/// The selected-block outline's pipeline: a line list, sharing the mesh
/// pipeline's depth buffer and format so it's correctly occluded by terrain
/// in front of it, but with a small negative depth bias so it wins the
/// exact tie against the *targeted* block's own coplanar face instead of
/// z-fighting it (issue #52's Design decisions). Doesn't write depth --
/// nothing needs to be occluded by a wireframe -- and doesn't cull (lines
/// have no winding).
const FIGURE_VERTEX_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

/// One figure vertex: a world-space position and a colour.
pub const fn figure_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: (6 * std::mem::size_of::<f32>()) as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &FIGURE_VERTEX_ATTRS,
    }
}

/// The pipeline that draws other players.
///
/// Depth-tested and back-face culled like the terrain, so a figure behind a
/// hill is behind it. Camera bind group only: the vertices come in already
/// placed, so there is no per-figure uniform to bind or to keep in step.
/// The fastest presentation this surface actually offers.
///
/// `Mailbox` first (no tearing, no waiting), then `Immediate` (no waiting),
/// then `Fifo`, which every surface supports and which is vsync. Returning a
/// concrete mode rather than `AutoNoVsync` is the point: the caller can then log
/// what it got, and "we asked for no vsync" stops being mistakable for "we got
/// no vsync".
pub fn chosen_present_mode(caps: &wgpu::SurfaceCapabilities) -> wgpu::PresentMode {
    for wanted in [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate] {
        if caps.present_modes.contains(&wanted) {
            return wanted;
        }
    }
    wgpu::PresentMode::Fifo
}

pub fn build_figure_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("figure-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/figure.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("figure-layout"),
        bind_group_layouts: &[Some(camera_bgl)],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("figure-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(figure_vertex_layout())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            // Reversed-Z, like the terrain: greater is nearer.
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

pub fn build_outline_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    outline_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("outline-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/outline.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("outline-layout"),
        bind_group_layouts: &[Some(camera_bgl), Some(outline_bgl)],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("outline-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(outline_vertex_layout())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            // Reversed-Z counterpart of LessEqual -- the outline must draw
            // at exactly the depth of the face it outlines, not be rejected by it.
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: wgpu::StencilState::default(),
            // No bias: wgpu 29 rejects any depth bias on a non-triangle
            // topology (validation, `DepthBiasWithIncompatibleTopology`) --
            // this pipeline draws `LineList`. `GreaterEqual` above is what
            // actually makes the outline win the z-fight against the face
            // it outlines.
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_layout_matches_vertex() {
        // `Vertex` is plain data in cubara-voxel; its GPU layout lives here. Nothing
        // in the type system ties the two together, so pin it: a field added to
        // Vertex changes the stride and fails here, rather than silently making the
        // GPU read every vertex at the wrong offset.
        let layout = vertex_layout();
        assert_eq!(
            layout.array_stride,
            std::mem::size_of::<Vertex>() as wgpu::BufferAddress
        );
        assert_eq!(layout.array_stride, 12, "three packed u32 words");
        assert_eq!(layout.attributes.len(), 3, "packed0, packed1, packed2");

        // Offsets must land on the real field boundaries.
        let offsets: Vec<u64> = layout.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(offsets, vec![0, 4, 8]);
    }

    #[test]
    fn lighting_default_matches_mesh_wgsls_original_literals() {
        // `mesh.wgsl` hard-coded these before `Lighting`/`FrameUniform`
        // existed (`vec3<f32>(0.4, 1.0, 0.3)`, `mix(0.28, 0.42, ...)`,
        // `mix(0.4, 1.0, in.ao)`, `diffuse * 0.75`). Pinned here so nobody
        // can "clean up" a default and silently shift every terrain pixel.
        let l = Lighting::default();
        assert_eq!(l.sun_dir, glam::vec3(0.4, 1.0, 0.3).normalize());
        assert_eq!(l.sun_color, glam::Vec3::ONE);
        assert_eq!(l.diffuse_weight, 0.75);
        assert_eq!(l.ambient_low, 0.28);
        assert_eq!(l.ambient_high, 0.42);
        assert_eq!(l.ao_floor, 0.4);
        // `scene::CLEAR_COLOR` -- kept in step by this assertion rather than
        // by hoping two literals in two files never drift apart.
        assert_eq!(l.fog_color, glam::vec3(0.45, 0.62, 0.80));
        assert!(
            l.fog_end <= l.fog_start,
            "the default must have fog off, not merely far away"
        );
    }

    #[test]
    fn frame_uniform_packs_lighting_at_the_offsets_the_shader_expects() {
        let f = FrameUniform::new(
            glam::Mat4::IDENTITY,
            glam::Vec3::new(1.0, 2.0, 3.0),
            Lighting::default(),
        );
        assert_eq!(f.eye, [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(f.sun_color[3], 0.75, "diffuse weight lives in sun_color.w");
        assert_eq!(f.ambient, [0.28, 0.42, 0.4, 0.0]);
        assert_eq!(f.fog[0], 0.0, "fog_start");
        assert_eq!(f.fog[1], 0.0, "fog_end");
    }

    #[test]
    fn mesh_pipeline_constants_carry_lightings_values_and_the_viewport() {
        // Pinned so a future edit to either `Lighting::default` or this
        // function's key names can't silently drift from what `mesh.wgsl`'s
        // `override` declarations actually expect -- a typo'd key here would
        // just fall back to the shader's own default silently, not error.
        let lighting = Lighting {
            fog_start: 100.0,
            fog_end: 200.0,
            ..Lighting::default()
        };
        let c = mesh_pipeline_constants(&lighting, 1920, 1080);
        // `PipelineCompilationOptions::constants` is `&[(&str, f64)]`, not a
        // map -- a plain lookup rather than indexing keeps this test honest
        // about the same shape the real caller uses.
        let get = |key: &str| {
            c.iter()
                .find(|(k, _)| *k == key)
                .unwrap_or_else(|| panic!("no constant named {key}"))
                .1
        };
        assert_eq!(get("ambient_low"), lighting.ambient_low as f64);
        assert_eq!(get("ambient_high"), lighting.ambient_high as f64);
        assert_eq!(get("ao_floor"), lighting.ao_floor as f64);
        assert_eq!(get("diffuse_weight"), lighting.diffuse_weight as f64);
        assert_eq!(get("sun_dir_x"), lighting.sun_dir.x as f64);
        assert_eq!(get("sun_dir_y"), lighting.sun_dir.y as f64);
        assert_eq!(get("sun_dir_z"), lighting.sun_dir.z as f64);
        assert_eq!(get("sun_color_r"), lighting.sun_color.x as f64);
        assert_eq!(get("fog_color_b"), lighting.fog_color.z as f64);
        assert_eq!(get("fog_start"), 100.0);
        assert_eq!(get("fog_end"), 200.0);
        assert_eq!(get("viewport_width"), 1920.0);
        assert_eq!(get("viewport_height"), 1080.0);
        assert_eq!(get("aspect"), 1920.0 / 1080.0);
        let (depth_a, depth_b) = reverse_z_depth_constants();
        assert_eq!(get("depth_a"), depth_a as f64);
        assert_eq!(get("depth_b"), depth_b as f64);
    }

    #[test]
    fn reverse_z_depth_constants_invert_the_projection_at_several_depths() {
        // Pins `reverse_z_depth_constants`'s closed form against the actual
        // matrix `mesh.wgsl`'s fog now depends on, rather than trusting its
        // derivation comment unchecked -- if the projection ever changes
        // (a different FOV, a different NEAR_PLANE/FAR_PLANE), this fails
        // instead of the fog silently drifting.
        let (a, b) = reverse_z_depth_constants();
        let aspect = 16.0 / 9.0;
        let eye = glam::Vec3::ZERO;
        let look_dir = glam::vec3(0.0, 0.0, -1.0);
        let vp = CameraUniform::look_view_proj(aspect, eye, look_dir);
        for depth in [1.0f32, 10.0, 100.0, 960.0, 1999.0] {
            let world = eye + look_dir * depth;
            let clip = vp * glam::vec4(world.x, world.y, world.z, 1.0);
            let ndc_z = clip.z / clip.w;
            let recovered = a / (ndc_z + b);
            let relative_error = ((recovered - depth) / depth).abs();
            // 2e-3, not 1e-3: reversed-Z spends its precision near the *near*
            // plane by design (the type's own doc comment), so recovering
            // depth from `ndc_z` right at the far edge (1999 of 2000) is
            // dividing by a value close to `f32`'s noise floor -- this is
            // that tradeoff showing up, not an error in the closed form.
            assert!(
                relative_error < 2e-3,
                "depth {depth}: recovered {recovered} ({relative_error:e} relative error)"
            );
        }
    }

    #[test]
    fn fog_range_starts_before_it_ends_and_ends_inside_the_render_radius() {
        let (start, end) = Lighting::fog_range(1024.0);
        assert!(start < end, "smoothstep(start, end, ..) needs start < end");
        assert_eq!(end, 1024.0 - 64.0);
        assert_eq!(start, 0.6 * end);
    }

    #[test]
    fn fog_range_below_one_chunk_collapses_to_off_not_negative() {
        // A radius smaller than the `- 64.0` margin must not produce
        // `end < 0.0` (which would make `start > end`, inverting the
        // smoothstep and lighting up everything behind the camera instead
        // of nothing) -- it collapses to the same `(0.0, 0.0)` `Default`
        // already uses for "no fog".
        assert_eq!(Lighting::fog_range(0.0), (0.0, 0.0));
        assert_eq!(Lighting::fog_range(32.0), (0.0, 0.0));
    }
}
