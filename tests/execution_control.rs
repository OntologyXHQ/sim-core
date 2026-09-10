use std::collections::BTreeSet;

use ontologyx_sim_core::{
    Analysis, AnalysisKind, CancellationToken, Circuit, Component, ComponentKind,
    EngineCapabilities, EngineError, EngineId, ExecutionControl, ExecutionPolicy, Net, NetEndpoint,
    NgSpiceEngine, Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine,
    SimulationRequest, SimulationResult, Simulator, Unit,
};

fn analog_pin(id: &str) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
}

fn request() -> SimulationRequest {
    let source = Component::new("v1", ComponentKind::voltage_source())
        .with_pin(analog_pin("pos"))
        .with_pin(analog_pin("neg"))
        .with_parameter(
            "dc",
            ontologyx_sim_core::ParameterValue::Quantity(Quantity::new(5.0, Unit::Volt)),
        );
    let resistor = Component::new("r1", ComponentKind::resistor())
        .with_pin(analog_pin("a"))
        .with_pin(analog_pin("b"))
        .with_parameter(
            "resistance",
            ontologyx_sim_core::ParameterValue::Quantity(Quantity::new(1000.0, Unit::Ohm)),
        );
    let ground = Component::new("gnd", ComponentKind::ground()).with_pin(Pin::new(
        "gnd",
        "gnd",
        SignalDomain::Reference,
        PinDirection::Passive,
    ));

    SimulationRequest {
        circuit: Circuit::new()
            .with_component(source)
            .with_component(resistor)
            .with_component(ground)
            .with_net(
                Net::new("vin")
                    .connect(NetEndpoint::new("v1", "pos"))
                    .connect(NetEndpoint::new("r1", "a")),
            )
            .with_net(
                Net::new("gnd")
                    .connect(NetEndpoint::new("v1", "neg"))
                    .connect(NetEndpoint::new("r1", "b"))
                    .connect(NetEndpoint::new("gnd", "gnd")),
            ),
        analysis: Analysis::OperatingPoint,
        probes: vec![Probe {
            endpoint: NetEndpoint::new("v1", "pos"),
            alias: Some("vin".to_owned()),
        }],
    }
}

struct PanicEngine;

impl SimulationEngine for PanicEngine {
    fn id(&self) -> EngineId {
        EngineId::new("panic-engine")
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: true,
            digital: false,
            mixed_signal: false,
            analyses: BTreeSet::from([AnalysisKind::OperatingPoint]),
        }
    }

    fn simulate(&self, _request: &SimulationRequest) -> Result<SimulationResult, EngineError> {
        panic!("cancelled execution must not invoke the underlying engine")
    }
}

#[test]
fn default_execution_policy_is_bounded() {
    let policy = ExecutionPolicy::default();
    assert!(policy.timeout_ms > 0);
    assert!(policy.poll_interval_ms > 0);
    assert!(policy.max_input_bytes > 0);
    assert!(policy.max_output_bytes > 0);
    assert!(policy.max_log_bytes > 0);
    policy.validate().unwrap();
}

#[test]
fn unbounded_execution_policy_is_explicit() {
    let policy = ExecutionPolicy::unbounded();
    assert_eq!(policy.timeout_ms, 0);
    assert_eq!(policy.max_input_bytes, 0);
    assert_eq!(policy.max_output_bytes, 0);
    assert_eq!(policy.max_log_bytes, 0);
    assert!(policy.poll_interval_ms > 0);
}

#[test]
fn invalid_execution_policy_fails_closed() {
    let policy = ExecutionPolicy {
        poll_interval_ms: 0,
        ..ExecutionPolicy::default()
    };
    let error = policy.validate().unwrap_err();
    assert_eq!(error.code(), "execution_policy_invalid");
}

#[test]
fn pre_cancelled_control_never_invokes_engine() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let control = ExecutionControl::default().with_cancellation(cancellation);
    let mut simulator = Simulator::new();
    simulator.register_engine(PanicEngine);

    let error = simulator
        .simulate_with_control(&request(), &control)
        .unwrap_err();
    let ontologyx_sim_core::SimulationError::Engine { error, .. } = error else {
        panic!("expected engine execution error");
    };
    assert_eq!(error.code(), "execution_cancelled");
    assert!(error.is_cancelled());
}

#[cfg(unix)]
mod unix_process_control {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static TEMP_EXECUTABLE_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempExecutable {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempExecutable {
        fn new(body: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let counter = TEMP_EXECUTABLE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ontologyx-sim-core-execution-test-{}-{nonce}-{counter}",
                std::process::id()
            ));
            fs::create_dir(&dir).unwrap();
            let path = dir.join("fake-ngspice");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
            Self { dir, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempExecutable {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn ngspice_timeout_terminates_the_child_promptly() {
        let executable = TempExecutable::new("exec sleep 5");
        let engine = NgSpiceEngine::new(executable.path());
        let control = ExecutionControl::new(ExecutionPolicy {
            timeout_ms: 40,
            poll_interval_ms: 5,
            ..ExecutionPolicy::default()
        });

        let started = Instant::now();
        let error = engine
            .simulate_with_control(&request(), &control)
            .unwrap_err();
        assert!(error.is_timeout(), "unexpected error: {error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn ngspice_cancellation_terminates_the_child_promptly() {
        let executable = TempExecutable::new("exec sleep 5");
        let engine = NgSpiceEngine::new(executable.path());
        let cancellation = CancellationToken::new();
        let cancellation_from_thread = cancellation.clone();
        let control = ExecutionControl::new(ExecutionPolicy {
            timeout_ms: 2_000,
            poll_interval_ms: 5,
            ..ExecutionPolicy::default()
        })
        .with_cancellation(cancellation);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            cancellation_from_thread.cancel();
        });

        let started = Instant::now();
        let error = engine
            .simulate_with_control(&request(), &control)
            .unwrap_err();
        canceller.join().unwrap();
        assert!(error.is_cancelled(), "unexpected error: {error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn ngspice_log_growth_is_resource_bounded() {
        let executable = TempExecutable::new("head -c 65536 /dev/zero > ngspice.log\nexec sleep 5");
        let engine = NgSpiceEngine::new(executable.path());
        let control = ExecutionControl::new(ExecutionPolicy {
            timeout_ms: 2_000,
            poll_interval_ms: 5,
            max_log_bytes: 1024,
            ..ExecutionPolicy::default()
        });

        let started = Instant::now();
        let error = engine
            .simulate_with_control(&request(), &control)
            .unwrap_err();
        assert!(error.is_resource_limit(), "unexpected error: {error}");
        assert!(error.message().contains("log size"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
