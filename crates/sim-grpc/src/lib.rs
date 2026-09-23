//! gRPC server for costar simulation.

/// Generated protobuf code.
// tonic's generated service code returns `Result<_, tonic::Status>`.
#[allow(clippy::result_large_err)]
pub mod proto {
    tonic::include_proto!("costar.simulator.v1");
}

pub mod inspect;
pub mod server;
pub mod session;
