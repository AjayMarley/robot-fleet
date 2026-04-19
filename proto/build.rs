fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        // Emit rerun directives so Cargo rebuilds when protos change
        .emit_rerun_if_changed(true)
        .compile_protos(
            &["fleet.proto", "artifacts.proto"],
            &["."],
        )?;
    Ok(())
}
