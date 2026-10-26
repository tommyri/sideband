//! Compiles `proto/socialspeak/v1/*.proto` with the vendored `protoc`.
//!
//! Generated Rust is written to `OUT_DIR` and included by `src/lib.rs`. It is
//! not committed.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=proto");

    let protos = [
        "proto/socialspeak/v1/common.proto",
        "proto/socialspeak/v1/auth.proto",
        "proto/socialspeak/v1/channels.proto",
        "proto/socialspeak/v1/messages.proto",
        "proto/socialspeak/v1/roles.proto",
        "proto/socialspeak/v1/moderation.proto",
        "proto/socialspeak/v1/voice.proto",
        "proto/socialspeak/v1/envelope.proto",
    ];

    let mut config = prost_build::Config::new();
    config
        .bytes(["."])
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    config.compile_protos(&protos, &["proto"])?;
    Ok(())
}
