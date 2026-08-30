//! Region files: save/load's on-disk chunk storage
//! (`docs/PHASE1_ARCHITECTURE.md` §7.1/§7.3, issue #60).
//!
//! A region is a 32×32×32 block of chunks -- cubic, because chunks are (a
//! column-shaped region would reintroduce the vertical special case cubic
//! chunks exist to remove). Only *dirty* chunks are ever written (§7.4);
//! everything else regenerates from the seed. A region file is a sorted
//! directory followed by payloads:
//!
//! ```text
//! "CBRG" | u16 format_version | u32 entry_count
//! [ u16 local_index | u32 offset | u32 length ] × entry_count   -- sorted by index
//! payloads, in that same order
//! ```
//!
//! Sorted, and written in directory order, so the same world state produces
//! byte-identical bytes -- what makes the round-trip test a hash comparison
//! rather than a semantic diff. All integers little-endian, so a world saved
//! on Windows loads on macOS. `offset` is measured from the start of the
//! file (not the start of the payload section), so a payload's bytes are
//! always `&file[offset..offset + length]` with no further arithmetic.
//!
//! Saved ids are whatever the registry assigned *when the world was saved*
//! -- this module never remaps them. Translating saved ids to this
//! process's current runtime ids ([`cubara_voxel::Chunk::remap_ids`]) needs
//! the id table, which lives in `level.ron`'s header, which lives in
//! `cubara-sim` (this crate must never know about the player, and the
//! header is assembled next to player state) -- so that's the caller's job,
//! between [`read_region_file`]/[`decode_region`] and
//! [`cubara_world::World::load_chunk_edits`].

use std::path::Path;

use cubara_voxel::{Chunk, ChunkCoord, ChunkPayloadError};

use crate::world::World;
use crate::worldgen::TerrainBlocks;

/// Chunks per axis in one region -- `32³` chunks, `512³` blocks.
pub const REGION_SIZE: i32 = 32;

const MAGIC: [u8; 4] = *b"CBRG";
/// This region file schema's own version -- independent of `level.ron`'s
/// `format_version` and of [`crate::WORLDGEN_VERSION`]; each names a
/// different thing that can change on its own schedule.
pub const REGION_FORMAT_VERSION: u16 = 1;

/// A region file failed to read, or a chunk inside it failed to decode.
/// Every variant names the problem; a corrupt file is always a hard error,
/// never a best-effort partial load (`docs/PHASE1_ARCHITECTURE.md` §7.2's
/// "never silent damage").
#[derive(Debug)]
pub enum RegionError {
    Io(std::io::Error),
    /// The file doesn't start with `"CBRG"` -- not a region file at all.
    BadMagic,
    /// A `format_version` this build doesn't know how to read.
    UnsupportedFormatVersion(u16),
    /// The file is shorter than its own directory says it should be.
    Truncated,
    Chunk(ChunkPayloadError),
}

impl std::fmt::Display for RegionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegionError::Io(e) => write!(f, "region file I/O error: {e}"),
            RegionError::BadMagic => write!(f, "not a region file (bad magic bytes)"),
            RegionError::UnsupportedFormatVersion(v) => {
                write!(f, "region file format version {v} is not supported (expected {REGION_FORMAT_VERSION})")
            }
            RegionError::Truncated => write!(f, "region file is truncated"),
            RegionError::Chunk(e) => write!(f, "region file contains a bad chunk payload: {e}"),
        }
    }
}

impl std::error::Error for RegionError {}

/// The region a chunk coordinate belongs to -- floor division by
/// [`REGION_SIZE`] on each axis (`div_euclid`, correct for negative
/// coordinates, same reasoning as [`ChunkCoord::from_block`]).
pub fn region_of(coord: ChunkCoord) -> (i32, i32, i32) {
    (
        coord.x.div_euclid(REGION_SIZE),
        coord.y.div_euclid(REGION_SIZE),
        coord.z.div_euclid(REGION_SIZE),
    )
}

/// A chunk's position within its own region, packed into one `u16`
/// (`0..32768`, well inside range): `z`-outer/`y`-mid/`x`-inner, the same
/// axis order `cubara_sim::WorldHash` already hashes chunk voxels in.
fn local_index(coord: ChunkCoord) -> u16 {
    let lx = coord.x.rem_euclid(REGION_SIZE);
    let ly = coord.y.rem_euclid(REGION_SIZE);
    let lz = coord.z.rem_euclid(REGION_SIZE);
    ((lz * REGION_SIZE + ly) * REGION_SIZE + lx) as u16
}

