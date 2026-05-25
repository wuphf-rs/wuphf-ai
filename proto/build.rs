/// Generates Rust client and server bindings from the canonical protocol.
fn main() {
    println!("cargo:rerun-if-changed=connector.proto");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["connector.proto"], &["."])
        .expect("compile connector protocol");
}
