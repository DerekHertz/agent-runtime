// Compiles proto/runtime.proto with protox (pure Rust), so no system `protoc` is needed.
fn main() {
    let proto_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../proto");
    let proto = format!("{proto_dir}/runtime.proto");
    println!("cargo:rerun-if-changed={proto}");
    let fds = protox::compile([&proto], [proto_dir]).expect("compile proto/runtime.proto");
    tonic_prost_build::configure()
        .compile_fds(fds)
        .expect("generate gRPC code");
}
