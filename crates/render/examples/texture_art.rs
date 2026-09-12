//! The generator for this project's own 16x16 textures.
//!
//! ```bash
//! cargo run -p cubara-render --example texture_art
//! ```
//!
//! Writes into `assets/textures/`. Every image is a pure function of fixed
//! seeds, so running it again reproduces the committed files byte for byte --
//! which is what makes them *ours*: drawn by this code, in this palette, not
//! traced or recoloured from any other game (REQUIREMENTS.md #6).
//!
//! Only textures added after the generator existed live here. The earlier ones
//! (stone, soil, grass, ...) were generated the same way in their own PRs and
//! are not regenerated, so nothing already in a golden image moves.

use std::path::Path;

const N: usize = 16;

type Rgba = [u8; 4];

/// A small, fixed integer hash: the only randomness any texture uses.
fn hash(seed: u32, x: u32, y: u32) -> u32 {
    let mut h = seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(x.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(y.wrapping_mul(0xC2B2_AE35));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^ (h >> 15)
}

/// `0.0..1.0` from [`hash`].
fn unit(seed: u32, x: u32, y: u32) -> f32 {
    (hash(seed, x, y) & 0xFFFF) as f32 / 65536.0
}

fn shade(base: [u8; 3], by: f32) -> Rgba {
    let c = |v: u8| (v as f32 * by).round().clamp(0.0, 255.0) as u8;
    [c(base[0]), c(base[1]), c(base[2]), 255]
}

fn save(dir: &Path, name: &str, pixels: &[Rgba]) {
    let mut img = image::RgbaImage::new(N as u32, N as u32);
    for (i, p) in pixels.iter().enumerate() {
        img.put_pixel((i % N) as u32, (i / N) as u32, image::Rgba(*p));
    }
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("write texture");
    println!("wrote {}", path.display());
}

/// Rounded stones set in dark mortar. Tiles seamlessly: the stone centres wrap
/// around the edges, so a wall of it has no visible seams.
fn cobble() -> Vec<Rgba> {
    const SEED: u32 = 0x00C0_BB1E;
    const MORTAR: [u8; 3] = [78, 80, 88];
    const STONE: [u8; 3] = [132, 135, 143];
    // Hand-placed centres on the 16x16 tile, so the stones are a pleasing
    // size rather than whatever a random scatter lands on.
    const CENTRES: [(f32, f32); 8] = [
        (2.5, 2.5),
        (9.0, 1.5),
        (14.0, 5.5),
        (5.5, 7.5),
        (11.0, 9.5),
        (1.5, 12.5),
        (7.5, 13.5),
        (14.5, 14.0),
    ];
    let wrap = |d: f32| {
        let d = d.abs();
        d.min(N as f32 - d)
    };
    let mut out = Vec::with_capacity(N * N);
    for y in 0..N {
        for x in 0..N {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut dists: Vec<(f32, usize)> = CENTRES
                .iter()
                .enumerate()
                .map(|(i, &(cx, cy))| {
                    let (dx, dy) = (wrap(px - cx), wrap(py - cy));
                    ((dx * dx + dy * dy).sqrt(), i)
                })
                .collect();
            dists.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (near, cell) = dists[0];
            let edge = dists[1].0 - near;
            let grain = 0.92 + unit(SEED, x as u32, y as u32) * 0.16;
            let p = if edge < 1.1 {
                shade(MORTAR, grain)
            } else {
                // Each stone its own tone, lit a little from the top-left.
                let tone = 0.86 + unit(SEED ^ 0x5EED, cell as u32, 0) * 0.2;
                let (cx, cy) = CENTRES[cell];
                let lit = if (px - cx) + (py - cy) < -1.5 {
                    1.08
                } else {
                    1.0
                };
                let dark_rim = if edge < 1.9 { 0.9 } else { 1.0 };
                shade(STONE, tone * grain * lit * dark_rim)
            };
            out.push(p);
        }
    }
    out
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/textures");
    save(&dir, "cobble", &cobble());
}
