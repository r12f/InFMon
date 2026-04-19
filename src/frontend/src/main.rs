//! infmon-frontend binary entry point.
//!
//! Thread layout (spec §4):
//! - main: startup, signal handling, supervision
//! - poller: 1 Hz tick loop (spawned by lifecycle)
//! - exporter-N: one per configured exporter
//! - control: Unix socket for CLI RPCs (future)

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use infmon_frontend::lifecycle;

fn main() {
    env_logger::init();

    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/infmon/config.yaml"));

    log::info!(
        "infmon-frontend starting with config: {}",
        config_path.display()
    );

    // Start the frontend
    let mut frontend = match lifecycle::Frontend::start(&config_path) {
        Ok(f) => f,
        Err(e) => {
            log::error!("failed to start: {e}");
            std::process::exit(1);
        }
    };

    // Set up signal handling
    let shutdown = Arc::new(AtomicBool::new(false));
    let reload = Arc::new(AtomicBool::new(false));

    let shutdown_flag = shutdown.clone();
    let reload_flag = reload.clone();

    signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown_flag.clone())
        .expect("failed to register SIGTERM handler");
    signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown_flag)
        .expect("failed to register SIGINT handler");
    signal_hook::flag::register(signal_hook::consts::SIGHUP, reload_flag)
        .expect("failed to register SIGHUP handler");

    // Main loop: wait for signals
    while !shutdown.load(Ordering::Acquire) {
        if reload.swap(false, Ordering::AcqRel) {
            if let Err(e) = frontend.reload() {
                log::error!("reload failed: {e}");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    frontend.stop();
    log::info!("infmon-frontend exited");
}
