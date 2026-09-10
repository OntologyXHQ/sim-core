use std::error::Error;

use ontologyx_sim_core::{
    DigitalEngine, MixedSignalEngine, NgSpiceEngine, ServiceLimits, SimulationService, Simulator,
    VerilatorEngine, XSpiceEngine,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let bind = std::env::var("OXSIM_BIND").unwrap_or_else(|_| "127.0.0.1:4000".to_owned());

    let mut simulator = Simulator::new();
    simulator.register_engine(DigitalEngine::new());
    simulator.register_engine(NgSpiceEngine::default());
    simulator.register_engine(XSpiceEngine::default());
    simulator.register_engine(MixedSignalEngine::default());
    simulator.register_engine(VerilatorEngine::default());

    let service = SimulationService::new(simulator, ServiceLimits::default())?;
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("ontologyx-sim-core service listening on http://{bind}");
    axum::serve(listener, service.router()).await?;
    Ok(())
}
