use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root");
    let proto_root = workspace_root.join("proto");
    let ingest_proto = proto_root.join("rust_brain/v1/ingest.proto");
    let audit_proto = proto_root.join("rust_brain/v1/audit.proto");
    let pipeline_proto = proto_root.join("rust_brain/v1/pipeline.proto");
    let agent_proto = proto_root.join("rust_brain/v1/agent.proto");

    println!("cargo:rerun-if-changed={}", ingest_proto.display());
    println!("cargo:rerun-if-changed={}", audit_proto.display());
    println!("cargo:rerun-if-changed={}", pipeline_proto.display());
    println!("cargo:rerun-if-changed={}", agent_proto.display());

    let fds = protox::compile(
        [&ingest_proto, &audit_proto, &pipeline_proto, &agent_proto],
        [&proto_root],
    )
        .expect("protobuf compilation failed");

    prost_build::Config::new()
        .compile_fds(fds)
        .expect("prost code generation failed");
}
