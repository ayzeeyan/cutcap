#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod benchmark;
mod bridge;
mod coordinator;
mod engine;
#[cfg(feature = "gpu-diagnostics")]
mod gpu;
mod media_ui;
mod presentation;
mod preview;

use slint::ComponentHandle;
use std::{process::ExitCode, rc::Rc};

slint::include_modules!();

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("window initialization failed: {0}")]
    Platform(#[from] slint::PlatformError),
    #[error("could not start an application worker: {0}")]
    Worker(#[from] std::io::Error),
    #[error(transparent)]
    Engine(#[from] cutcap_contracts::EngineError),
    #[error("logging initialization failed: {0}")]
    Logging(Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Arguments(&'static str),
    #[error(transparent)]
    Media(#[from] cutcap_media::MediaError),
    #[error("benchmark report: {0}")]
    Report(#[from] serde_json::Error),
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

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut benchmark_paths = None;
    match args.as_slice() {
        [] => {}
        [argument] if argument == "--help" => {
            println!(
                "CutCap Phase 1\nRun without arguments for the native editor.\n--gpu-info requires --features gpu-diagnostics (DX12 adapter enumeration only).\n--benchmark-preview <source-directory> <report.json> runs the real preview UI on generated 720p/1080p/4k.mp4 inputs and exits."
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
        [argument, directory, report] if argument == "--benchmark-preview" => {
            benchmark_paths = Some((
                std::path::PathBuf::from(directory),
                std::path::PathBuf::from(report),
            ));
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
    let bridge = Rc::new(bridge::EngineBridge::start()?);
    let coordinator = coordinator::connect(&window, bridge.clone())?;
    if let Some((directory, _)) = &benchmark_paths {
        coordinator
            .media
            .borrow_mut()
            .enable_benchmark(directory.clone());
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "CutCap starting; CPU source preview; external FFmpeg dependency"
    );
    let result = window.run();
    coordinator.timer.stop();
    let media_shutdown = coordinator.media.borrow_mut().shutdown();
    let shutdown = bridge.shutdown();
    let report_result = if let Some((_, path)) = benchmark_paths {
        let media = coordinator.media.borrow();
        let report = media.benchmark_report();
        serde_json::to_writer_pretty(std::fs::File::create(path)?, &report)?;
        if report.completed {
            Ok(())
        } else {
            Err(StartupError::Arguments(
                "Preview benchmark did not complete. Inspect its report.",
            ))
        }
    } else {
        Ok(())
    };
    tracing::info!("CutCap event loop stopped");
    result?;
    shutdown?;
    media_shutdown?;
    report_result?;
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
