//! Where the mountains are, in numbers: height percentiles, how much of the
//! world is raised, and how steep it gets.
//!
//! ```bash
//! cargo run --release -p cubara-world --example mountain_census -- [seed]
//! ```

use cubara_world::World;

fn main() {
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x005E_ED00_00C0_FFEE);
    let world = World::with_seed(seed);
    let (half, step) = (4096, 8);
    let mut heights = Vec::new();
    let mut steepest = 0;
    let mut z = -half;
    while z < half {
        let mut x = -half;
        while x < half {
            let h = world.surface_height(x, z);
            heights.push(h);
            let dx = (world.surface_height(x + 1, z) - h).abs();
            let dz = (world.surface_height(x, z + 1) - h).abs();
            steepest = steepest.max(dx.max(dz));
            x += step;
        }
        z += step;
    }
    let n = heights.len();
    let raised =
        |above: i32| heights.iter().filter(|&&h| h > above).count() as f64 / n as f64 * 100.0;
    let shares = [60, 100, 150, 200].map(raised);

    heights.sort();
    let pct = |p: f64| heights[((n as f64 * p) as usize).min(n - 1)];
    println!("seed {seed}: {n} columns over {0}x{0} blocks", half * 2);
    println!(
        "height p10 {} p50 {} p90 {} p99 {} max {}",
        pct(0.10),
        pct(0.50),
        pct(0.90),
        pct(0.99),
        heights[n - 1]
    );
    println!(
        "above y=60: {:.1}%  y=100: {:.1}%  y=150: {:.1}%  y=200: {:.1}%",
        shares[0], shares[1], shares[2], shares[3]
    );
    println!("steepest one-block step: {steepest} blocks");
    println!("surface at the origin: {}", world.surface_height(0, 0));
    for (x, z) in [(4000, 0), (11, 11), (8, 8)] {
        println!("surface at ({x}, {z}): {}", world.surface_height(x, z));
    }
}
