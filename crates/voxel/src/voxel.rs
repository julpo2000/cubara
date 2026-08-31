//! Voxel data and greedy meshing.
//!
//! A single 16³ chunk. Meshing culls hidden faces (a face is only emitted when its
//! neighbour is empty) and then *greedily merges* adjacent coplanar faces into
//! large quads, so a flat surface becomes a handful of triangles instead of one
//! quad per block. Out-of-chunk neighbours count as empty, so the outer shell is
//! always meshed. Merging also requires the same block id, not just the same
//! solidity, so a stone/soil boundary never gets one quad wearing one texture.

use crate::block::BlockId;
use crate::mesh::{Face, Mesh, Vertex};
use crate::registry::BlockRegistry;
use crate::storage::{ChunkPayloadError, ChunkStorage};

/// What the mesher needs beyond a chunk's raw block ids: whether an id is
/// solid, and which texture-array layer a texture *name* occupies. Bundled
/// because the two always travel together and neither belongs to
/// `cubara-voxel` alone -- `layer_of` is supplied by `cubara-render`, which is
/// the only thing that knows what a texture-array layer is (`ARCHITECTURE.md`
/// Rule 4: this crate stays GPU-free, so it takes the mapping as a callback
/// rather than owning it). The mesher resolves a quad's `(BlockId, Face)` to a
/// texture *name* via `registry` (§3.2's `Sided` top/side/bottom, or `All`'s
/// one name), then `layer_of` turns that name into a GPU layer index -- face
/// selection is data the registry already owns, so it happens here rather
/// than in the shader (block 1.4b, `docs/PHASE1_ARCHITECTURE.md` §11).
#[derive(Clone, Copy)]
pub struct MeshContext<'a> {
    pub registry: &'a BlockRegistry,
    pub layer_of: &'a dyn Fn(&str) -> u32,
}

/// One cell of a greedy-mesh slice: the face orientation, its four corners'
/// ambient-occlusion levels, and which block it belongs to. Two cells only
/// merge into one quad when they are *equal* -- same orientation, same AO,
/// **and** same block -- so AO discontinuities and material boundaries both
/// correctly split the merge instead of blending into one wrong quad.
#[derive(Clone, Copy, PartialEq, Eq)]
struct MaskCell {
    /// 0 = no face here, +1 = faces toward +axis, -1 = toward -axis.
    sign: i8,
    /// Per-corner occlusion 0..=3 (3 = unoccluded/bright), in corner order c0..c3.
    ao: [u8; 4],
    /// The solid block this face belongs to (meaningless when `sign == 0`).
    block: BlockId,
}

const NO_CELL: MaskCell = MaskCell {
    sign: 0,
    ao: [0; 4],
    block: BlockId::AIR,
};

/// Standard voxel ambient occlusion for one face corner from its three neighbouring
/// voxels on the air side (the two edge voxels + the diagonal). Two solid edges fully
/// occlude the corner; otherwise each solid neighbour darkens it one step.
fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        0
    } else {
        3 - (side1 as u8 + side2 as u8 + corner as u8)
    }
}

/// A cubic chunk of `SIZE³` blocks, stored palette-compressed (see
/// [`ChunkStorage`]).
pub struct Chunk {
    storage: ChunkStorage,
}

impl Chunk {
    pub const SIZE: usize = 16;
    const VOLUME: usize = Self::SIZE * Self::SIZE * Self::SIZE;

    #[inline]
    fn index(x: usize, y: usize, z: usize) -> usize {
        (z * Self::SIZE + y) * Self::SIZE + x
    }

    /// The block at a local position.
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockId {
        self.storage.get(Self::index(x, y, z))
    }

