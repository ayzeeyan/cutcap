#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod coordinator;
mod engine;
#[cfg(feature = "gpu-diagnostics")]
mod gpu;
mod preview;

use slint::ComponentHandle;
use std::{process::ExitCode, rc::Rc};

slint::include_modules!();

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("window initialization failed: {0}")]
    Platform(#[from] slint::PlatformError),
    #[error("could not start the command worker: {0}")]
    Worker(#[from] std::io::Error),
    #[error(transparent)]
    Engine(#[from] cutcap_contracts::EngineError),
    #[error("logging initialization failed: {0}")]
    Logging(Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Arguments(&'static str),
}

fn run() -> Result<(), StartupError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cutcap=info".into()),
        )
        .with_ansi(false)
        .with_target(false)
        .try_init()
        .map_err(StartupError::Logging)?;

    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [argument] if argument == "--help" => {
            println!(
                "CutCap Phase 0\nRun without arguments for the native editor.\n--gpu-info requires --features gpu-diagnostics (DX12 adapter enumeration only)."
            );
            return Ok(());
        }
        [argument] if argument == "--gpu-info" => {
            #[cfg(feature = "gpu-diagnostics")]
            {
                return gpu::report();
            }
            #[cfg(not(feature = "gpu-diagnostics"))]
            {
                return Err(StartupError::Arguments(
                    "Rebuild with --features gpu-diagnostics to use --gpu-info.",
                ));
            }
        }
        _ => return Err(StartupError::Arguments("Unknown arguments. Use --help.")),
    }

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let window = AppWindow::new()?;
    // Avoid the OS cascade placing this large editor below the primary work area.
    // Saved per-monitor workspace placement belongs to the persistence milestone.
    window
        .window()
        .set_position(slint::PhysicalPosition::new(32, 16));
    preview::initialize(&window);
    let bridge = Rc::new(bridge::EngineBridge::start()?);
    let timer = coordinator::connect(&window, bridge.clone());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "CutCap starting; winit/software shell; media unavailable"
    );
    let result = window.run();
    timer.stop();
    let shutdown = bridge.shutdown();
    tracing::info!("CutCap event loop stopped");
    result?;
    shutdown?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            eprintln!("CutCap: {error}");
            ExitCode::FAILURE
        }
    }
}
