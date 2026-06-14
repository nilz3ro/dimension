fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = prost_build::Config::new();
    // Use bytes::Bytes for all bytes fields (zero-copy slicing)
    config.bytes(["."]);

    let proto_files = &["proto/dimension.proto"];
    let includes = &["proto/"];
    let file_descriptors = protox::compile(proto_files, includes)?;
    config.compile_fds(file_descriptors)?;

    Ok(())
}
