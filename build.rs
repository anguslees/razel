use std::error::Error;
use std::path::PathBuf;

const BEP_PROTO: &str = "proto/src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=proto");
    println!("cargo::rustc-check-cfg=cfg(bazel)");

    let out_dir = std::env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?;
    let descriptor_path = PathBuf::from(out_dir).join("build_event_stream_descriptor.bin");
    let includes = [PathBuf::from("proto"), protoc_bin_vendored::include_path()?];
    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        .include_file("bep_proto.rs")
        .skip_source_info()
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    config.compile_protos(&[PathBuf::from(BEP_PROTO)], &includes)?;

    println!(
        "cargo::rustc-env=RAZEL_BEP_DESCRIPTOR_PATH={}",
        descriptor_path.display()
    );
    Ok(())
}
