use std::collections::BTreeSet;

use ontologyx_sim_core::{
    Analysis, AnalysisKind, Circuit, Component, ComponentKind, EngineCapabilities, EngineError,
    EngineId, Net, NetEndpoint, Pin, PinDirection, SignalDomain, SimulationEngine,
    SimulationRequest, SimulationResult, Simulator, validate_circuit,
};

fn analog_pin(id: &str) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
}

fn rc_circuit() -> Circuit {
    Circuit::new()
        .with_component(
            Component::new("r1", ComponentKind::resistor())
                .with_pin(analog_pin("a"))
                .with_pin(analog_pin("b")),
        )
        .with_component(
            Component::new("c1", ComponentKind::capacitor())
                .with_pin(analog_pin("a"))
                .with_pin(analog_pin("b")),
        )
        .with_net(
            Net::new("n1")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("c1", "a")),
        )
}

#[test]
fn valid_circuit_has_no_errors() {
    let report = validate_circuit(&rc_circuit());
    assert!(report.is_valid(), "{report:?}");
    assert_eq!(report.error_count(), 0);
}

#[test]
fn duplicate_component_ids_fail_closed() {
    let circuit = rc_circuit()
        .with_component(Component::new("r1", ComponentKind::resistor()).with_pin(analog_pin("x")));
    let report = validate_circuit(&circuit);
    assert!(!report.is_valid());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "duplicate_component_id")
    );
}

#[test]
fn unknown_pin_is_reported() {
    let circuit = rc_circuit().with_net(Net::new("bad").connect(NetEndpoint::new("r1", "missing")));
    let report = validate_circuit(&circuit);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "unknown_pin")
    );
}

#[test]
fn direct_analog_digital_join_requires_bridge() {
    let circuit = Circuit::new()
        .with_component(
            Component::new("a", ComponentKind::new("analog_source")).with_pin(analog_pin("out")),
        )
        .with_component(
            Component::new("d", ComponentKind::logic_gate()).with_pin(Pin::new(
                "in",
                "in",
                SignalDomain::Digital,
                PinDirection::Input,
            )),
        )
        .with_net(
            Net::new("mixed")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("d", "in")),
        );
    let report = validate_circuit(&circuit);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "mixed_signal_bridge_required")
    );
}

#[test]
fn circuit_ir_round_trips_through_json() {
    let source = rc_circuit();
    let json = serde_json::to_string(&source).unwrap();
    let decoded: Circuit = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, source);
}

struct FakeAnalog;
impl SimulationEngine for FakeAnalog {
    fn id(&self) -> EngineId {
        EngineId::new("fake-analog")
    }
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: true,
            digital: false,
            mixed_signal: false,
            analyses: BTreeSet::from([AnalysisKind::Transient]),
        }
    }
    fn version(&self) -> Option<String> {
        Some("fake-1".to_owned())
    }

    fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult, EngineError> {
        Ok(SimulationResult::new(
            self.id(),
            self.version(),
            request.analysis.kind(),
            request.circuit.schema_version,
            Vec::new(),
            Vec::new(),
        ))
    }
}

#[test]
fn simulator_selects_engine_by_capability() {
    let mut simulator = Simulator::new();
    simulator.register_engine(FakeAnalog);
    let request = SimulationRequest {
        circuit: rc_circuit(),
        analysis: Analysis::Transient {
            step: 1e-6,
            stop: 1e-3,
        },
        probes: Vec::new(),
    };
    let result = simulator.simulate(&request).unwrap();
    assert_eq!(result.engine.as_str(), "fake-analog");
}

#[test]
fn duplicate_model_ids_fail_closed() {
    use ontologyx_sim_core::ModelDefinition;

    let circuit = rc_circuit()
        .with_model(ModelDefinition::spice_device(
            "d1-model",
            "DTEST",
            ".model DTEST D (IS=1e-14)",
        ))
        .with_model(ModelDefinition::spice_device(
            "d1-model",
            "DALT",
            ".model DALT D (IS=1e-15)",
        ));
    let report = validate_circuit(&circuit);
    assert!(!report.is_valid());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "duplicate_model_id")
    );
}

#[test]
fn semiconductor_requires_known_model_reference() {
    let diode = Component::new("d1", ComponentKind::diode())
        .with_pin(analog_pin("anode"))
        .with_pin(analog_pin("cathode"))
        .with_parameter(
            "model",
            ontologyx_sim_core::ParameterValue::Text("missing".to_owned()),
        );
    let circuit = Circuit::new().with_component(diode);
    let report = validate_circuit(&circuit);
    assert!(!report.is_valid());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "unknown_model")
    );
}

#[test]
fn model_registry_round_trips_through_json() {
    use ontologyx_sim_core::ModelDefinition;

    let source = rc_circuit().with_model(ModelDefinition::spice_subcircuit(
        "opamp-model",
        "OX_OPAMP",
        ".subckt OX_OPAMP INP INN VCC VEE OUT\nE1 OUT 0 INP INN 1e5\n.ends OX_OPAMP",
    ));
    let json = serde_json::to_string(&source).unwrap();
    let decoded: Circuit = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, source);
}

#[test]
fn engine_descriptors_expose_version_and_capabilities() {
    let mut simulator = Simulator::new();
    simulator.register_engine(FakeAnalog);
    let descriptors = simulator.engine_descriptors();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id.as_str(), "fake-analog");
    assert_eq!(descriptors[0].version.as_deref(), Some("fake-1"));
    assert!(descriptors[0].capabilities.analog);
    assert!(
        descriptors[0]
            .capabilities
            .analyses
            .contains(&AnalysisKind::Transient)
    );
}

#[test]
fn normalized_result_carries_deterministic_provenance_and_stats() {
    let result = SimulationResult::new(
        EngineId::new("fake-analog"),
        Some("fake-1".to_owned()),
        AnalysisKind::Transient,
        ontologyx_sim_core::CIRCUIT_SCHEMA_VERSION,
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        result.metadata.result_schema_version,
        ontologyx_sim_core::SIMULATION_RESULT_SCHEMA_VERSION
    );
    assert_eq!(result.metadata.core_version, ontologyx_sim_core::VERSION);
    assert_eq!(result.metadata.engine_version.as_deref(), Some("fake-1"));
    assert_eq!(result.stats.waveform_count, 0);
    assert_eq!(result.stats.point_count, 0);
}

#[test]
fn simulation_error_exposes_stable_top_level_code() {
    let error = ontologyx_sim_core::SimulationError::NoCompatibleEngine {
        analysis: AnalysisKind::DigitalTransient,
    };
    assert_eq!(error.code(), "no_compatible_engine");
}

#[test]
fn normalized_result_round_trips_with_explicit_schema_metadata() {
    let source = SimulationResult::new(
        EngineId::new("fake-analog"),
        Some("fake-1".to_owned()),
        AnalysisKind::Transient,
        ontologyx_sim_core::CIRCUIT_SCHEMA_VERSION,
        Vec::new(),
        Vec::new(),
    );
    let json = serde_json::to_string(&source).unwrap();
    assert!(json.contains("\"result_schema_version\":1"));
    assert!(json.contains("\"circuit_schema_version\":1"));
    let decoded: SimulationResult = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, source);
}
