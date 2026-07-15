//! Optional CPU profiling.
//!
//! Uses [`puffin`] scope macros throughout the hot paths; they are essentially
//! free unless profiling is enabled. Building with `--features profile` turns
//! scopes on and starts a `puffin_http` server, which the standalone
//! `puffin_viewer` can connect to for a live flame graph.
//!
//! Enable and view:
//! ```text
//! cargo run --release --features profile -- --bench
//! # in another terminal:  cargo install puffin_viewer && puffin_viewer --url 127.0.0.1:8585
//! ```

/// Keeps the profiling server alive for as long as it is held. A no-op handle
/// when the `profile` feature is off.
pub struct Profiler {
    #[cfg(feature = "profile")]
    _server: puffin_http::Server,
}

impl Profiler {
    /// Turn profiling on (when built with `--features profile`) and start the
    /// server. Returns `None` when profiling is disabled or the server fails.
    #[must_use]
    pub fn init() -> Option<Self> {
        #[cfg(feature = "profile")]
        {
            puffin::set_scopes_on(true);
            match puffin_http::Server::new("127.0.0.1:8585") {
                Ok(server) => {
                    log::info!(
                        "puffin profiling enabled — connect puffin_viewer to 127.0.0.1:8585"
                    );
                    Some(Self { _server: server })
                }
                Err(err) => {
                    log::error!("failed to start puffin server: {err}");
                    None
                }
            }
        }
        #[cfg(not(feature = "profile"))]
        {
            None
        }
    }

    /// Mark the end of a frame for the profiler. Cheap no-op when scopes are off.
    pub fn new_frame() {
        puffin::GlobalProfiler::lock().new_frame();
    }
}
