//! DX12 adapter enumeration only; independent of the UI and preview presentation.
//! Build with `cargo run -p cutcap-ffi --features gpu-diagnostics --bin cutcap-gpu-info`.
use std::process::ExitCode;

fn main() -> ExitCode {
    if !wgpu::Instance::enabled_backend_features().contains(wgpu::Backends::DX12) {
        eprintln!("The DX12 backend is unavailable on this target.");
        return ExitCode::FAILURE;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12));
    if adapters.is_empty() {
        eprintln!("No DX12 adapters were found. The CPU preview path can still run.");
        return ExitCode::FAILURE;
    }
    for adapter in adapters {
        let info = adapter.get_info();
        println!(
            "{} | {:?} | {:?} | driver: {} {}",
            info.name, info.backend, info.device_type, info.driver, info.driver_info
        );
    }
    ExitCode::SUCCESS
}
