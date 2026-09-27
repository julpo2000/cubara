//! `--bench flight`: how much of what a player sees while flying is still
//! loading.
//!
//! The owner, 2026-09-27, after flying around: "veel inladen ... zou fijn zijn
//! als dat consistenter goed is". A frame rate says nothing about that, and
//! neither does a golden image of a world loaded up front. So this flies the
//! game's own streaming ([`NodeStreaming`], [`FarStreaming`]) and the game's
//! own renderer, pointed at a texture instead of a window
//! ([`Renderer::offscreen`]), along a straight line at flying speed in real
//! time -- and then flies the same line again, stopping at every sampled
//! point until nothing is left to load. The fraction of pixels that differ
//! between the two at the same eye is what was still loading.
//!
//! Two passes rather than one: settling a second world at each point while
//! the first flies would hand the first's mesh workers free time, and it
//! would look better than a player's.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cubara_render::{CameraPose, Frame, Renderer};
use cubara_world::World;

use crate::far_streaming::{FarQuality, FarStreaming, FAR_VIEW_RADIUS};
use crate::streaming::NodeStreaming;

/// Blocks per second: `cubara_sim::player`'s free-fly speed.
pub const FLY_SPEED: f32 = 24.0;

/// A pixel counts as still loading when a channel is off by more than this
/// from the finished frame. Far above what a far patch one level coarser or
/// a node one level off moves a pixel by on open ground, far below ground
/// against sky.
const TOLERANCE: u8 = 40;

/// What to fly.
#[derive(Clone, Debug)]
pub struct Flight {
    pub start: [f32; 3],
    /// The direction flown, which is also the one looked in.
    pub look: [f32; 3],
    pub speed: f32,
    pub seconds: f32,
    /// Frames per second the flight is paced at: a monitor's refresh rate,
    /// since the renderer uploads a fixed number of meshes per frame.
    pub fps: u32,
    /// How often a frame is kept to compare, in seconds of flight.
    pub every: f32,
    pub size: (u32, u32),
    pub quality: FarQuality,
}

/// Where the eye is `t` seconds into `flight`.
pub fn eye_at(flight: &Flight, t: f32) -> [f32; 3] {
    let dir = glam::Vec3::from(flight.look).normalize_or_zero();
    (glam::Vec3::from(flight.start) + dir * flight.speed * t).to_array()
}

/// The fraction of `live`'s pixels more than [`TOLERANCE`] off `settled`'s.
pub fn loading_fraction(live: &Frame, settled: &Frame) -> f64 {
    cubara_render::headless::compare(&live.pixels, &settled.pixels, TOLERANCE).differing_fraction
}

/// One kept frame: when, where, and how much of it was still loading.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub t: f32,
    pub eye: [f32; 3],
    pub loading: f64,
}

/// What a flight found.
#[derive(Debug)]
pub struct FlightReport {
    pub samples: Vec<Sample>,
    /// The part of each frame of the real-time pass the game spent streaming
    /// and drawing, in seconds. Not the whole frame: the rest is this
    /// measurement's sleep standing in for a display, and a sleep wakes when
    /// it likes -- about 4 ms late at p99 on an idle M3.
    pub work_times: Vec<f32>,
}

impl FlightReport {
    /// The share of kept frames with more than `fraction` of the screen still
    /// loading.
    pub fn frames_over(&self, fraction: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let over = self.samples.iter().filter(|s| s.loading > fraction).count();
        over as f64 / self.samples.len() as f64
    }

    /// The mean fraction of the screen still loading.
    pub fn mean(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().map(|s| s.loading).sum::<f64>() / self.samples.len() as f64
    }

    /// The worst kept frame.
    pub fn worst(&self) -> Option<Sample> {
        self.samples
            .iter()
            .copied()
            .max_by(|a, b| a.loading.total_cmp(&b.loading))
    }

