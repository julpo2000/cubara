//! The one place a `wgpu::Instance` is made — the window, the headless
//! renderer, the bench and `--caps` all go through [`new_instance`], so they
//! run with the same flags. `scripts/check-architecture.sh` holds that to one.
//!
//! **The trade this makes.** wgpu ≥ 25 turns on
//! [`wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL`] in release builds too: a
//! GPU-side check of every indirect draw's arguments that turns an
//! out-of-bounds or runaway draw into a no-op. On the orbit bench it cost
//! **17% of CPU/frame** (0.252 → 0.208 ms on the RTX 4060 laptop, GPU/frame
//! unchanged) — the whole of what the wgpu 24 → 30 upgrade (#275) added.
//! Release builds give that check up; debug builds keep it, so every test and
//! CI run still validates the arguments.
//!
//! That is sound **because `ChunkArena` writes the indirect arguments on the
//! CPU**, from allocations it made itself. It stops being sound the moment
//! arguments are written on the GPU — GPU-driven culling (#28, #32) or any
//! compute pass that emits draws. Whoever builds that turns validation back
//! on for it (or proves the compute pass cannot write out of range).
//!
//! Two consequences of running without it, both on D3D12 only: an
//! indirectly drawn shader's `@builtin(vertex_index)` ignores `base_vertex`,
//! and its `@builtin(instance_index)` ignores `first_instance`. Direct draws
//! are unaffected — wgpu-hal's D3D12 backend hands both to the shader itself on
//! every direct draw — so `far.wgsl`, which finds its patch by
//! `instance_index`, is sound while its draw stays direct. `mesh.wgsl` — the
//! one indirectly drawn shader — uses neither. `check-architecture.sh` keeps
//! indirect draws in `arena.rs` alone and `mesh.wgsl` off both built-ins.
//!
//! `WGPU_VALIDATION_INDIRECT_CALL=1` (and wgpu's other `WGPU_*` flag
//! variables) still switch it on at run time, for diagnosing a bad draw.

/// A `wgpu::Instance` on the primary backends, with [`instance_flags`] for
/// the build this is.
pub fn new_instance() -> wgpu::Instance {
    wgpu::Instance::new(this_build_descriptor())
}

/// [`descriptor`] for the build this is.
fn this_build_descriptor() -> wgpu::InstanceDescriptor {
    descriptor(cfg!(debug_assertions))
}

/// What [`new_instance`] asks wgpu for, in a debug or release build: the
/// primary backends, [`instance_flags`], then the `WGPU_*` environment
/// variables on top. Apart from `new_instance` only so a test can read the
/// flags back, which a `wgpu::Instance` does not offer.
fn descriptor(debug_build: bool) -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: instance_flags(debug_build).with_env(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    }
}

/// The instance flags for a debug or release build, before the environment
/// is applied. Debug keeps wgpu's full validation, indirect calls included;
/// release has none (see the module docs for why that is sound, and when it
/// stops being).
pub fn instance_flags(debug_build: bool) -> wgpu::InstanceFlags {
    if debug_build {
        wgpu::InstanceFlags::debugging()
    } else {
        wgpu::InstanceFlags::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::InstanceFlags as F;

    #[test]
    fn debug_builds_validate_indirect_calls() {
        // CI's tests run as debug builds, so this is what keeps them checking
        // every indirect draw's arguments.
        let flags = descriptor(true).flags;
        assert!(flags.contains(F::VALIDATION_INDIRECT_CALL));
        assert!(flags.contains(F::VALIDATION));
    }

    #[test]
    fn release_builds_do_not_validate_indirect_calls() {
        // The 17% of CPU/frame this module exists for.
        let desc = descriptor(false);
        assert!(!desc.flags.contains(F::VALIDATION_INDIRECT_CALL));
        assert_eq!(desc.backends, wgpu::Backends::PRIMARY);
    }

    #[test]
    fn the_tests_themselves_run_with_indirect_validation() {
        // CI runs `cargo test` without `--release`, so what `new_instance`
        // asks for here is what every GPU test runs with -- and it must be the
        // debug choice, or the tests stop checking the arguments too. (Under
        // `--release` this fails, which is the point: it says so.)
        let flags = this_build_descriptor().flags;
        assert!(flags.contains(F::VALIDATION_INDIRECT_CALL));
    }
}