    /// Set the block at a local position, promoting/re-packing storage as
    /// needed (see [`ChunkStorage`]).
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, id: BlockId) {
        self.storage.set(Self::index(x, y, z), id, Self::VOLUME);
    }

    /// Whether the block at a local position is solid, per `registry`. A
    /// block can be non-air and still not solid (e.g. a future transparent
    /// type), so this is a real registry lookup, not just "not air".
    #[inline]
    pub fn is_solid(&self, registry: &BlockRegistry, x: usize, y: usize, z: usize) -> bool {
        registry.is_solid(self.get(x, y, z))
    }

    /// The block id at a local position, with bounds handling: anything
    /// outside the chunk counts as air. The mesher's primitive -- unlike
    /// [`is_solid`](Self::is_solid) it doesn't collapse to a bool, since a
    /// greedy quad also needs to know *which* block it belongs to.
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockId {
        if x < 0 || y < 0 || z < 0 {
            return BlockId::AIR;
        }
        let (x, y, z) = (x as usize, y as usize, z as usize);
        if x >= Self::SIZE || y >= Self::SIZE || z >= Self::SIZE {
            return BlockId::AIR;
        }
        self.get(x, y, z)
    }

    /// Build a chunk by asking `f` for each local block's id.
    ///
    /// Samples every cell into a flat buffer first, then chooses `Uniform` or
    /// `Palette` from the complete set in one pass
    /// ([`ChunkStorage::from_ids`]), rather than promoting/re-packing
    /// incrementally through [`set`](Self::set) 4096 times. Both are correct;
    /// this one measurably matters, because `f` is worldgen (real terrain
    /// sampling, not free) and a simple sampling loop is what lets the
    /// compiler optimise it the way it already did for the old all-`bool`
    /// representation -- routing every cell through the promote/repack state
    /// machine instead cost an **order of magnitude** on real terrain (~70ms
    /// -> ~1.0s over a radius-64 region), not just the palette bookkeeping's
    /// own small cost. See `BENCHMARKS.md`'s block-1.2 row.
    pub fn from_fn(mut f: impl FnMut(usize, usize, usize) -> BlockId) -> Self {
        let mut ids = Vec::with_capacity(Self::VOLUME);
        for z in 0..Self::SIZE {
            for y in 0..Self::SIZE {
                for x in 0..Self::SIZE {
                    ids.push(f(x, y, z));
                }
            }
        }
        Self {
            storage: ChunkStorage::from_ids(&ids),
        }
    }

    /// Whether the chunk has no solid blocks (nothing to mesh).
    pub fn is_empty(&self) -> bool {
        match &self.storage {
            ChunkStorage::Uniform(id) => *id == BlockId::AIR,
            // A palette chunk can still end up all-air (e.g. place then break
            // the one block that triggered promotion) -- the palette table
            // doesn't demote, so only a full scan is honest here.
            ChunkStorage::Palette { .. } => {
                (0..Self::VOLUME).all(|idx| self.storage.get(idx) == BlockId::AIR)
            }
        }
    }

    /// Encode this chunk's contents exactly as
    /// `docs/PHASE1_ARCHITECTURE.md` §7.3 specifies -- the in-memory
    /// [`ChunkStorage`] representation, and nothing else (save/load, block
    /// 1.9). Errors are [`ChunkPayloadError`]; see
    /// [`ChunkStorage::write_payload`].
    pub fn write_payload(&self) -> Result<Vec<u8>, ChunkPayloadError> {
        let mut out = Vec::new();
        self.storage.write_payload(&mut out)?;
        Ok(out)
    }

    /// Decode a payload [`write_payload`](Self::write_payload) produced.
    pub fn read_payload(bytes: &[u8]) -> Result<Self, ChunkPayloadError> {
        Ok(Self {
            storage: ChunkStorage::read_payload(bytes, Self::VOLUME)?,
        })
    }

    /// Rebuild this chunk with every id passed through `remap`, without
    /// touching the packed voxel index data at all -- only a palette's (or a
    /// uniform chunk's single) id values ever need translating, since an
    /// index only ever points *into* the palette, never names an id
    /// directly. Used by save/load (block 1.9) to translate a loaded
    /// chunk's saved ids into this process's current runtime ids (§7.2's
    /// `saved_id → runtime_id` remap). `None` from `remap` for any id
    /// present fails the whole chunk -- a corrupt or foreign save file, not
    /// something to patch over with air.
    pub fn remap_ids(&self, remap: impl Fn(BlockId) -> Option<BlockId>) -> Option<Chunk> {
        let storage = match &self.storage {
            ChunkStorage::Uniform(id) => ChunkStorage::Uniform(remap(*id)?),
            ChunkStorage::Palette {
                palette,
                bits,
                data,
            } => {
                let mut new_palette = Vec::with_capacity(palette.len());
                for &id in palette {
                    new_palette.push(remap(id)?);
                }
                ChunkStorage::Palette {
                    palette: new_palette,
                    bits: *bits,
                    data: data.clone(),
                }
            }
        };
        Some(Chunk { storage })
    }

    /// Greedy-mesh the chunk at full resolution (LOD 0). Vertices are node-local
    /// (0..SIZE); placing the chunk in the world is a GPU-side per-node origin,
    /// not something done here.
    pub fn build_mesh(&self, ctx: &MeshContext) -> Mesh {
        greedy_mesh(Self::SIZE as i32, 1, |x, y, z| self.block_at(x, y, z), ctx)
    }

    /// Greedy-mesh a downsampled copy for distant LOD. `level` halves the resolution
    /// each step (0 = full 16³, 1 = 8³, 2 = 4³, …), capped so the grid stays ≥ 1³. A
    /// coarse cell is solid when at least half the fine cells it covers are solid
    /// (majority, ties solid), taking on the first solid fine cell's id as its
    /// representative material. Vertices still span 0..SIZE, so a coarse chunk
    /// keeps the same world footprint as the full one — just with far fewer
    /// triangles.
    pub fn build_mesh_lod(&self, ctx: &MeshContext, level: u32) -> Mesh {
        let level = level.min(Self::SIZE.trailing_zeros()); // log2(SIZE)
        if level == 0 {
            return self.build_mesh(ctx);
        }
        let factor = 1i32 << level;
        let n = Self::SIZE as i32 / factor;
        let coarse = self.downsample(ctx.registry, level);
        greedy_mesh(
            n,
            factor,
            |x, y, z| {
                if x < 0 || y < 0 || z < 0 || x >= n || y >= n || z >= n {
                    BlockId::AIR
                } else {
                    coarse[((z * n + y) * n + x) as usize]
                }
            },
            ctx,
        )
    }

    /// Majority-downsampled block grid at `level` (side `SIZE >> level`): each
    /// coarse cell is solid when ≥ half of the `factor³` fine cells it covers
    /// are, taking the first solid fine cell's id as its representative
    /// material. Phase 1 has one non-air id in the whole world, so "first" is
    /// exact; true majority tie-breaking across materials is untested until
    /// real multi-material terrain lands, and isn't needed before then.
    fn downsample(&self, registry: &BlockRegistry, level: u32) -> Vec<BlockId> {
        let factor = 1usize << level;
        let n = Self::SIZE / factor;
        let threshold = (factor * factor * factor).div_ceil(2); // ≥ half ⇒ solid
        let mut coarse = vec![BlockId::AIR; n * n * n];
        for cz in 0..n {
            for cy in 0..n {
                for cx in 0..n {
                    let mut solid_count = 0usize;
                    let mut representative = BlockId::AIR;
                    for dz in 0..factor {
                        for dy in 0..factor {
                            for dx in 0..factor {
                                let id =
                                    self.get(cx * factor + dx, cy * factor + dy, cz * factor + dz);
                                if registry.is_solid(id) {
                                    solid_count += 1;
                                    if representative == BlockId::AIR {
                                        representative = id;
                                    }
                                }
                            }
                        }
                    }
                    coarse[(cz * n + cy) * n + cx] = if solid_count >= threshold {
                        representative
                    } else {
                        BlockId::AIR
                    };
                }
            }
        }
        coarse
    }
}

