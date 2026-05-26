fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .boxed(".wuphf.connector.v1.ServerFrame.body.message_create")
        .compile_protos(&["connector.proto"], &["."])?;
    Ok(())
}
