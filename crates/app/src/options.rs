//! The options screen and the frame-rate guard: the owner's "per PC, choose
//! looks or performance, and never below the monitor's refresh rate"
//! (2026-09-26), apart from the window that shows it.
//!
//! What is drawn and what a key does are pure functions here; `main.rs` only
//! routes keys and spawns the benchmark.

use crate::far_streaming::FarQuality;
use crate::settings::Settings;

/// What the options screen says: the far terrain's quality and who chose it,
/// the keys, and whether a benchmark is running.
pub fn options_text(settings: &Settings, target_fps: u32, benchmarking: bool) -> String {
    let who = if settings.by_hand {
        "chosen by you".to_string()
    } else {
        match &settings.tuned {
            Some(t) => format!("chosen by the benchmark for {} FPS", t.target_fps),
            None => "not benchmarked yet".to_string(),
        }
    };
    let bench = if benchmarking {
        format!("benchmarking this PC for {target_fps} FPS...")
    } else {
        format!("[B] benchmark this PC for {target_fps} FPS")
    };
    format!(
        "-- OPTIONS --\n\
         [Esc] back\n\
         Far terrain: {} ({who})\n\
         [1] Off  [2] Low  [3] Medium  [4] High  -- looks or frame rate\n\
         [A] let the benchmark choose\n\
         {bench}",
        settings.far.name()
    )
}

/// What a key on the options screen asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionsKey {
    /// The player picks a quality; theirs from now on.
    Quality(FarQuality),
    /// Hand the choice back to the benchmark.
    Auto,
    /// Benchmark now.
    Benchmark,
}

/// The options screen's keys, by the character they type.
pub fn options_key(c: char) -> Option<OptionsKey> {
    Some(match c.to_ascii_lowercase() {
        '1' => OptionsKey::Quality(FarQuality::Off),
        '2' => OptionsKey::Quality(FarQuality::Low),
        '3' => OptionsKey::Quality(FarQuality::Medium),
        '4' => OptionsKey::Quality(FarQuality::High),
        'a' => OptionsKey::Auto,
        'b' => OptionsKey::Benchmark,
        _ => return None,
    })
}

/// Apply `key` to `settings`: a picked quality is the player's, and stays;
/// `Auto` clears both that and the last benchmark, so one runs again.
pub fn apply(settings: &mut Settings, key: OptionsKey) {
    match key {
        OptionsKey::Quality(q) => {
            settings.far = q;
            settings.by_hand = true;
        }
        OptionsKey::Auto => {
            settings.by_hand = false;
            settings.tuned = None;
        }
        OptionsKey::Benchmark => {}
    }
}

/// The frame rate to hold: the monitor's refresh rate, rounded to the nearest
/// whole frame (a "59.94 Hz" monitor reports 59,940 mHz). 60 when the
/// monitor does not say.
pub fn target_fps(refresh_millihertz: Option<u32>) -> u32 {
    refresh_millihertz
        .map(|mhz| (mhz + 500) / 1000)
        .filter(|&fps| fps > 0)
        .unwrap_or(60)
}

/// Seconds after a start, a new world or a quality change during which missed
/// frames are not counted: meshing and streaming a world in costs frames that
/// say nothing about what playing it costs.
const SETTLE_SECONDS: f32 = 10.0;

/// How long a stretch of frames is judged over.
const WINDOW_SECONDS: f32 = 3.0;

/// A frame that took this many times its budget missed its refresh.
const MISSED: f32 = 1.5;

/// The share of a stretch's frames that may miss before the quality steps down.
const MISS_SHARE: f32 = 0.2;

/// Watches frame times for the game falling below the monitor's refresh rate
/// while playing -- the owner's "never below your refresh rate" -- and says
/// when to step the far terrain down one quality.
///
/// With the display in sync a frame takes a whole number of refreshes, so a
/// frame that misses one takes about twice its budget. A stretch where a fifth
/// of frames miss is a machine that cannot hold the rate at this quality; an
/// occasional hitch is not, and is left alone. It only ever steps *down* --
/// finding headroom to step up is the benchmark's job, since a frame waiting
/// on the display says nothing about how much faster it could have been.
#[derive(Clone, Debug)]
pub struct FrameWatch {
    budget: f32,
    settling: f32,
    elapsed: f32,
    frames: u32,
    missed: u32,
}

impl FrameWatch {
    pub fn new(target_fps: u32) -> Self {
        Self {
            budget: 1.0 / target_fps.max(1) as f32,
            settling: SETTLE_SECONDS,
            elapsed: 0.0,
            frames: 0,
            missed: 0,
        }
    }