/// Greedy mesher over any cubic grid. Sweeps slices along each axis, builds a
/// per-slice mask of visible faces (orientation + per-corner AO + block), and
/// merges equal cells into the largest quads. `block_at` answers the block id
/// in grid space (out of bounds = air); `scale` multiplies vertex positions (1
/// for full res, `factor` for a downsampled LOD), keeping the world footprint
/// constant. Positions stay integers throughout -- the packed [`Vertex`]
/// format has no room for fractional coordinates, and greedy-meshed corners
/// never need one.
fn greedy_mesh(
    n: i32,
    scale: i32,
    block_at: impl Fn(i32, i32, i32) -> BlockId,
    ctx: &MeshContext,
) -> Mesh {
    let mut mesh = Mesh::default();
    // `mask[v * n + u]`: the face at each slice cell.
    let mut mask = vec![NO_CELL; (n * n) as usize];
    // The same grid, captured before the merge loop consumes `mask`: does
    // this cell own a real face? A skirt must never cover a cell that does
    // -- see `push_skirt`.
    let mut has_own_face = vec![false; (n * n) as usize];

    for d in 0..3usize {
        let u = (d + 1) % 3;
        let v = (d + 2) % 3;
        let mut pos = [0i32; 3];
        let mut step = [0i32; 3];
        step[d] = 1;

        // Sweep the slice boundaries from -1 to n-1; the face plane is at pos[d]+1.
        pos[d] = -1;
        while pos[d] < n {
            // Build the mask for this slice boundary.
            let mut idx = 0usize;
            for vv in 0..n {
                pos[v] = vv;
                for uu in 0..n {
                    pos[u] = uu;
                    let a_id = block_at(pos[0], pos[1], pos[2]);
                    let b_id = block_at(pos[0] + step[0], pos[1] + step[1], pos[2] + step[2]);
                    let a = ctx.registry.is_solid(a_id);
                    let b = ctx.registry.is_solid(b_id);
                    let sign = if a == b {
                        0
                    } else if a {
                        1
                    } else {
                        -1
                    };
                    mask[idx] = if sign == 0 {
                        NO_CELL
                    } else {
                        // Occluders sit on the empty side of the face plane. pos[d]
                        // is still the pre-advance boundary here.
                        let air_d = if sign == 1 { pos[d] + 1 } else { pos[d] };
                        let block = if sign == 1 { a_id } else { b_id };
                        MaskCell {
                            sign,
                            ao: face_ao(&block_at, ctx.registry, d, u, v, air_d, uu, vv),
                            block,
                        }
                    };
                    idx += 1;
                }
            }

            pos[d] += 1; // advance to the face plane

            for (owns, cell) in has_own_face.iter_mut().zip(mask.iter()) {
                *owns = cell.sign != 0;
            }

            // Greedily merge the mask into quads.
            let mut j = 0i32;
            while j < n {
                let mut i = 0i32;
                while i < n {
                    let m = mask[(j * n + i) as usize];
                    if m.sign == 0 {
                        i += 1;
                        continue;
                    }

                    // Grow the quad width along u, then height along v — only over
                    // cells with an identical face (same orientation, AO *and* block).
                    let mut w = 1i32;
                    while i + w < n && mask[(j * n + i + w) as usize] == m {
                        w += 1;
                    }
                    let mut h = 1i32;
                    'height: while j + h < n {
                        for k in 0..w {
                            if mask[((j + h) * n + i + k) as usize] != m {
                                break 'height;
                            }
                        }
                        h += 1;
                    }

                    push_quad(
                        &mut mesh, d, u, v, pos[d], i, j, w, h, m.sign, m.ao, m.block, ctx, scale,
                    );
                    push_skirt(
                        &mut mesh,
                        d,
                        u,
                        v,
                        pos[d],
                        i,
                        j,
                        w,
                        h,
                        n,
                        m,
                        ctx,
                        scale,
                        &has_own_face,
                    );

                    // Consume the merged cells.
                    for l in 0..h {
                        for k in 0..w {
                            mask[((j + l) * n + i + k) as usize] = NO_CELL;
                        }
                    }
                    i += w;
                }
                j += 1;
            }
        }
    }
    mesh
}

/// The four corners' AO for the face cell at (`uu`,`vv`) whose empty side is the
/// `air_d` layer along axis `d`. Corner order matches `push_quad`'s c0..c3:
/// (0,0), (1,0), (1,1), (0,1) in the (`u`,`v`) axes.
#[allow(clippy::too_many_arguments)]
fn face_ao(
    block_at: &impl Fn(i32, i32, i32) -> BlockId,
    registry: &BlockRegistry,
    d: usize,
    u: usize,
    v: usize,
    air_d: i32,
    uu: i32,
    vv: i32,
) -> [u8; 4] {
    let occluded = |du: i32, dv: i32| -> bool {
        let mut p = [0i32; 3];
        p[d] = air_d;
        p[u] = uu + du;
        p[v] = vv + dv;
        registry.is_solid(block_at(p[0], p[1], p[2]))
    };
    let mut ao = [3u8; 4];
    for (k, (cu, cv)) in [(0, 0), (1, 0), (1, 1), (0, 1)].iter().enumerate() {
        let su = if *cu == 1 { 1 } else { -1 };
        let sv = if *cv == 1 { 1 } else { -1 };
        ao[k] = vertex_ao(occluded(su, 0), occluded(0, sv), occluded(su, sv));
    }
    ao
}

