#![cfg(feature = "service")]

use std::{thread, time::Duration};

use ontologyx_sim_core::{
    Analysis, AnalysisKind, Circuit, Component, ComponentKind, DigitalEngine, EngineCapabilities,
    EngineError, EngineId, ExecutionControl, ExecutionPolicy, Net, NetEndpoint, ParameterValue,
    Pin, PinDirection, Probe, ServiceLimits, SignalDomain, SimulationEngine, SimulationJobStatus,
    SimulationRequest, SimulationResult, SimulationService, Simulator, SubmitSimulation,
};

fn pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn request() -> SimulationRequest {
    let source = Component::new("source", ComponentKind::logic_input())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(1));
    let sink = Component::new("sink", ComponentKind::logic_output())
        .with_pin(pin("in", PinDirection::Input));
    SimulationRequest {
        circuit: Circuit::new()
            .with_component(source)
            .with_component(sink)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_job_lifecycle_streams_normalized_result() {
    let mut simulator = Simulator::new();
    simulator.register_engine(DigitalEngine::new());
    let service = SimulationService::new(simulator, ServiceLimits::default()).unwrap();
    let _router = service.router();

    let submitted = service
        .submit(SubmitSimulation { request: request() })
        .await
        .unwrap();
    assert_eq!(submitted.status, SimulationJobStatus::Queued);

    let (initial, mut events) = service.subscribe(&submitted.id).await.unwrap();
    let terminal = if initial.status.is_terminal() {
        initial
    } else {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .expect("service job event timeout")
                .expect("service job event channel");
            if event.job.status.is_terminal() {
                break event.job;
            }
        }
    };

    assert_eq!(terminal.status, SimulationJobStatus::Succeeded);
    let result = terminal.result.expect("normalized simulation result");
    assert_eq!(result.engine.as_str(), "digital");
    assert_eq!(result.waveforms.len(), 1);
    assert!(terminal.error.is_none());
}

#[derive(Clone, Default)]
struct CancellationEngine;

impl SimulationEngine for CancellationEngine {
    fn id(&self) -> EngineId {
        EngineId::new("cancellation-test")
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: false,
            digital: true,
            mixed_signal: false,
            analyses: [AnalysisKind::DigitalTransient].into_iter().collect(),
        }
    }

    fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult, EngineError> {
        self.simulate_with_control(request, &ExecutionControl::default())
    }

    fn simulate_with_control(
        &self,
        _request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, EngineError> {
        for _ in 0..2_000 {
            if control.cancellation.is_cancelled() {
                return Err(EngineError::new(
                    "execution_cancelled",
                    "test worker observed cancellation",
                ));
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err(EngineError::new(
            "test_timeout",
            "cancellation was not delivered",
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_delete_contract_cancels_running_work() {
    let mut simulator = Simulator::new();
    simulator.register_engine(CancellationEngine);
    let service = SimulationService::new(simulator, ServiceLimits::default()).unwrap();
    let submitted = service
        .submit(SubmitSimulation { request: request() })
        .await
        .unwrap();

    loop {
        let snapshot = service.get(&submitted.id).await.unwrap();
        if snapshot.status == SimulationJobStatus::Running {
            break;
        }
        tokio::task::yield_now().await;
    }

    let cancelling = service.cancel(&submitted.id).await.unwrap();
    assert_eq!(cancelling.status, SimulationJobStatus::Cancelling);

    loop {
        let snapshot = service.get(&submitted.id).await.unwrap();
        if snapshot.status.is_terminal() {
            assert_eq!(snapshot.status, SimulationJobStatus::Cancelled);
            assert!(snapshot.result.is_none());
            break;
        }
        tokio::task::yield_now().await;
    }
}

#[test]
fn service_limits_are_fail_closed_and_use_core_execution_policy() {
    let invalid = ServiceLimits {
        max_retained_jobs: 1,
        max_concurrent_jobs: 2,
        max_request_bytes: 1024,
        execution_policy: ExecutionPolicy::default(),
    };
    assert_eq!(
        invalid.validate().unwrap_err().code,
        "service_config_invalid"
    );

    let invalid_poll = ServiceLimits {
        execution_policy: ExecutionPolicy {
            poll_interval_ms: 0,
            ..ExecutionPolicy::default()
        },
        ..ServiceLimits::default()
    };
    assert_eq!(
        invalid_poll.validate().unwrap_err().code,
        "service_config_invalid"
    );
}
