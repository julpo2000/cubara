//! Writing a file so that a crash cannot leave half of it.
//!
//! Block 2.15. Both halves of the save format used `std::fs::write`, which
//! opens the target, truncates it, and writes into it. A process that dies
//! between the truncate and the last byte leaves a file that is neither the old
//! contents nor the new ones — and for `level.ron` that is a world that no
//! longer loads at all.
//!
//! # The shape
//!
//! Write a sibling temporary, flush it to the operating system, then `rename`
//! it onto the target. A rename within one directory is atomic on both
//! platforms this project supports, so a reader sees the whole old file or the
//! whole new one and never a mixture.
//!
//! The temporary is a *sibling* rather than something under a temp directory,
//! because `rename` is only atomic within a filesystem and a temp directory may
//! be on another one. Crossing that boundary turns the rename into a copy, and
//! a copy is exactly the torn write this exists to prevent.
//!
//! # What this does not promise
//!
//! **Durability against power loss**, which would need an `fsync` of the file
//! *and* of the directory, and a `fsync` per region file would put a disk flush
//! in the middle of a tick loop. What is guaranteed here is the property that
//! actually loses worlds in practice: a file is never half-written, whatever
//! happens to the process.
//!
//! **Nothing across files.** Each file is all-or-nothing on its own; a save is
//! several of them and they are not written as one transaction. See the block's
//! issue for why that bound is acceptable and what it costs.

use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path`, atomically with respect to readers.
///
/// Creates the parent directory if it is missing, so callers do not each have
/// to remember to.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_path(path);

    // A scope, so the file is closed before the rename: Windows refuses to
    // rename onto a path while a handle to the source is open, and this failing
    // only on one platform is exactly the kind of bug that reaches `main`.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Leaving the temporary behind would make the next save fail too,
            // and a save directory that fills with debris is its own bug
            // report. The rename failure is what the caller hears about.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The sibling temporary for `path`.
///
/// A fixed suffix rather than a random one: only the process that owns the save
/// directory writes into it, and two concurrent savers racing over one world
/// would be a bug the name cannot fix. The suffix is distinctive enough that a
/// loader can tell debris from a real file, which is what
/// [`is_temp`] is for.
fn temp_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(TEMP_SUFFIX);
    path.with_file_name(name)
}

/// The suffix [`write_atomic`] gives its in-progress files.
pub const TEMP_SUFFIX: &str = ".writing";

/// Whether a directory entry is one of [`write_atomic`]'s temporaries.
///
/// Loaders skip these. A temporary that survives is the debris of a crash, and
/// reading it would be reading exactly the half-written file the rename exists
/// to hide.
pub fn is_temp(name: &str) -> bool {
    name.ends_with(TEMP_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cubara-durable-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_write_replaces_the_previous_contents() {
        let dir = scratch("replace");
        let p = dir.join("f.bin");
        write_atomic(&p, b"first").expect("first write");
        write_atomic(&p, b"second").expect("second write");
        assert_eq!(std::fs::read(&p).expect("read"), b"second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The temporary does not survive a successful write.
    ///
    /// A save directory that accumulates `.writing` files after every autosave
    /// would be a leak measured in whole worlds.
    #[test]
    fn no_temporary_is_left_behind() {
        let dir = scratch("no-debris");
        let p = dir.join("f.bin");
        write_atomic(&p, b"x").expect("write");
        let left: Vec<String> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| is_temp(n))
            .collect();
        assert!(left.is_empty(), "temporaries left behind: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The property the module exists for**: a file that was being written
    /// when the process died is not the file a reader sees.
    ///
    /// The crash is simulated the only way it can be in-process — by creating
    /// the temporary the way `write_atomic` would and then never renaming it,
    /// which is precisely the state a killed process leaves behind.
    #[test]
    fn a_crash_mid_write_leaves_the_previous_file_intact() {
        let dir = scratch("torn");
        let p = dir.join("level.ron");
        write_atomic(&p, b"the good save").expect("first write");

        // A process that died between `File::create` and `rename`.
        std::fs::write(temp_path(&p), b"half a sav").expect("partial temp");

        assert_eq!(
            std::fs::read(&p).expect("read"),
            b"the good save",
            "the interrupted write damaged the file that was already there"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A write that fails leaves the previous file untouched.**
    ///
    /// This is the property that separates "writes somewhere else first" from
    /// "writes in place", and the one the crash test above cannot see: that
    /// test simulates the crash by creating the temporary itself, so it passes
    /// even against a `write_atomic` that truncates the target directly.
    ///
    /// The failure is forced portably by putting a *directory* where the
    /// temporary belongs, so creating the file there cannot succeed on any
    /// platform. An in-place implementation would have truncated the target
    /// before ever discovering there was a problem.
    #[test]
    fn a_failed_write_does_not_damage_what_was_there() {
        let dir = scratch("failed-write");
        let p = dir.join("level.ron");
        write_atomic(&p, b"the good save").expect("first write");

        std::fs::create_dir(temp_path(&p)).expect("block the temporary");

        assert!(
            write_atomic(&p, b"a new save").is_err(),
            "the write claimed to succeed with its temporary blocked"
        );
        assert_eq!(
            std::fs::read(&p).expect("read"),
            b"the good save",
            "a failed write destroyed the file that was already there"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_parent_directory_is_created() {
        let dir = scratch("parents");
        let p = dir.join("players").join("0.ron");
        write_atomic(&p, b"{}").expect("write into a missing directory");
        assert!(p.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