/// Emit one merged quad on the plane `plane` along axis `d`, spanning `w`×`h` cells
/// in the (`u`,`v`) axes starting at (`i`,`j`), with orientation `sign` (±1), per-corner
/// AO `ao` (levels 0..=3, corner order c0..c3), and the `block` the quad belongs to
/// (resolved to a texture layer via `ctx`). `scale` multiplies grid coordinates into
/// local space (>1 for downsampled LOD meshes) -- always an integer, so the result is
/// always an exact lattice coordinate for the packed [`Vertex`] format.
#[allow(clippy::too_many_arguments)]
fn push_quad(
    mesh: &mut Mesh,
    d: usize,
    u: usize,
    v: usize,
    plane: i32,
    i: i32,
    j: i32,
    w: i32,
    h: i32,
    sign: i8,
    ao: [u8; 4],
    block: BlockId,
    ctx: &MeshContext,
    scale: i32,
) {
    let mut base = [0i32; 3];
    base[d] = plane;
    base[u] = i;
    base[v] = j;
    let mut du = [0i32; 3];
    du[u] = w;
    let mut dv = [0i32; 3];
    dv[v] = h;

    let face = Face::from_axis_sign(d, sign);
    let tex_layer = ctx
        .registry
        .texture_for_face(block, face)
        .map(|name| (ctx.layer_of)(name))
        .unwrap_or(0);

    let c0 = base;
    let c1 = [base[0] + du[0], base[1] + du[1], base[2] + du[2]];
    let c2 = [
        base[0] + du[0] + dv[0],
        base[1] + du[1] + dv[1],
        base[2] + du[2] + dv[2],
    ];
    let c3 = [base[0] + dv[0], base[1] + dv[1], base[2] + dv[2]];

    // Each corner's own texel-tile coordinate: (0,0) at the origin corner,
    // stepping to (w,h) at the far corner -- one tile per grid cell, tiling
    // the texture across the merged quad.
    let uv0 = (0, 0);
    let uv1 = (w, 0);
    let uv2 = (w, h);
    let uv3 = (0, h);

    // (du × dv) points toward +d, so keep that order for +d faces and reverse for -d
    // faces to stay outward/CCW for back-face culling. AO and UV follow the same reorder.
    let (verts, uvs, vao) = if sign == 1 {
        (
            [c0, c1, c2, c3],
            [uv0, uv1, uv2, uv3],
            [ao[0], ao[1], ao[2], ao[3]],
        )
    } else {
        (
            [c0, c3, c2, c1],
            [uv0, uv3, uv2, uv1],
            [ao[0], ao[3], ao[2], ao[1]],
        )
    };

    let start = mesh.vertices.len() as u32;
    for ((c, (cu, cv)), a) in verts.iter().zip(uvs).zip(vao) {
        mesh.vertices.push(Vertex::new(
            (c[0] * scale) as u32,
            (c[1] * scale) as u32,
            (c[2] * scale) as u32,
            a as u32,
            face,
            tex_layer,
            cu as u32,
            cv as u32,
        ));
    }

    // Pick the diagonal that connects the two most-similar corners, so the darkened
    // corner doesn't bleed across the quad (the classic AO "flip" fix). Without it,
    // opposite-corner occlusion interpolates as an ugly gradient seam.
    if vao[0] as u16 + vao[2] as u16 > vao[1] as u16 + vao[3] as u16 {
        mesh.indices.extend_from_slice(&[
            start + 1,
            start + 2,
            start + 3,
            start + 1,
            start + 3,
            start,
        ]);
    } else {
        mesh.indices
            .extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    }
}

