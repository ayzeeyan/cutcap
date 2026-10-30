fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile_with_config(
        "ui/app-window.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent-dark".into()),
    )?;
    Ok(())
}
