//! GPU adapter capability report.
//!
//! Prints the adapter and whether it supports the wgpu features the GPU-driven
//! rendering plan depends on (see issue #26 and `PLAN.md` §10). Run with:
//! `cargo run --release -- --caps`, then paste the output into the spike issue.

pub fn run() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no suitable GPU adapter");

    let info = adapter.get_info();
    let features = adapter.features();
    let limits = adapter.limits();

    log::info!(
        "adapter: {} ({:?}, {:?})",
        info.name,
        info.device_type,
        info.backend
    );

    // The features that gate the GPU-driven rendering path, plus the three
    // timestamp-query tiers relevant to the bench's GPU/frame timing
    // (`bench.rs`, `scene.rs`). The base `TIMESTAMP_QUERY` is all it needs:
    // `resolve_query_set`, `Queue::get_timestamp_period`, and setting a
    // render/compute pass descriptor's own `timestamp_writes` field (what
    // `scene.rs` does) are all gated on that alone in wgpu-core, not on
    // either narrower tier. `TIMESTAMP_QUERY_INSIDE_ENCODERS` gates
    // `CommandEncoder::write_timestamp` outside a pass (this crate used that
    // at first -- reads back a silent, permanent 0ms on Metal, since
    // wgpu-hal's encoder-level writes there both sample the same "stage
    // boundary"). `TIMESTAMP_QUERY_INSIDE_PASSES` gates the separate,
    // imperative `RenderPass`/`ComputePass::write_timestamp` (writing more
    // than one timestamp inside a single pass) -- also unused here. All
    // three are still reported, since a caller choosing between writing
    // styles needs to know what's possible on this adapter regardless of
    // which this crate happens to use.
    let checks = [
        (
            "MULTI_DRAW_INDIRECT_COUNT",
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT,
        ),
        (
            "INDIRECT_FIRST_INSTANCE",
            wgpu::Features::INDIRECT_FIRST_INSTANCE,
        ),
        ("TIMESTAMP_QUERY", wgpu::Features::TIMESTAMP_QUERY),
        (
            "TIMESTAMP_QUERY_INSIDE_ENCODERS",
            wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ),
        (
            "TIMESTAMP_QUERY_INSIDE_PASSES",
            wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES,
        ),
    ];
    log::info!("GPU-driven rendering feature support:");
    for (name, feat) in checks {
        let mark = if features.contains(feat) {
            "yes"
        } else {
            "NO "
        };
        log::info!("  [{mark}] {name}");
    }
    // wgpu 27 removed the `MULTI_DRAW_INDIRECT` feature flag: plain
    // `multi_draw_indexed_indirect` moved to this downlevel capability
    // instead of an opt-in feature (`render.rs`'s `gpu_driven_features` has
    // the full story).
    let indirect_execution = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION);
    log::info!(
        "  [{}] INDIRECT_EXECUTION (downlevel capability, not a feature -- gates multi_draw_indexed_indirect)",
        if indirect_execution { "yes" } else { "NO " }
    );

    log::info!(
        "limits: max_buffer_size {} MiB | max_storage_buffer_binding {} MiB | max_bind_groups {}",
        limits.max_buffer_size / (1024 * 1024),
        limits.max_storage_buffer_binding_size / (1024 * 1024),
        limits.max_bind_groups,
    );
}
