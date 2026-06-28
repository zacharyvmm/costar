//! costar gRPC server binary entry point.

use tonic::transport::Server;

use sim_grpc::proto::simulator_server::SimulatorServer;
use sim_grpc::server::SimulatorServiceImpl;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let addr = "[::1]:9321".parse()?;
    let service = SimulatorServiceImpl::new();

    log::info!("costar gRPC server listening on {}", addr);

    Server::builder()
        .add_service(SimulatorServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
