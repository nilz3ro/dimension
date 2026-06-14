fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile the worker proto for use in the gateway's client code.
    // The gateway uses WorkerServiceClient to forward execution requests to workers.
    tonic_prost_build::compile_protos("../dimension-worker/proto/worker.proto")?;
    Ok(())
}
