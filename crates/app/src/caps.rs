//! GPU adapter capability report.
//!
//! Prints the adapter and whether it supports the wgpu features the GPU-driven
//! rendering plan depends on (see issue #26 and `PLAN.md` §10). Run with:
//! `cargo run --release -- --caps`, then paste the output into the spike issue.

pub fn run() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
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
    // timestamp-query tiers the bench's GPU/frame timing depends on
    // (`bench.rs`): the base feature only gets you `resolve_query_set` and
    // `Queue::get_timestamp_period`; writing a timestamp from a command
    // encoder (outside a render pass, which is what the bench uses so it
    // does not also need the pass-scoped tier) needs
    // `TIMESTAMP_QUERY_INSIDE_ENCODERS`; writing one *inside* a pass needs
    // `TIMESTAMP_QUERY_INSIDE_PASSES` on top. The bench only needs the first
    // two, but this reports all three since a caller deciding whether
    // in-pass timing is worth pursuing needs to know if it is even possible
    // here.
    let checks = [
        ("MULTI_DRAW_INDIRECT", wgpu::Features::MULTI_DRAW_INDIRECT),
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

    log::info!(
        "limits: max_buffer_size {} MiB | max_storage_buffer_binding {} MiB | max_bind_groups {}",
        limits.max_buffer_size / (1024 * 1024),
        limits.max_storage_buffer_binding_size / (1024 * 1024),
        limits.max_bind_groups,
    );
}
