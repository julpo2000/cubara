//! Settings that belong to this PC rather than to a world: how finely the far
//! terrain is drawn, and what that choice was tuned for.
//!
//! The owner's call (2026-09-26): a benchmark decides, per PC, what keeps the
//! game at or above the monitor's refresh rate, and the player can choose
//! looks over frame rate or the other way round in the options. So the choice
//! lives here, next to the saves but not in any one of them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::far_streaming::FarQuality;

/// Everything that is this machine's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub far: FarQuality,
    /// What `far` was chosen for by the benchmark, if it was.
    #[serde(default)]
    pub tuned: Option<Tuned>,
    /// The player picked `far` in the options. Their choice stands: no
    /// benchmark overrules it, and missed frames do not step it down.
    #[serde(default)]
    pub by_hand: bool,
}

/// The conditions a benchmark chose the settings under. A different monitor
/// or a different GPU is a reason to benchmark again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuned {
    /// The frame rate the choice had to hold: the monitor's refresh rate.
    pub target_fps: u32,
    /// The GPU it was measured on.
    pub gpu: String,
}

impl Settings {
    /// Whether a benchmark should run before these settings can be trusted
    /// for a monitor at `target_fps` and a GPU called `gpu`: never
    /// benchmarked, or benchmarked for something else. A choice the player
    /// made by hand is theirs, and is not overruled.
    pub fn needs_tuning(&self, target_fps: u32, gpu: &str) -> bool {
        if self.by_hand {
            return false;
        }
        match &self.tuned {
            Some(t) => t.target_fps != target_fps || t.gpu != gpu,
            None => true,
        }
    }
}

/// Where this PC's settings live: beside the world, in the saves folder, which
/// is not part of the project's source.
pub fn settings_path() -> PathBuf {
    cubara_server::assets::repo_root().join("saves/settings.ron")
}

/// The settings at `path`, or `None` when there are none yet or they do not
/// read -- a broken settings file is a reason to benchmark again, not to
/// refuse to start.
pub fn load(path: &Path) -> Option<Settings> {
    let text = std::fs::read_to_string(path).ok()?;
    match ron::from_str(&text) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("{} does not read ({e}); using defaults", path.display());
            None
        }
    }
}

pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = ron::ser::to_string_pretty(settings, ron::ser::PrettyConfig::default())
        .map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuned(fps: u32, gpu: &str) -> Settings {
        Settings {
            far: FarQuality::Medium,
            tuned: Some(Tuned {
                target_fps: fps,
                gpu: gpu.to_string(),
            }),
            by_hand: false,
        }
    }

    #[test]
    fn a_fresh_pc_is_benchmarked() {
        assert!(Settings::default().needs_tuning(60, "Apple M3"));
    }

    #[test]
    fn a_benchmark_holds_until_the_monitor_or_gpu_changes() {
        let s = tuned(144, "Apple M3");
        assert!(!s.needs_tuning(144, "Apple M3"));
        assert!(s.needs_tuning(60, "Apple M3"), "a different monitor");
        assert!(s.needs_tuning(144, "RTX 4060"), "a different GPU");
    }

    #[test]
    fn a_choice_made_by_hand_is_not_overruled() {
        for far in FarQuality::BEST_FIRST {
            let s = Settings {
                far,
                tuned: None,
                by_hand: true,
            };
            assert!(!s.needs_tuning(60, "Apple M3"), "{far:?}");
        }
    }

    #[test]
    fn settings_survive_a_round_trip_and_a_broken_file_is_none() {
        let dir = std::env::temp_dir().join(format!(
            "cubara-settings-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("settings.ron");
        assert_eq!(load(&path), None, "no file yet");
        let s = tuned(240, "RTX 4060");
        save(&path, &s).unwrap();
        assert_eq!(load(&path), Some(s));
        std::fs::write(&path, "not ron at all {").unwrap();
        assert_eq!(load(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
