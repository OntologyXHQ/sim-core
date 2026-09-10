#![cfg(all(feature = "production", target_os = "linux"))]

use std::path::Path;

use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, ExecutionControl, IsolatedProcessExecutor,
    IsolatedProcessLimits, Net, NetEndpoint, ParameterValue, Pin, PinDirection, Probe,
    RuntimeExecutor, SignalDomain, SimulationRequest, default_worker_simulator,
};

fn pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn request() -> SimulationRequest {
    SimulationRequest {
        circuit: Circuit::new()
            .with_component(
                Component::new("source", ComponentKind::logic_input())
                    .with_pin(pin("out", PinDirection::Output))
                    .with_parameter("value", ParameterValue::Integer(1)),
            )
            .with_component(
                Component::new("sink", ComponentKind::logic_output())
                    .with_pin(pin("in", PinDirection::Input)),
            )
            .with_net(
                Net::new("signal")
                    .connect(NetEndpoint::new("source", "out"))
                    .connect(NetEndpoint::new("sink", "in")),
            ),
        analysis: Analysis::DigitalTransient { stop: 1e-6 },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("sink", "in"),
            alias: Some("signal".to_owned()),
        }],
    }
}

#[test]
fn real_linux_worker_is_namespaced_and_resource_limited() {
    let required = std::env::var_os("ONTOLOGYX_SIM_REQUIRE_PRODUCTION_ISOLATION").is_some();
    let worker = Path::new(env!("CARGO_BIN_EXE_sim-worker"));
    let descriptors = default_worker_simulator().engine_descriptors();
    let executor = match IsolatedProcessExecutor::new(
        worker,
        descriptors,
        format!("sim-worker-{}", ontologyx_sim_core::VERSION),
        IsolatedProcessLimits::default(),
    ) {
        Ok(executor) => executor,
        Err(error) if !required => {
            eprintln!("skipping production isolation proof: {error}");
            return;
        }
        Err(error) => panic!("production isolation is required: {error}"),
    };

    let preview = executor.isolation_command_preview();
    assert!(preview.iter().any(|arg| arg == "--unshare-net"));
    assert!(preview.iter().any(|arg| arg.starts_with("--as=")));
    assert!(preview.iter().any(|arg| arg.starts_with("--cpu=")));

    let result = executor
        .execute(&request(), &ExecutionControl::default())
        .unwrap();
    assert_eq!(result.engine.as_str(), "digital");
    assert_eq!(result.waveforms.len(), 1);
}
