//! Which ores exist, and how common they are — loaded from `assets/ores/*.ron`.
//!
//! **Numbers only.** *Where* an ore goes is the threshold in
//! `cubara_world::worldgen`'s density pass (`docs/PHASE2_ARCHITECTURE.md` §6),
//! and it stays there: an ore is a material choice inside terrain generation,
//! which is an algorithm. This file is the part that can be retuned without a
//! recompile (`REQUIREMENTS.md` #3).
//!
//! Loaded here rather than in `cubara-world` for the same reason
//! [`structure`](crate::structure) is: this is where the RON machinery already
//! lives, next to blocks, items and recipes — `cubara-world` has no serde
//! dependency and should not grow one to parse a second copy of the same thing.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One ore, as authored.
///
/// `threshold` and `freq` are integers rather than floats deliberately. The
/// placement decision has to be bit-identical across platforms
/// (`PHASE1_ARCHITECTURE.md` §8.5); an integer in the file plus one
/// IEEE-defined conversion at load is the cheapest way to guarantee that, and
/// it mirrors the oak's `density` being "one in N" rather than a float.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename = "Ore")]
pub struct OreDef {
    pub name: String,
    /// The block this replaces. Only ever a *deep* material — an ore that
    /// replaced grass would surface, and one that replaced a trunk would
    /// hollow out a tree.
    pub replaces: String,
    /// The highest `y` this ore can appear at. Above it, never.
    pub max_y: i32,
    /// Noise above this becomes ore, in **thousandths** (620 → 0.620).
    /// Higher is rarer.
    pub threshold: i32,
    /// Noise frequency, **per 1000 blocks** (45 → 0.045). Lower means larger,
    /// blobbier veins.
    pub freq: i32,
}

impl OreDef {
    /// `threshold` as the fraction the noise is actually compared against.
    ///
    /// One multiplication by a power-of-two-representable constant, from an
    /// exactly-representable integer: identical on every platform, which is
    /// the whole reason the file stores an integer.
    pub fn threshold_f32(&self) -> f32 {
        self.threshold as f32 * 0.001
    }

    /// `freq` as the fraction the sample position is scaled by. See
    /// [`threshold_f32`](Self::threshold_f32).
    pub fn freq_f32(&self) -> f32 {
        self.freq as f32 * 0.001
    }
}

#[derive(Debug)]
pub enum OreError {
    Io {
        file: PathBuf,
        error: std::io::Error,
    },
    Parse {
        file: PathBuf,
        error: ron::error::SpannedError,
    },
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    /// A threshold outside `0..=1000` is either "every stone block is ore" or
    /// "this ore never generates". Both are certainly a typo, and both are
    /// invisible in-game until someone goes looking for iron.
    BadThreshold {
        name: String,
        threshold: i32,
        file: PathBuf,
    },
    /// `freq: 0` would sample the same point everywhere, making the ore either
    /// all-or-nothing across the entire world.
    BadFreq {
        name: String,
        freq: i32,
        file: PathBuf,
    },
}

impl fmt::Display for OreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OreError::Io { file, error } => write!(f, "{}: {error}", file.display()),
            OreError::Parse { file, error } => write!(f, "{}: {error}", file.display()),
            OreError::DuplicateName {
                name,
                first,
                second,
            } => write!(
                f,
                "duplicate ore {name:?}: {} and {}",
                first.display(),
                second.display()
            ),
            OreError::BadThreshold {
                name,
                threshold,
                file,
            } => write!(
                f,
                "{}: ore {name:?} has threshold {threshold}, outside 0..=1000 \
                 (thousandths) -- it would generate either everywhere or nowhere",
                file.display()
            ),
            OreError::BadFreq { name, freq, file } => write!(
                f,
                "{}: ore {name:?} has freq {freq}; it must be positive, or the \
                 noise is sampled at one point for the whole world",
                file.display()
            ),
        }
    }
}

impl std::error::Error for OreError {}

/// Every ore that exists, by name.
#[derive(Debug, Default)]
pub struct OreRegistry {
    by_name: HashMap<String, OreDef>,
}

