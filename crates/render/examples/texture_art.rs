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

const CLEAR: Rgba = [0, 0, 0, 0];

/// A sprite being drawn: opaque pixels on a transparent tile.
struct Sprite([Rgba; N * N]);

impl Sprite {
    fn new() -> Self {
        Self([CLEAR; N * N])
    }

    fn set(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if (0..N as i32).contains(&x) && (0..N as i32).contains(&y) {
            self.0[y as usize * N + x as usize] = [c[0], c[1], c[2], 255];
        }
    }

    fn opaque(&self, x: i32, y: i32) -> bool {
        (0..N as i32).contains(&x)
            && (0..N as i32).contains(&y)
            && self.0[y as usize * N + x as usize][3] != 0
    }

    /// A one-pixel dark rim around everything drawn, so an icon reads against
    /// any slot colour and any other icon beside it.
    fn outlined(mut self) -> Vec<Rgba> {
        const RIM: [u8; 3] = [28, 24, 26];
        let mut rim = Vec::new();
        for y in 0..N as i32 {
            for x in 0..N as i32 {
                let touches = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| self.opaque(x + dx, y + dy));
                if !self.opaque(x, y) && touches {
                    rim.push((x, y));
                }
            }
        }
        for (x, y) in rim {
            self.set(x, y, RIM);
        }
        self.0.to_vec()
    }
}

const WOOD_LIGHT: [u8; 3] = [168, 122, 70];
const WOOD_DARK: [u8; 3] = [112, 78, 44];

/// A wooden handle from the bottom-left corner up towards the middle, two
/// pixels thick with the lower edge in shadow.
fn handle(s: &mut Sprite, from: i32, to: i32) {
    for x in from..=to {
        let y = 15 - x;
        s.set(x, y, WOOD_LIGHT);
        s.set(x + 1, y, WOOD_DARK);
    }
}

fn stick() -> Vec<Rgba> {
    let mut s = Sprite::new();
    handle(&mut s, 2, 12);
    s.outlined()
}

/// A pickaxe: the handle, and a curved head across it whose colour is the
/// tier -- wood, stone or iron -- lit along its top edge.
fn pick(head: [u8; 3], light: [u8; 3], dark: [u8; 3]) -> Vec<Rgba> {
    let mut s = Sprite::new();
    handle(&mut s, 2, 9);
    // The head runs across the handle's top along (1, 1), and its two ends
    // curl back towards the handle -- along (-1, 1) -- which is what makes it
    // a crescent rather than a straight bar.
    for t in -5i32..=5 {
        let curl = ((t as f32 / 5.0).powi(2) * 2.5).round() as i32;
        let (x, y) = (10 + t - curl, 5 + t + curl);
        let tip = t.abs() >= 4;
        s.set(x, y, if tip { dark } else { light });
        s.set(x - 1, y + 1, if tip { dark } else { head });
        if t.abs() <= 2 {
            s.set(x - 1, y + 2, dark);
        }
    }
    s.outlined()
}

fn iron_ingot() -> Vec<Rgba> {
    const TOP: [u8; 3] = [226, 226, 232];
    const FACE: [u8; 3] = [182, 182, 192];
    const SHADE: [u8; 3] = [128, 128, 140];
    let mut s = Sprite::new();
    // A bar seen from slightly above: a bevelled top, a front face, a shadow.
    for x in 3..=12 {
        s.set(x, 6, TOP);
    }
    for x in 2..=13 {
        s.set(x, 7, TOP);
    }
    for y in 8..=10 {
        for x in 2..=13 {
            s.set(x, y, FACE);
        }
    }
    for x in 2..=13 {
        s.set(x, 11, SHADE);
    }
    // A glint.
    s.set(4, 7, [250, 250, 252]);
    s.set(5, 7, [250, 250, 252]);
    s.outlined()
}

fn raw_iron() -> Vec<Rgba> {
    const SEED: u32 = 0x0012_0A1E;
    const TONES: [[u8; 3]; 4] = [
        [118, 74, 40],
        [163, 110, 62],
        [196, 142, 88],
        [128, 118, 112],
    ];
    // Three overlapping lumps make one knobbly nugget.
    const LUMPS: [(f32, f32, f32); 3] = [(6.5, 8.5, 3.6), (9.5, 7.0, 3.2), (8.5, 10.5, 3.0)];
    let mut s = Sprite::new();
    for y in 0..N as i32 {
        for x in 0..N as i32 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let inside = LUMPS
                .iter()
                .any(|&(cx, cy, r)| (px - cx).powi(2) + (py - cy).powi(2) <= r * r);
            if !inside {
                continue;
            }
            let pick = (hash(SEED, x as u32, y as u32) % TONES.len() as u32) as usize;
            // Lit from the top-left: the upper pixels lean to the light tones.
            let lit = if px + py < 15.0 { 1.08 } else { 0.9 };
            let [r, g, b, _] = shade(TONES[pick], lit);
            s.set(x, y, [r, g, b]);
        }
    }
    s.outlined()
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/textures");
    save(&dir, "cobble", &cobble());

    // Item icons: sprites on a transparent tile, named `item_<item>` so no
    // block can ever pick one up as a face texture by accident.
    save(&dir, "item_stick", &stick());
    save(
        &dir,
        "item_wooden_pick",
        &pick(WOOD_LIGHT, [196, 150, 96], WOOD_DARK),
    );
    save(
        &dir,
        "item_stone_pick",
        &pick([132, 135, 143], [170, 173, 180], [88, 90, 98]),
    );
    save(
        &dir,
        "item_iron_pick",
        &pick([200, 200, 208], [238, 238, 244], [140, 140, 150]),
    );
    save(&dir, "item_iron_ingot", &iron_ingot());
    save(&dir, "item_raw_iron", &raw_iron());
}