    pub fn line(&self) -> String {
        let worst = self.worst().unwrap_or(Sample {
            t: 0.0,
            eye: [0.0; 3],
            loading: 0.0,
        });
        let (work_p99, work_max) = p99_and_max(&self.work_times);
        format!(
            "FLIGHT: loading mean {:.2}% worst {:.2}% (at {:.2} s, {:.0?}), \
             frames over 1%: {:.0}%, work p99 {:.1} ms max {:.1} ms ({} kept frames)",
            self.mean() * 100.0,
            worst.loading * 100.0,
            worst.t,
            worst.eye,
            self.frames_over(0.01) * 100.0,
            work_p99 * 1000.0,
            work_max * 1000.0,
            self.samples.len()
        )
    }
}

fn p99_and_max(times: &[f32]) -> (f32, f32) {
    let mut sorted = times.to_vec();
    sorted.sort_by(f32::total_cmp);
    let p99 = sorted
        .get(sorted.len().saturating_sub(1) * 99 / 100)
        .copied()
        .unwrap_or(0.0);
    (p99, sorted.last().copied().unwrap_or(0.0))
}

/// Everything a flight streams: the world, the game's two streamers, and the
/// game's renderer.
struct Streamed {
    world: Arc<World>,
    nodes: NodeStreaming,
    far: FarStreaming,
    renderer: Renderer,
}

impl Streamed {
    fn new(flight: &Flight) -> Option<Self> {
        let camera = pose(flight, flight.start);
        let (renderer, assets) = Renderer::offscreen(flight.size.0, flight.size.1, camera)?;
        let layers = assets.layers;
        let registry = Arc::new(assets.registry);
        let nodes = NodeStreaming::new(
            registry,
            &crate::game::load_structure_registry(),
            &crate::game::load_ore_registry(),
            move |name: &str| layers.layer_of(name),
        );
        let world = Arc::new(World::new());
        let far = FarStreaming::new(world.seed(), flight.quality);
        Some(Self {
            world,
            nodes,
            far,
            renderer,
        })
    }

    /// Stream for `eye` and draw one frame, the way the game's frame does.
    fn frame(&mut self, flight: &Flight, eye: [f32; 3]) {
        self.nodes.update(&mut self.renderer, &self.world, eye);
        self.far.update(&mut self.renderer, eye);
        self.renderer.render(
            pose(flight, eye),
            None,
            None,
            &[],
            cubara_render::Hud {
                hotbar: None,
                panel: None,
                health: None,
                crosshair: false,
                menu: None,
            },
            FAR_VIEW_RADIUS as f32,
        );
    }