impl OreRegistry {
    pub fn load(dir: &Path) -> Result<Self, OreError> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| OreError::Io {
            file: dir.to_path_buf(),
            error,
        })?;

        let mut defs = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| OreError::Io {
                file: dir.to_path_buf(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|error| OreError::Io {
                file: path.clone(),
                error,
            })?;
            let def: OreDef = ron::from_str(&text).map_err(|error| OreError::Parse {
                file: path.clone(),
                error,
            })?;
            defs.push((path, def));
        }
        Self::from_defs(defs)
    }

    pub fn from_defs(defs: Vec<(PathBuf, OreDef)>) -> Result<Self, OreError> {
        let mut by_name: HashMap<String, OreDef> = HashMap::new();
        let mut files: HashMap<String, PathBuf> = HashMap::new();

        for (file, def) in defs {
            if !(0..=1000).contains(&def.threshold) {
                return Err(OreError::BadThreshold {
                    name: def.name,
                    threshold: def.threshold,
                    file,
                });
            }
            if def.freq <= 0 {
                return Err(OreError::BadFreq {
                    name: def.name,
                    freq: def.freq,
                    file,
                });
            }
            if let Some(first) = files.get(&def.name) {
                return Err(OreError::DuplicateName {
                    name: def.name,
                    first: first.clone(),
                    second: file,
                });
            }
            files.insert(def.name.clone(), file);
            by_name.insert(def.name.clone(), def);
        }
        Ok(Self { by_name })
    }

    pub fn get(&self, name: &str) -> Option<&OreDef> {
        self.by_name.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Every ore, in **name order**.
    ///
    /// Sorted rather than in `HashMap` order because this is what fills the
    /// generator's fixed-size ore set, and the set is checked in slot order
    /// with the first match winning. An unordered iteration would make which
    /// ore wins an overlap depend on hash order, which is exactly the kind of
    /// thing that makes two machines generate different worlds (Rule 1).
    pub fn sorted(&self) -> Vec<&OreDef> {
        let mut all: Vec<&OreDef> = self.by_name.values().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, threshold: i32, freq: i32) -> (PathBuf, OreDef) {
        (
            PathBuf::from(format!("{name}.ron")),
            OreDef {
                name: name.to_string(),
                replaces: "cubara:stone".to_string(),
                max_y: 40,
                threshold,
                freq,
            },
        )
    }

    #[test]
    fn an_ore_round_trips_through_the_registry() {
        let reg = OreRegistry::from_defs(vec![def("cubara:iron_ore", 620, 45)]).unwrap();
        let iron = reg.get("cubara:iron_ore").unwrap();
        assert_eq!(iron.max_y, 40);
        assert_eq!(iron.replaces, "cubara:stone");
    }

    #[test]
    fn integer_tuning_converts_to_the_fraction_it_names() {
        let reg = OreRegistry::from_defs(vec![def("cubara:iron_ore", 620, 45)]).unwrap();
        let iron = reg.get("cubara:iron_ore").unwrap();
        assert_eq!(iron.threshold_f32(), 0.620);
        assert_eq!(iron.freq_f32(), 0.045);
    }

    #[test]
    fn a_threshold_outside_thousandths_is_rejected() {
        // 1620 reads like "1.62", which no noise value ever exceeds: the ore
        // would silently never generate.
        assert!(matches!(
            OreRegistry::from_defs(vec![def("cubara:iron_ore", 1620, 45)]),
            Err(OreError::BadThreshold { .. })
        ));
        assert!(matches!(
            OreRegistry::from_defs(vec![def("cubara:iron_ore", -1, 45)]),
            Err(OreError::BadFreq { .. } | OreError::BadThreshold { .. })
        ));
    }

    #[test]
    fn a_zero_frequency_is_rejected() {
        assert!(matches!(
            OreRegistry::from_defs(vec![def("cubara:iron_ore", 620, 0)]),
            Err(OreError::BadFreq { .. })
        ));
    }

    #[test]
    fn two_ores_of_the_same_name_are_rejected() {
        assert!(matches!(
            OreRegistry::from_defs(vec![
                def("cubara:iron_ore", 620, 45),
                def("cubara:iron_ore", 700, 45),
            ]),
            Err(OreError::DuplicateName { .. })
        ));
    }

    #[test]
    fn sorted_is_by_name_not_hash_order() {
        // Inserted deliberately out of order: `sorted` is what decides which
        // ore wins an overlap, so it must not inherit HashMap iteration order.
        let reg = OreRegistry::from_defs(vec![
            def("cubara:tin_ore", 620, 45),
            def("cubara:coal_ore", 620, 45),
            def("cubara:iron_ore", 620, 45),
        ])
        .unwrap();
        let names: Vec<&str> = reg.sorted().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["cubara:coal_ore", "cubara:iron_ore", "cubara:tin_ore"]
        );
    }
}