/// The inverse of [`local_index`]: which chunk coordinate a region + local
/// index names.
fn coord_from_local(region: (i32, i32, i32), local: u16) -> ChunkCoord {
    let idx = i32::from(local);
    let lx = idx % REGION_SIZE;
    let ly = (idx / REGION_SIZE) % REGION_SIZE;
    let lz = idx / (REGION_SIZE * REGION_SIZE);
    ChunkCoord::new(
        region.0 * REGION_SIZE + lx,
        region.1 * REGION_SIZE + ly,
        region.2 * REGION_SIZE + lz,
    )
}

/// `r.<rx>.<ry>.<rz>.cbr`, the region file naming convention (§7.1).
pub fn region_file_name(region: (i32, i32, i32)) -> String {
    format!("r.{}.{}.{}.cbr", region.0, region.1, region.2)
}

/// The inverse of [`region_file_name`] -- `None` for anything that isn't
/// one, so a directory listing can just filter, not fail, on files that
/// don't belong to this format.
pub fn parse_region_file_name(name: &str) -> Option<(i32, i32, i32)> {
    let rest = name.strip_prefix("r.")?.strip_suffix(".cbr")?;
    let mut parts = rest.split('.');
    let rx = parts.next()?.parse().ok()?;
    let ry = parts.next()?.parse().ok()?;
    let rz = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None; // trailing garbage after the third component
    }
    Some((rx, ry, rz))
}

