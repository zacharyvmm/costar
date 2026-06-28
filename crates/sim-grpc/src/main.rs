//! costar gRPC server binary entry point.

mod server;
pub mod session;
pub mod inspect;

use tonic::transport::Server;

use sim_grpc::proto::simulator_server::SimulatorServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let addr = "[::1]:9321".parse()?;
    let service = server::SimulatorServiceImpl::new();

    log::info!("costar gRPC server listening on {}", addr);

    Server::builder()
        .add_service(SimulatorServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
