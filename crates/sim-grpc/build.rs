fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure a `protoc` is available without requiring a system install.
    // CI runners, forks, and fresh dev machines don't ship protoc, so fall
    // back to a vendored binary when PROTOC isn't already set. An explicit
    // PROTOC env (e.g. a workspace-local build) still takes precedence.
    if std::env::var_os("PROTOC").is_none() {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/simulator.proto"], &["proto/"])?;
    Ok(())
}