    /// Stream and draw at `eye` until nothing is left to load, then once more
    /// so what arrived last is drawn.
    fn settle(&mut self, flight: &Flight, eye: [f32; 3]) {
        let waiting = Instant::now();
        loop {
            self.frame(flight, eye);
            if self.settled() {
                break;
            }
            if waiting.elapsed() > Duration::from_secs(60) {
                log::warn!("flight: nothing settled at {eye:?} in a minute");
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.frame(flight, eye);
    }

    fn settled(&self) -> bool {
        self.nodes.settled() && self.far.settled() && self.renderer.uploads_pending() == 0
    }
}

fn pose(flight: &Flight, eye: [f32; 3]) -> CameraPose {
    CameraPose {
        eye: glam::Vec3::from(eye),
        look_dir: glam::Vec3::from(flight.look).normalize_or_zero(),
    }
}

/// Fly `flight` and compare what was on screen with what should have been.
/// With `frames`, the worst kept frame and its finished version are written
/// there as `worst-live.png` and `worst-settled.png`. `None` without a GPU.
pub fn fly(flight: &Flight, frames: Option<&Path>) -> Option<FlightReport> {
    // The real-time pass: what a player sees -- once the start is loaded, as
    // it is for a player who has been standing there. Loading a world from
    // nothing is a different question, with its own answer.
    let mut live = Streamed::new(flight)?;
    live.settle(flight, flight.start);
    let budget = Duration::from_secs_f64(1.0 / flight.fps.max(1) as f64);
    let mut kept: Vec<(f32, [f32; 3], Frame)> = Vec::new();
    let mut work_times = Vec::new();
    let start = Instant::now();
    let mut next_frame = start;
    let mut next_keep = 0.0;
    loop {
        let t = start.elapsed().as_secs_f32();
        if t > flight.seconds {
            break;
        }
        let eye = eye_at(flight, t);
        let working = Instant::now();
        live.frame(flight, eye);
        work_times.push(working.elapsed().as_secs_f32());
        if t >= next_keep {
            let (resident, held, meshing, visible) = live.nodes.counts();
            log::debug!(
                "flight: t {t:5.2} s: {resident} resident, {held} held, {meshing} meshing, \
                 {visible} visible, {} uploads queued",
                live.renderer.uploads_pending()
            );
            kept.push((t, eye, live.renderer.read_frame()?));
            next_keep += flight.every;
        }
        // Paced like a display showing `fps` frames a second: a frame that
        // ran late starts the next at once rather than catching up.
        next_frame += budget;
        let now = Instant::now();
        match next_frame.checked_duration_since(now) {
            Some(rest) => std::thread::sleep(rest),
            None => next_frame = now,
        }
    }
    drop(live);

    // The settled pass: the same eyes, each loaded completely.
    let mut settled = Streamed::new(flight)?;
    let mut samples = Vec::with_capacity(kept.len());
    let mut worst: Option<(f64, Frame, Frame)> = None;
    for (t, eye, frame) in kept {
        settled.far.reselect();
        settled.settle(flight, eye);
        let reference = settled.renderer.read_frame()?;
        let loading = loading_fraction(&frame, &reference);
        log::info!(
            "flight: t {t:5.2} s at {eye:.0?}: {:.2}% loading",
            loading * 100.0
        );
        if let Some(dir) = frames.filter(|_| loading > 0.01) {
            for (kind, f) in [("live", &frame), ("settled", &reference)] {
                save(&dir.join(format!("t{:05.2}-{kind}.png", t)), f);
            }
        }
        if worst.as_ref().is_none_or(|(l, _, _)| loading > *l) {
            worst = Some((loading, frame, reference));
        }
        samples.push(Sample { t, eye, loading });
    }
    if let (Some(dir), Some((_, live, settled))) = (frames, worst) {
        for (name, frame) in [("worst-live.png", live), ("worst-settled.png", settled)] {
            save(&dir.join(name), &frame);
        }
    }
    Some(FlightReport {
        samples,
        work_times,
    })
}

fn save(path: &Path, frame: &Frame) {
    if let Err(e) = image::save_buffer(
        path,
        &frame.pixels,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    ) {
        log::error!("flight: could not write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flight() -> Flight {
        Flight {
            start: [0.0, 100.0, 0.0],
            look: [3.0, 0.0, 4.0],
            speed: 10.0,
            seconds: 5.0,
            fps: 60,
            every: 0.5,
            size: (64, 32),
            quality: FarQuality::High,
        }
    }

    #[test]
    fn the_eye_moves_along_the_look_at_the_speed() {
        let f = flight();
        assert_eq!(eye_at(&f, 0.0), [0.0, 100.0, 0.0]);
        let e = eye_at(&f, 2.0);
        assert!(
            (e[0] - 12.0).abs() < 1e-4 && (e[2] - 16.0).abs() < 1e-4,
            "{e:?}"
        );
        assert_eq!(e[1], 100.0);
    }

    fn frame(pixels: Vec<u8>) -> Frame {
        Frame {
            width: (pixels.len() / 4) as u32,
            height: 1,
            pixels,
        }
    }

    #[test]
    fn a_pixel_counts_as_loading_only_when_far_off() {
        let settled = frame(vec![100, 100, 100, 255, 100, 100, 100, 255]);
        let near = frame(vec![120, 100, 100, 255, 100, 100, 100, 255]);
        let hole = frame(vec![200, 100, 100, 255, 100, 100, 100, 255]);
        assert_eq!(loading_fraction(&near, &settled), 0.0);
        assert_eq!(loading_fraction(&hole, &settled), 0.5);
    }

    #[test]
    fn the_report_counts_the_frames_over_a_share() {
        let sample = |loading| Sample {
            t: 0.0,
            eye: [0.0; 3],
            loading,
        };
        let r = FlightReport {
            samples: vec![sample(0.0), sample(0.02), sample(0.005), sample(0.1)],
            work_times: vec![0.002; 10],
        };
        assert_eq!(r.frames_over(0.01), 0.5);
        assert!((r.mean() - 0.03125).abs() < 1e-12);
        assert_eq!(r.worst().unwrap().loading, 0.1);
    }
}