/// Encode `chunks` as one region file's bytes. Sorts by [`local_index`] and
/// writes the directory and payloads in that same order, so the same set of
/// `(coord, chunk)` pairs -- in any input order -- always produces
/// byte-identical output (§7.1's own stated purpose: this is what makes the
/// round-trip test a hash comparison, not a semantic diff).
pub fn encode_region(chunks: &[(ChunkCoord, Chunk)]) -> Result<Vec<u8>, RegionError> {
    let mut entries: Vec<(u16, Vec<u8>)> = Vec::with_capacity(chunks.len());
    for (coord, chunk) in chunks {
        let payload = chunk.write_payload().map_err(RegionError::Chunk)?;
        entries.push((local_index(*coord), payload));
    }
    entries.sort_by_key(|(idx, _)| *idx);

    const DIR_ENTRY_SIZE: u32 = 2 + 4 + 4;
    let header_size = MAGIC.len() as u32 + 2 + 4;
    let mut offset = header_size + entries.len() as u32 * DIR_ENTRY_SIZE;

    let mut out = Vec::with_capacity(offset as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&REGION_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (idx, payload) in &entries {
        out.extend_from_slice(&idx.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        offset += payload.len() as u32;
    }
    for (_, payload) in &entries {
        out.extend_from_slice(payload);
    }
    Ok(out)
}

/// Decode a region file's bytes (as [`encode_region`] produced), resolving
/// each entry's chunk coordinate against `region`. Ids in the returned
/// chunks are whatever was in force when the file was saved -- see the
/// module docs on why remapping isn't done here.
pub fn decode_region(
    bytes: &[u8],
    region: (i32, i32, i32),
) -> Result<Vec<(ChunkCoord, Chunk)>, RegionError> {
    if bytes.len() < MAGIC.len() + 2 + 4 {
        return Err(RegionError::Truncated);
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err(RegionError::BadMagic);
    }
    let mut pos = MAGIC.len();
    let format_version = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
    pos += 2;
    if format_version != REGION_FORMAT_VERSION {
        return Err(RegionError::UnsupportedFormatVersion(format_version));
    }
    let entry_count = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    const DIR_ENTRY_SIZE: usize = 2 + 4 + 4;
    let dir_end = pos + entry_count * DIR_ENTRY_SIZE;
    let directory = bytes.get(pos..dir_end).ok_or(RegionError::Truncated)?;

    let mut result = Vec::with_capacity(entry_count);
    for entry in directory.as_chunks::<DIR_ENTRY_SIZE>().0 {
        let local = u16::from_le_bytes(entry[0..2].try_into().unwrap());
        let offset = u32::from_le_bytes(entry[2..6].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(entry[6..10].try_into().unwrap()) as usize;
        let payload = bytes
            .get(offset..offset + length)
            .ok_or(RegionError::Truncated)?;
        let chunk = Chunk::read_payload(payload).map_err(RegionError::Chunk)?;
        result.push((coord_from_local(region, local), chunk));
    }
    Ok(result)
}

/// Write one region file to `path`. A no-op file with zero entries if
/// `chunks` is empty -- callers that skip empty regions entirely (as
/// [`save_regions`] does) never call this with nothing to write, but the
/// byte format itself is well-defined for it either way.
pub fn write_region_file(path: &Path, chunks: &[(ChunkCoord, Chunk)]) -> Result<(), RegionError> {
    let bytes = encode_region(chunks)?;
    std::fs::write(path, bytes).map_err(RegionError::Io)
}

/// Read and decode one region file.
pub fn read_region_file(
    path: &Path,
    region: (i32, i32, i32),
) -> Result<Vec<(ChunkCoord, Chunk)>, RegionError> {
    let bytes = std::fs::read(path).map_err(RegionError::Io)?;
    decode_region(&bytes, region)
}

/// Write every dirty region of `world` to `region_dir` (`saves/<world>/region/`,
/// though this function is agnostic of that convention -- the caller
/// resolves the full save path). Groups [`World::dirty_chunks`] by
/// [`region_of`] and writes one file per non-empty region; a `World` with no
/// edits writes nothing at all.
pub fn save_regions(
    region_dir: &Path,
    world: &World,
    blocks: TerrainBlocks,
) -> Result<(), RegionError> {
    let dirty = world.dirty_chunks();
    if dirty.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(region_dir).map_err(RegionError::Io)?;

    let mut by_region: std::collections::BTreeMap<(i32, i32, i32), Vec<(ChunkCoord, Chunk)>> =
        std::collections::BTreeMap::new();
    for coord in dirty {
        let chunk = world.edited_chunk_at(coord, blocks);
        by_region
            .entry(region_of(coord))
            .or_default()
            .push((coord, chunk));
    }

    for (region, chunks) in &by_region {
        let path = region_dir.join(region_file_name(*region));
        write_region_file(&path, chunks)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worldgen::TerrainBlocks;
    use cubara_voxel::BlockId;

    fn stone_blocks() -> TerrainBlocks {
        TerrainBlocks {
            oak: None,
            grass: BlockId::STONE,
            soil: BlockId::STONE,
            stone: BlockId::STONE,
        }
    }

    /// A fresh, unique scratch directory for one test -- same pattern as
    /// `cubara_voxel::registry`'s own filesystem tests: process id + thread
    /// id keeps parallel test runs from colliding, removed at the end.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cubara-region-test-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn region_of_floors_toward_negative_infinity() {
        assert_eq!(region_of(ChunkCoord::new(0, 0, 0)), (0, 0, 0));
        assert_eq!(region_of(ChunkCoord::new(31, 31, 31)), (0, 0, 0));
        assert_eq!(region_of(ChunkCoord::new(32, 0, 0)), (1, 0, 0));
        assert_eq!(region_of(ChunkCoord::new(-1, 0, 0)), (-1, 0, 0));
        assert_eq!(region_of(ChunkCoord::new(-32, 0, 0)), (-1, 0, 0));
        assert_eq!(region_of(ChunkCoord::new(-33, 0, 0)), (-2, 0, 0));
    }

    #[test]
    fn local_index_and_coord_from_local_are_inverses() {
        let region = (3, -2, 5);
        for x in [0, 1, 17, 31] {
            for y in [0, 9, 31] {
                for z in [0, 31] {
                    let coord = ChunkCoord::new(
                        region.0 * REGION_SIZE + x,
                        region.1 * REGION_SIZE + y,
                        region.2 * REGION_SIZE + z,
                    );
                    let idx = local_index(coord);
                    assert_eq!(coord_from_local(region, idx), coord, "x={x} y={y} z={z}");
                }
            }
        }
    }

    #[test]
    fn region_file_name_round_trips_through_parse() {
        for region in [(0, 0, 0), (3, -2, 5), (-1, -1, -1), (100, 0, -100)] {
            let name = region_file_name(region);
            assert_eq!(parse_region_file_name(&name), Some(region), "{name}");
        }
    }

    #[test]
    fn parse_region_file_name_rejects_non_region_files() {
        for bad in [
            "level.ron",
            "r.1.2.cbr",
            "r.1.2.3.4.cbr",
            "r.a.b.c.cbr",
            "x.1.2.3.cbr",
        ] {
            assert_eq!(parse_region_file_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn encode_decode_round_trips_and_is_sorted_regardless_of_input_order() {
        let region = (0, 0, 0);
        let a = ChunkCoord::new(0, 0, 0);
        let b = ChunkCoord::new(5, 3, 2);
        let c = ChunkCoord::new(1, 0, 0);

        fn make(coord: ChunkCoord, a: ChunkCoord, b: ChunkCoord, c: ChunkCoord) -> Chunk {
            if coord == a {
                Chunk::from_fn(|_, _, _| BlockId(1))
            } else if coord == b {
                Chunk::from_fn(|x, y, z| BlockId(((x + y + z) % 4) as u16 + 1))
            } else {
                debug_assert_eq!(coord, c);
                Chunk::from_fn(|_, _, _| BlockId::AIR)
            }
        }

        let forward = vec![
            (a, make(a, a, b, c)),
            (b, make(b, a, b, c)),
            (c, make(c, a, b, c)),
        ];
        let backward = vec![
            (c, make(c, a, b, c)),
            (b, make(b, a, b, c)),
            (a, make(a, a, b, c)),
        ];

        let bytes_forward = encode_region(&forward).unwrap();
        let bytes_backward = encode_region(&backward).unwrap();
        assert_eq!(
            bytes_forward, bytes_backward,
            "input order must not affect the encoded bytes"
        );

        let decoded = decode_region(&bytes_forward, region).unwrap();
        let mut coords: Vec<ChunkCoord> = decoded.iter().map(|(c, _)| *c).collect();
        coords.sort();
        let mut expected = vec![a, b, c];
        expected.sort();
        assert_eq!(coords, expected);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let bytes = b"NOPE\x01\x00\x00\x00\x00\x00".to_vec();
        assert!(matches!(
            decode_region(&bytes, (0, 0, 0)),
            Err(RegionError::BadMagic)
        ));
    }

    #[test]
    fn decode_rejects_unsupported_format_version() {
        let mut bytes = encode_region(&[]).unwrap();
        bytes[4] = 99; // format_version low byte
        assert!(matches!(
            decode_region(&bytes, (0, 0, 0)),
            Err(RegionError::UnsupportedFormatVersion(99))
        ));
    }

    #[test]
    fn decode_rejects_truncated_bytes() {
        let full = encode_region(&[(
            ChunkCoord::new(0, 0, 0),
            Chunk::from_fn(|_, _, _| BlockId(1)),
        )])
        .unwrap();
        for len in [0, 3, 9, full.len() - 1] {
            assert!(
                matches!(
                    decode_region(&full[..len], (0, 0, 0)),
                    Err(RegionError::Truncated)
                ),
                "len {len}"
            );
        }
    }

    #[test]
    fn write_and_read_region_file_round_trips_through_disk() {
        let dir = scratch_dir("file-roundtrip");
        let path = dir.join(region_file_name((0, 0, 0)));
        let chunks = vec![
            (
                ChunkCoord::new(0, 0, 0),
                Chunk::from_fn(|_, _, _| BlockId(1)),
            ),
            (
                ChunkCoord::new(2, 1, 0),
                Chunk::from_fn(|x, _, _| BlockId(x as u16 + 1)),
            ),
        ];
        write_region_file(&path, &chunks).unwrap();
        let read_back = read_region_file(&path, (0, 0, 0)).unwrap();
        assert_eq!(read_back.len(), chunks.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_the_same_state_twice_produces_byte_identical_files() {
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockId::AIR);
        world.set_block(100, 0, 100, BlockId::STONE);
        let blocks = stone_blocks();

        let dir_a = scratch_dir("byte-stability-a");
        let dir_b = scratch_dir("byte-stability-b");
        save_regions(&dir_a, &world, blocks).unwrap();
        save_regions(&dir_b, &world, blocks).unwrap();

        let files_a: Vec<_> = std::fs::read_dir(&dir_a)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let files_b: Vec<_> = std::fs::read_dir(&dir_b)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files_a.len(), files_b.len());
        for name in &files_a {
            let bytes_a = std::fs::read(dir_a.join(name)).unwrap();
            let bytes_b = std::fs::read(dir_b.join(name)).unwrap();
            assert_eq!(bytes_a, bytes_b, "{name:?}");
        }
        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }

    #[test]
    fn save_regions_writes_nothing_for_an_unedited_world() {
        let world = World::new();
        let dir = scratch_dir("no-edits");
        save_regions(&dir, &world, stone_blocks()).unwrap();
        assert!(
            !dir.join("region").exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
            "an unedited world writes no region files"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