/// §6.4 -- hide LOD seams with skirts, not stitching. Where a node meets a
/// neighbour at a different level the two don't sample the same lattice, so
/// a crack of background can show through at the boundary. Stitching (real
/// transition geometry matched to the neighbour's resolution) would need to
/// know the neighbour's level at mesh time, serialising a pipeline that's
/// deliberately parallel; a skirt instead just extends this node's own
/// border wall one more lattice cell downward, purely from data this node
/// already has. Applied to every horizontal border wall regardless of
/// level -- meshing has no way to know whether a given edge actually meets
/// a different level or a same-level neighbour (whose lattice already lines
/// up exactly, so its skirt is simply never visible) without a cross-node
/// lookup, which is exactly what this must not do.
///
/// `(w, h, sign, ao, block)` are `m`'s merged-quad shape and appearance,
/// already used for the real quad above; a skirt is that same wall, minus
/// its top cell of height/width, shifted down by one. Only a `d == 0` (X)
/// or `d == 2` (Z) wall sitting exactly on the node's own edge (`plane` is
/// `0` or `n`) qualifies -- `d == 1` (a top/bottom face) has no "downward"
/// that would hide a horizontal-boundary crack. No-op where the wall's own
/// bottom is already at the lattice floor (`i`/`j` == 0): nothing there to
/// extend into without a negative, unrepresentable coordinate, and a wall
/// reaching the floor has no gap to hide anyway.
///
/// **And no-op over any cell that already owns a face** (`has_own_face`,
/// the mask snapshotted before the merge loop consumed it). "Base above the
/// lattice floor" is not the same question as "there is a crack here". A
/// node's own border plane treats the outside as air, so *every* solid cell
/// on it emits a face; a material change (dirt over stone) splits the greedy
/// mask there, leaving the upper quad's base above the floor with the lower
/// material's own real face directly beneath it. Skirting into that cell
/// puts two coplanar, same-facing quads over it -- a z-fight that shows
/// in-game as two textures flickering through each other along a node
/// boundary. The cells below a quad are checked individually and the skirt
/// is emitted only over the contiguous runs that are genuinely uncovered,
/// so a wall that is part-exposed still gets a skirt where it needs one.
#[allow(clippy::too_many_arguments)]
fn push_skirt(
    mesh: &mut Mesh,
    d: usize,
    u: usize,
    v: usize,
    plane: i32,
    i: i32,
    j: i32,
    w: i32,
    h: i32,
    n: i32,
    m: MaskCell,
    ctx: &MeshContext,
    scale: i32,
    has_own_face: &[bool],
) {
    if d == 1 || (plane != 0 && plane != n) {
        return;
    }

    // The strip of cells one step below the quad, in mask coordinates, and
    // the length of that strip. `d == 0` walls extend down the u axis (a
    // 1-by-run quad); `d == 2` walls extend down the v axis (run-by-1).
    if (d == 0 && i == 0) || (d != 0 && j == 0) {
        return;
    }
    let span = if d == 0 { h } else { w };
    let cell = |k: i32| -> usize {
        if d == 0 {
            ((j + k) * n + (i - 1)) as usize
        } else {
            ((j - 1) * n + (i + k)) as usize
        }
    };

    // Emit only over contiguous runs of cells that own no face of their own.
    let mut k = 0i32;
    while k < span {
        if has_own_face[cell(k)] {
            k += 1;
            continue;
        }
        let start = k;
        while k < span && !has_own_face[cell(k)] {
            k += 1;
        }
        let run = k - start;
        let (qi, qj, qw, qh) = if d == 0 {
            (i - 1, j + start, 1, run)
        } else {
            (i + start, j - 1, run, 1)
        };
        push_quad(
            mesh, d, u, v, plane, qi, qj, qw, qh, m.sign, m.ao, m.block, ctx, scale,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{DropRule, Faces, Material, Shape};

    fn empty() -> Chunk {
        Chunk {
            storage: ChunkStorage::Uniform(BlockId::AIR),
        }
    }

    fn uniform(id: BlockId) -> Chunk {
        Chunk {
            storage: ChunkStorage::Uniform(id),
        }
    }

    fn set(chunk: &mut Chunk, x: usize, y: usize, z: usize) {
        chunk.set(x, y, z, BlockId::STONE);
    }

    /// A registry with a single solid material -- air (0) plus one other
    /// material sorts to id 1, matching `BlockId::STONE`. Meshing tests only
    /// care about solid-vs-air, not registry mechanics (that's
    /// `registry::tests`), so this is the minimal fixture that makes
    /// `BlockId::STONE` solid.
    fn registry() -> BlockRegistry {
        BlockRegistry::from_materials(vec![(
            std::path::PathBuf::from("test-fixture.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
            },
        )])
        .expect("fixture registry is valid")
    }

    /// Every texture name maps to layer 0 -- these tests are about geometry,
    /// not texturing (that's `mesh::tests` and the render crate).
    fn zero_layer(_: &str) -> u32 {
        0
    }

    fn ctx(registry: &BlockRegistry) -> MeshContext<'_> {
        MeshContext {
            registry,
            layer_of: &zero_layer,
        }
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    fn position(v: Vertex) -> [f32; 3] {
        [v.x() as f32, v.y() as f32, v.z() as f32]
    }

    #[test]
    fn single_block_makes_six_quads() {
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        assert_eq!(mesh.vertices.len(), 24, "6 quads * 4 vertices");
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn full_chunk_merges_each_face_into_one_quad() {
        let chunk = uniform(BlockId::STONE);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        // Only the 6 outer faces are visible, each merged into a single quad.
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn full_chunk_gets_no_skirts_since_every_border_wall_already_reaches_the_floor() {
        // A skirt only adds anything when a border wall's own base sits above
        // the lattice floor (see the doc comment on `push_skirt`) -- a fully
        // solid chunk's four side walls all start at y=0, so this must stay
        // exactly the 6-quad/12-triangle shell above, not 6 quads plus skirts.
        let chunk = uniform(BlockId::STONE);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        assert_eq!(mesh.triangle_count(), 12, "no skirt geometry added");
    }

    #[test]
    fn skirt_extends_a_border_wall_down_by_one_cell_above_the_floor() {
        // A synthetic height difference at the node's own x=0 edge: solid
        // only from y=8 up, as if this node's sampled terrain doesn't reach
        // all the way down to the lattice floor there -- exactly the shape a
        // coarser or finer neighbour's differently-sampled edge might not
        // line up with, which a skirt papers over (§6.4).
        let chunk = Chunk::from_fn(|x, y, _| {
            if x == 0 && y >= 8 {
                BlockId::STONE
            } else {
                BlockId::AIR
            }
        });
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));

        // The border wall itself (NegX: outward-facing where x=0 is solid and
        // x=-1 is treated as air) spans y in [8, 16). The skirt is the extra
        // one-cell-tall quad directly below it, y in [7, 8).
        let has_skirt = mesh.vertices.as_chunks::<4>().0.iter().any(|quad| {
            quad.iter().all(|v| v.face() == Face::NegX)
                && quad.iter().map(|v| v.y()).min() == Some(7)
                && quad.iter().map(|v| v.y()).max() == Some(8)
        });
        assert!(has_skirt, "expected a one-cell skirt quad at y in [7, 8)");
    }

    /// A registry with two distinct materials, so a stacked pair of blocks
    /// splits the greedy mask instead of merging into one quad -- which is
    /// what real terrain (dirt over stone) does at every node edge.
    fn two_material_registry() -> BlockRegistry {
        let mat = |name: &str, tex: &str| {
            (
                std::path::PathBuf::from("test-fixture.ron"),
                Material {
                    name: name.to_string(),
                    solid: true,
                    faces: Faces::All(tex.to_string()),
                    shapes: vec![Shape::Full],
                    drops: DropRule::SameName,
                    requires_tier: 0,
                    hardness: Some(1),
                },
            )
        };
        BlockRegistry::from_materials(vec![
            mat("cubara:stone", "stone"),
            mat("cubara:dirt", "dirt"),
        ])
        .expect("fixture registry is valid")
    }

    fn layer_per_texture(name: &str) -> u32 {
        match name {
            "stone" => 0,
            "dirt" => 1,
            other => panic!("unexpected texture {other}"),
        }
    }

    /// Every (face, cell) a quad covers, with the texture layer that covers
    /// it. Two entries under one key means two coplanar, same-facing quads
    /// overlap -- which is exactly what z-fights on the GPU.
    fn coverage(mesh: &Mesh) -> std::collections::HashMap<(Face, u32, u32, u32), Vec<u32>> {
        let mut covered: std::collections::HashMap<(Face, u32, u32, u32), Vec<u32>> =
            std::collections::HashMap::new();
        for quad in mesh.vertices.as_chunks::<4>().0 {
            let face = quad[0].face();
            let layer = quad[0].tex_layer();
            let (xs, ys, zs) = (
                quad.iter().map(|v| v.x()),
                quad.iter().map(|v| v.y()),
                quad.iter().map(|v| v.z()),
            );
            let (x0, x1) = (xs.clone().min().unwrap(), xs.max().unwrap());
            let (y0, y1) = (ys.clone().min().unwrap(), ys.max().unwrap());
            let (z0, z1) = (zs.clone().min().unwrap(), zs.max().unwrap());
            // A quad is flat in one axis; the cells it covers span the other
            // two. `max` is the far corner, so the cell range is `min..max`.
            for x in x0..x1.max(x0 + 1) {
                for y in y0..y1.max(y0 + 1) {
                    for z in z0..z1.max(z0 + 1) {
                        covered.entry((face, x, y, z)).or_default().push(layer);
                    }
                }
            }
        }
        covered
    }

    #[test]
    fn a_skirt_never_covers_a_cell_that_already_has_its_own_face() {
        // Real terrain at a node edge is not solid-over-air; it is one
        // material stacked on another, and *both* are exposed, because a
        // node's own border plane treats the outside as air and so emits a
        // face for every solid cell on it.
        //
        // The greedy mask splits at the material change, so the upper quad's
        // base sits above the lattice floor -- which is precisely the
        // condition `push_skirt` reads as "this wall needs a skirt". It then
        // extends the upper material one cell down, over a cell the lower
        // material already covers with a real face of its own. Two coplanar,
        // same-facing quads with different textures is a z-fight, and it is
        // what shows up in-game as dirt and stone flickering through each
        // other along a node boundary.
        let chunk = Chunk::from_fn(|x, y, _| {
            if x != 0 {
                BlockId::AIR
            } else if y >= 8 {
                BlockId(2) // dirt
            } else {
                BlockId(1) // stone
            }
        });
        let registry = two_material_registry();
        let ctx = MeshContext {
            registry: &registry,
            layer_of: &layer_per_texture,
        };
        let mesh = chunk.build_mesh(&ctx);

        let overlaps: Vec<_> = coverage(&mesh)
            .into_iter()
            .filter(|(_, layers)| layers.len() > 1)
            .collect();
        assert!(
            overlaps.is_empty(),
            "{} cells are covered by more than one coplanar quad, e.g. {:?}",
            overlaps.len(),
            overlaps.first().unwrap()
        );
    }

    #[test]
    fn no_skirt_on_an_interior_wall_away_from_the_node_edge() {
        // A block in the middle of the chunk has no face anywhere near a
        // node-boundary plane (x/z = 0 or SIZE), so none of its faces
        // qualify as a border wall -- the same fixture
        // `single_block_makes_six_quads` uses, re-asserted here to make the
        // "no skirts on interior geometry" property explicit rather than
        // just inherited from an unrelated test staying green.
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        assert_eq!(mesh.triangle_count(), 12, "6 faces, no skirts");
    }

    #[test]
    fn vertex_ao_levels() {
        assert_eq!(
            vertex_ao(false, false, false),
            3,
            "no occluders → brightest"
        );
        assert_eq!(
            vertex_ao(true, false, false),
            2,
            "one side occludes one step"
        );
        assert_eq!(
            vertex_ao(false, true, true),
            1,
            "side + corner occlude two steps"
        );
        assert_eq!(
            vertex_ao(true, true, false),
            0,
            "two solid sides fully occlude"
        );
    }

    #[test]
    fn lone_block_is_fully_lit() {
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        assert!(
            mesh.vertices.iter().all(|v| v.ao() == 3),
            "a block with no neighbours has no occluded corners"
        );
    }

    #[test]
    fn neighbour_darkens_a_corner() {
        // Two diagonally-touching blocks: each occludes a corner of the other's faces.
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        set(&mut chunk, 6, 6, 5);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        assert!(
            mesh.vertices.iter().any(|v| v.ao() < 3),
            "a diagonal neighbour should occlude at least one corner"
        );
    }

    /// World-space bounds over a mesh's vertices.
    fn bounds(mesh: &Mesh) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for &v in &mesh.vertices {
            let p = position(v);
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        (min, max)
    }

    #[test]
    fn lod_keeps_footprint_and_merges_full_chunk() {
        // A full chunk downsamples to a full coarse chunk: still six merged faces,
        // and it must occupy the same 0..16 world footprint (scale compensates).
        let chunk = uniform(BlockId::STONE);
        let registry = registry();
        let ctx = ctx(&registry);
        for level in 1..=4 {
            let mesh = chunk.build_mesh_lod(&ctx, level);
            assert_eq!(
                mesh.triangle_count(),
                12,
                "level {level}: one quad per face"
            );
            assert_eq!(
                bounds(&mesh),
                ([0.0; 3], [16.0; 3]),
                "level {level} footprint"
            );
        }
    }

    #[test]
    fn lod_downsamples_a_slab_cleanly() {
        // Bottom half solid (fine y < 8) → at LOD 1 the coarse cells for y 0..4 are
        // fully solid and 4..8 empty, so it's a clean slab whose top sits at world y=8.
        let chunk = Chunk::from_fn(|_, y, _| if y < 8 { BlockId::STONE } else { BlockId::AIR });
        let registry = registry();
        let mesh = chunk.build_mesh_lod(&ctx(&registry), 1);
        assert_eq!(mesh.triangle_count(), 12, "slab = six merged faces");
        let (min, max) = bounds(&mesh);
        assert_eq!((min[1], max[1]), (0.0, 8.0), "slab spans world y 0..8");
    }

    #[test]
    fn lod_reduces_triangles_on_stepped_terrain() {
        // A diagonal staircase has many stair-step faces at full res; downsampling
        // merges the steps into coarser ones, so LOD 1 has strictly fewer triangles.
        let chunk = Chunk::from_fn(|x, y, _| {
            if (y as i32) <= x as i32 {
                BlockId::STONE
            } else {
                BlockId::AIR
            }
        });
        let registry = registry();
        let ctx = ctx(&registry);
        let full = chunk.build_mesh(&ctx).triangle_count();
        let lod1 = chunk.build_mesh_lod(&ctx, 1).triangle_count();
        assert!(
            lod1 < full,
            "LOD 1 ({lod1}) should have fewer tris than LOD 0 ({full})"
        );
    }

    #[test]
    fn lod_level_is_capped_not_panicking() {
        // An absurd level clamps to log2(SIZE) rather than producing an empty grid.
        let chunk = uniform(BlockId::STONE);
        let registry = registry();
        assert_eq!(
            chunk.build_mesh_lod(&ctx(&registry), 99).triangle_count(),
            12
        );
    }

    #[test]
    fn quads_are_wound_outward() {
        // Back-face culling relies on every quad being wound CCW/outward, i.e. the
        // geometric normal must agree with the stored (looked-up) normal.
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let registry = registry();
        let mesh = chunk.build_mesh(&ctx(&registry));
        for quad in 0..mesh.vertices.len() / 4 {
            let v0 = position(mesh.vertices[quad * 4]);
            let v1 = position(mesh.vertices[quad * 4 + 1]);
            let v2 = position(mesh.vertices[quad * 4 + 2]);
            let geo = cross(sub(v1, v0), sub(v2, v0));
            let n = mesh.vertices[quad * 4].face().normal();
            let dot = geo[0] * n[0] + geo[1] * n[1] + geo[2] * n[2];
            assert!(dot > 0.0, "quad {quad} is wound inward (geo·n = {dot})");
        }
    }

    #[test]
    fn distinct_blocks_never_share_a_merged_quad() {
        // Two adjacent solid materials must not be merged into one quad, even
        // though neither emits a face at their shared boundary (both solid) --
        // otherwise a stone/soil border would render as a single quad wearing
        // one texture. A dedicated registry with two distinct solid materials,
        // since the shared `registry()` fixture only has one.
        let two_materials = BlockRegistry::from_materials(vec![
            (
                std::path::PathBuf::from("a.ron"),
                Material {
                    name: "cubara:stone".to_string(),
                    solid: true,
                    faces: Faces::All("stone".to_string()),
                    shapes: vec![Shape::Full],
                    drops: DropRule::SameName,
                    requires_tier: 0,
                    hardness: Some(1),
                },
            ),
            (
                std::path::PathBuf::from("b.ron"),
                Material {
                    name: "cubara:soil".to_string(),
                    solid: true,
                    faces: Faces::All("soil".to_string()),
                    shapes: vec![Shape::Full],
                    drops: DropRule::SameName,
                    requires_tier: 0,
                    hardness: Some(1),
                },
            ),
        ])
        .unwrap();
        let stone = two_materials.id_of("cubara:stone").unwrap();
        let soil = two_materials.id_of("cubara:soil").unwrap();

        // A slab of stone next to a slab of soil, sharing an internal x=8
        // boundary; both solid, so that boundary emits no face, but the two
        // slabs' *outer* faces must stay as separate quads (different layers).
        let chunk = Chunk::from_fn(|x, _, _| if x < 8 { stone } else { soil });
        let layer_of = |name: &str| if name == "stone" { 0 } else { 1 };
        let mesh_ctx = MeshContext {
            registry: &two_materials,
            layer_of: &layer_of,
        };
        let mesh = chunk.build_mesh(&mesh_ctx);

        let layers: std::collections::HashSet<u32> =
            mesh.vertices.iter().map(|v| v.tex_layer()).collect();
        assert_eq!(
            layers,
            std::collections::HashSet::from([0, 1]),
            "both materials' textures must appear, not just one"
        );
    }

    #[test]
    fn sided_material_gives_each_face_its_own_layer() {
        // Block 1.4b: the mesher, not the shader, resolves face direction to
        // a texture layer -- a single isolated `Sided` block must come out of
        // meshing with its top/bottom/side quads wearing three different
        // layers, driven entirely by `registry.texture_for_face`.
        let registry = BlockRegistry::from_materials(vec![(
            std::path::PathBuf::from("grass.ron"),
            Material {
                name: "cubara:grass".to_string(),
                solid: true,
                faces: Faces::Sided {
                    top: "grass_top".to_string(),
                    side: "grass_side".to_string(),
                    bottom: "soil".to_string(),
                },
                shapes: vec![Shape::Full],
                drops: DropRule::SameName,
                requires_tier: 0,
                hardness: Some(1),
            },
        )])
        .unwrap();
        let grass = registry.id_of("cubara:grass").unwrap();

        // A single interior block: all 6 faces visible, none merged.
        let chunk = Chunk::from_fn(|x, y, z| {
            if (x, y, z) == (5, 5, 5) {
                grass
            } else {
                BlockId::AIR
            }
        });
        let layer_of = |name: &str| match name {
            "grass_top" => 10,
            "grass_side" => 20,
            "soil" => 30,
            other => panic!("unexpected texture name {other}"),
        };
        let mesh_ctx = MeshContext {
            registry: &registry,
            layer_of: &layer_of,
        };
        let mesh = chunk.build_mesh(&mesh_ctx);

        let layer_by_face: std::collections::HashMap<Face, u32> = mesh
            .vertices
            .iter()
            .map(|v| (v.face(), v.tex_layer()))
            .collect();
        assert_eq!(layer_by_face.get(&Face::PosY), Some(&10), "top");
        assert_eq!(layer_by_face.get(&Face::NegY), Some(&30), "bottom");
        for face in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert_eq!(layer_by_face.get(&face), Some(&20), "side face {face:?}");
        }
    }

    #[test]
    fn from_fn_of_a_constant_id_stays_uniform() {
        // The real-world path (`World::chunk_at` builds every chunk through
        // `from_fn`) must hit the no-allocation fast path for the common case --
        // an entirely-air or entirely-solid chunk -- not just when constructed
        // directly as `ChunkStorage::Uniform`.
        let chunk = Chunk::from_fn(|_, _, _| BlockId::AIR);
        assert!(matches!(chunk.storage, ChunkStorage::Uniform(BlockId::AIR)));

        let chunk = Chunk::from_fn(|_, _, _| BlockId::STONE);
        assert!(matches!(
            chunk.storage,
            ChunkStorage::Uniform(BlockId::STONE)
        ));
    }

    #[test]
    fn from_fn_of_varying_ids_promotes_and_round_trips() {
        let chunk = Chunk::from_fn(|x, y, z| {
            if x == 3 && y == 4 && z == 5 {
                BlockId::STONE
            } else {
                BlockId::AIR
            }
        });
        assert!(matches!(chunk.storage, ChunkStorage::Palette { .. }));
        assert_eq!(chunk.get(3, 4, 5), BlockId::STONE);
        assert_eq!(chunk.get(0, 0, 0), BlockId::AIR);
        let registry = registry();
        assert!(chunk.is_solid(&registry, 3, 4, 5));
        assert!(!chunk.is_solid(&registry, 0, 0, 0));
    }

    #[test]
    fn chunk_payload_round_trips_uniform_and_palette() {
        let uniform = Chunk::from_fn(|_, _, _| BlockId::STONE);
        let bytes = uniform.write_payload().unwrap();
        let back = Chunk::read_payload(&bytes).unwrap();
        for x in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for z in 0..Chunk::SIZE {
                    assert_eq!(back.get(x, y, z), uniform.get(x, y, z));
                }
            }
        }

        let palette = Chunk::from_fn(|x, y, z| BlockId(((x + y * 2 + z * 3) % 5) as u16 + 1));
        let bytes = palette.write_payload().unwrap();
        let back = Chunk::read_payload(&bytes).unwrap();
        for x in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for z in 0..Chunk::SIZE {
                    assert_eq!(back.get(x, y, z), palette.get(x, y, z), "x={x} y={y} z={z}");
                }
            }
        }
    }

    #[test]
    fn remap_ids_translates_every_id_without_touching_indices() {
        let chunk = Chunk::from_fn(|x, y, z| BlockId(((x + y + z) % 3) as u16 + 1));
        let remapped = chunk
            .remap_ids(|id| Some(BlockId(id.0 + 100)))
            .expect("every id present has a mapping");
        for x in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for z in 0..Chunk::SIZE {
                    assert_eq!(remapped.get(x, y, z).0, chunk.get(x, y, z).0 + 100);
                }
            }
        }
    }

    #[test]
    fn remap_ids_fails_the_whole_chunk_on_one_unmapped_id() {
        let chunk = Chunk::from_fn(|x, y, z| BlockId(((x + y + z) % 3) as u16 + 1));
        let remapped = chunk.remap_ids(|id| if id.0 == 2 { None } else { Some(id) });
        assert!(remapped.is_none());
    }

    #[test]
    fn is_empty_is_true_for_a_palette_chunk_with_no_solid_cells_left() {
        // No demotion: placing then breaking the one block that promoted the
        // chunk leaves it Palette-backed but logically empty. `is_empty` must
        // still report that correctly (it can't rely on the variant alone).
        let mut chunk = empty();
        set(&mut chunk, 1, 1, 1);
        assert!(matches!(chunk.storage, ChunkStorage::Palette { .. }));
        assert!(!chunk.is_empty());

        chunk.set(1, 1, 1, BlockId::AIR);
        assert!(matches!(chunk.storage, ChunkStorage::Palette { .. }));
        assert!(
            chunk.is_empty(),
            "every cell is air again, just not Uniform"
        );
    }
}
