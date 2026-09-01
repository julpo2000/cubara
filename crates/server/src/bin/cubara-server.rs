//! The dedicated server: a Cubara world with no window, no GPU and no client.
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §3.3's standalone deployment. Its whole job is
//! to parse arguments and call [`cubara_server::headless::run`] — the same call
//! `cubara server` makes, so there is one loop rather than two that drift.
//!
//! This binary links no windowing library and no graphics API, which is the
//! reason it exists rather than being only a subcommand: on a headless host,
//! linking `wgpu` means installing a GPU stack for a process that will never
//! draw a pixel.

use cubara_server::headless;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match headless::parse_args(&args) {
        Ok(cfg) => headless::run(&cfg),
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    }
}
