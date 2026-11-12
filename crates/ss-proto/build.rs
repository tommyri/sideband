//! Compiles `proto/socialspeak/v1/*.proto` with the vendored `protoc`.
//!
//! Generated Rust is written to `OUT_DIR` and included by `src/lib.rs`. It is
//! not committed. With the `json` feature, `pbjson-build` also writes
//! `serde` implementations from the same descriptors.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=proto");

    let protos = [
        "proto/socialspeak/v1/common.proto",
        "proto/socialspeak/v1/auth.proto",
        "proto/socialspeak/v1/bots.proto",
        "proto/socialspeak/v1/channels.proto",
        "proto/socialspeak/v1/messages.proto",
        "proto/socialspeak/v1/roles.proto",
        "proto/socialspeak/v1/moderation.proto",
        "proto/socialspeak/v1/voice.proto",
        "proto/socialspeak/v1/envelope.proto",
    ];

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let descriptors = out.join("socialspeak.descriptors.bin");
    let mut config = prost_build::Config::new();
    config
        .bytes(["."])
        .file_descriptor_set_path(&descriptors)
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    config.compile_protos(&protos, &["proto"])?;
    #[cfg(feature = "json")]
    json(&descriptors)?;
    Ok(())
}

#[cfg(feature = "json")]
fn json(descriptors: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let set = std::fs::read(descriptors)?;
    // Unknown fields are ignored on input, as protobuf decoding does (T-101).
    pbjson_build::Builder::new()
        .register_descriptors(&set)?
        .ignore_unknown_fields()
        .build(&[".socialspeak"])?;
    Ok(())
}
