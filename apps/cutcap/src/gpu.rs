//! DX12 diagnostics only; independent of Slint and preview presentation.
use crate::StartupError;

pub fn report() -> Result<(), StartupError> {
    if !wgpu::Instance::enabled_backend_features().contains(wgpu::Backends::DX12) {
        return Err(StartupError::Arguments(
            "The DX12 backend is unavailable on this target.",
        ));
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12));
    if adapters.is_empty() {
        return Err(StartupError::Arguments(
            "No DX12 adapters were found. The software UI can still run.",
        ));
    }
    for adapter in adapters {
        let info = adapter.get_info();
        println!(
            "{} | {:?} | {:?} | driver: {} {}",
            info.name, info.backend, info.device_type, info.driver, info.driver_info
        );
    }
    Ok(())
}