    /// Start counting again after a settling period: a new world, a new
    /// quality, a benchmark finishing.
    pub fn settle(&mut self) {
        self.settling = SETTLE_SECONDS;
        self.elapsed = 0.0;
        self.frames = 0;
        self.missed = 0;
    }

    /// One frame took `dt` seconds. `true`: step the quality down now.
    pub fn frame(&mut self, dt: f32) -> bool {
        if self.settling > 0.0 {
            self.settling -= dt;
            return false;
        }
        self.elapsed += dt;
        self.frames += 1;
        if dt > MISSED * self.budget {
            self.missed += 1;
        }
        if self.elapsed < WINDOW_SECONDS {
            return false;
        }
        let too_many = self.missed as f32 > MISS_SHARE * self.frames as f32;
        if too_many {
            self.settle();
        } else {
            self.elapsed = 0.0;
            self.frames = 0;
            self.missed = 0;
        }
        too_many
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `frames` frames of `dt` each; whether any of them asked to step down.
    fn run(watch: &mut FrameWatch, frames: u32, dt: f32) -> bool {
        (0..frames).any(|_| watch.frame(dt))
    }

    #[test]
    fn a_steady_refresh_rate_never_steps_down() {
        let mut w = FrameWatch::new(60);
        assert!(!run(&mut w, 60 * 60, 1.0 / 60.0));
    }

    #[test]
    fn missing_every_other_refresh_steps_down_once_settled() {
        let mut w = FrameWatch::new(60);
        // During the first ten seconds nothing counts, however bad: 290
        // frames at 30 FPS is under ten seconds.
        assert!(!run(&mut w, 290, 2.0 / 60.0), "a loading world counted");
        // Then a stretch at 30 FPS on a 60 Hz monitor steps down...
        assert!(run(&mut w, 150, 2.0 / 60.0));
        // ...and settles again before judging the new quality.
        assert!(
            !run(&mut w, 290, 2.0 / 60.0),
            "stepped twice without settling"
        );
    }

    #[test]
    fn an_occasional_hitch_is_left_alone() {
        let mut w = FrameWatch::new(144);
        let budget = 1.0 / 144.0;
        let mut stepped = false;
        for i in 0..144 * 30 {
            let dt = if i % 20 == 0 { 3.0 * budget } else { budget };
            stepped |= w.frame(dt);
        }
        assert!(
            !stepped,
            "one frame in twenty missing is a hitch, not a machine too slow"
        );
    }

    #[test]
    fn the_target_is_the_monitors_refresh_rate() {
        assert_eq!(target_fps(Some(59_940)), 60);
        assert_eq!(target_fps(Some(143_998)), 144);
        assert_eq!(target_fps(Some(240_000)), 240);
        assert_eq!(target_fps(None), 60);
        assert_eq!(target_fps(Some(0)), 60, "a monitor reporting nothing");
    }

    #[test]
    fn picking_a_quality_makes_it_the_players_and_auto_gives_it_back() {
        let mut s = Settings::default();
        apply(&mut s, OptionsKey::Quality(FarQuality::Low));
        assert_eq!(s.far, FarQuality::Low);
        assert!(
            s.by_hand && !s.needs_tuning(60, "gpu"),
            "the benchmark would overrule it"
        );
        apply(&mut s, OptionsKey::Auto);
        assert!(
            !s.by_hand && s.needs_tuning(60, "gpu"),
            "auto did not ask for a benchmark"
        );
    }

    #[test]
    fn the_keys_are_one_to_four_a_and_b() {
        assert_eq!(options_key('1'), Some(OptionsKey::Quality(FarQuality::Off)));
        assert_eq!(
            options_key('4'),
            Some(OptionsKey::Quality(FarQuality::High))
        );
        assert_eq!(options_key('A'), Some(OptionsKey::Auto));
        assert_eq!(options_key('b'), Some(OptionsKey::Benchmark));
        assert_eq!(options_key('5'), None);
    }

    #[test]
    fn the_screen_says_who_chose_and_whether_a_benchmark_runs() {
        let mut s = Settings::default();
        assert!(options_text(&s, 144, false).contains("not benchmarked yet"));
        assert!(options_text(&s, 144, true).contains("benchmarking this PC for 144 FPS"));
        apply(&mut s, OptionsKey::Quality(FarQuality::Medium));
        let t = options_text(&s, 144, false);
        assert!(t.contains("Medium (chosen by you)"), "{t}");
    }
}
