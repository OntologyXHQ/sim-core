use ontologyx_sim_core::{
    Analysis, AnalysisKind, Circuit, Component, ComponentKind, EngineRegistry, FirmwareArtifact,
    FirmwareFormat, Net, NetEndpoint, Pin, PinDirection, Probe, RENODE_ENGINE_ID, RenodeEngine,
    RenodeTarget, SignalDomain, SimulationEngine, SimulationRequest, validate_circuit,
};

fn mcu_pin(id: &str, mapping: &str, direction: PinDirection) -> Pin {
    Pin::new(id, mapping, SignalDomain::Digital, direction)
}

fn mcu_request() -> SimulationRequest {
    let mcu = Component::new("mcu", ComponentKind::mcu()).with_pin(mcu_pin(
        "pd13",
        "gpioPortD@13",
        PinDirection::Output,
    ));
    let sink = Component::new("sink", ComponentKind::logic_output()).with_pin(Pin::new(
        "in",
        "in",
        SignalDomain::Digital,
        PinDirection::Input,
    ));
    SimulationRequest {
        circuit: Circuit::new()
            .with_component(mcu)
            .with_component(sink)
            .with_net(
                Net::new("gpio")
                    .connect(NetEndpoint::new("mcu", "pd13"))
                    .connect(NetEndpoint::new("sink", "in")),
            ),
        analysis: Analysis::FirmwareTransient {
            step: 50e-6,
            stop: 1e-3,
        },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("sink", "in"),
            alias: Some("pd13".into()),
        }],
    }
}

#[test]
fn firmware_artifact_contract_round_trips() {
    for artifact in [
        FirmwareArtifact::elf(vec![0x7f, b'E', b'L', b'F']),
        FirmwareArtifact::intel_hex(b":00000001FF\n".to_vec()),
        FirmwareArtifact::binary(vec![1, 2, 3, 4], 0x0800_0000).with_entry_point(0x0800_0001),
    ] {
        artifact.validate().unwrap();
        let json = serde_json::to_string(&artifact).unwrap();
        let restored: FirmwareArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, artifact);
    }
    assert_eq!(FirmwareFormat::Elf, FirmwareArtifact::elf(vec![1]).format);
}

#[test]
fn valid_r6_mcu_component_is_admitted() {
    let request = mcu_request();
    let report = validate_circuit(&request.circuit);
    assert!(report.is_valid(), "{:#?}", report.issues);
}

#[test]
fn r6_rejects_bidirectional_mcu_pin_until_shared_scheduler() {
    let mcu = Component::new("mcu", ComponentKind::mcu()).with_pin(mcu_pin(
        "pa5",
        "gpioPortA@5",
        PinDirection::Bidirectional,
    ));
    let report = validate_circuit(&Circuit::new().with_component(mcu));
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "mcu_pin_direction")
    );
}

#[test]
fn renode_engine_owns_firmware_transient_for_configured_mcu() {
    let request = mcu_request();
    let target = RenodeTarget::new(
        "mcu",
        "platforms/boards/stm32f4_discovery-kit.repl",
        FirmwareArtifact::elf(vec![1]),
    );
    let engine = RenodeEngine::new(target);
    let capabilities = engine.capabilities();
    assert!(
        capabilities
            .analyses
            .contains(&AnalysisKind::FirmwareTransient)
    );
    assert!(capabilities.supports(&request.analysis));

    let mut registry = EngineRegistry::new();
    registry.register(engine);
    let selected = registry.select_request(&request).expect("R6 Renode engine");
    assert_eq!(selected.id().as_str(), RENODE_ENGINE_ID);
}

#[test]
fn renode_target_rejects_host_path_escape() {
    let target = RenodeTarget::new("mcu", "../escape.repl", FirmwareArtifact::elf(vec![1]));
    assert_eq!(
        target.validate().unwrap_err().code(),
        "renode_target_invalid"
    );
}
