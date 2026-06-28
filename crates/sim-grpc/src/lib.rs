//! gRPC server for costar simulation — drives Electron GUI.

/// Generated protobuf code.
pub mod proto {
    tonic::include_proto!("costar.simulator.v1");
}

pub mod inspect;
pub mod server;
pub mod session;
