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
use infmon_frontend::logging;

fn main() {
    let _bootstrap_guard = logging::init_bootstrap();

    let config_path = match std::env::args().nth(1) {
        Some(arg) if arg == "--help" || arg == "-h" => {
            eprintln!("Usage: infmon-frontend [CONFIG_PATH]");
            eprintln!();
            eprintln!("  CONFIG_PATH  Path to config.yaml (default: /etc/infmon/config.yaml)");
            std::process::exit(0);
        }
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("/etc/infmon/config.yaml"),
    };

    tracing::info!(
        "infmon-frontend starting with config: {}",
        config_path.display()
    );

    // Set up signal flags before starting so Frontend can share them
    let shutdown = Arc::new(AtomicBool::new(false));
    let reload = Arc::new(AtomicBool::new(false));

    signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");
    signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    signal_hook::flag::register(signal_hook::consts::SIGHUP, reload.clone())
        .expect("failed to register SIGHUP handler");

    // Start the frontend, sharing the shutdown flag
    let mut frontend = match lifecycle::Frontend::start(&config_path, shutdown.clone()) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("failed to start: {e}");
            std::process::exit(1);
        }
    };

    // Main loop: wait for signals
    while !shutdown.load(Ordering::Acquire) {
        if reload.swap(false, Ordering::AcqRel) {
            if let Err(e) = frontend.reload() {
                tracing::error!("reload failed: {e}");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    frontend.stop();
    tracing::info!("infmon-frontend exited");
}
