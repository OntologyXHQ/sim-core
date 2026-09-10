use ontologyx_sim_core::{
    Analysis, AnalysisKind, Circuit, Component, ComponentKind, MixedSignalEngine, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Quantity, SignalDomain, SimulationEngine, Unit,
    validate_circuit,
};

fn pin(id: &str, domain: SignalDomain, direction: PinDirection) -> Pin {
    Pin::new(id, id, domain, direction)
}

fn bridge_circuit() -> Circuit {
    Circuit::new()
        .with_component(
            Component::new("source", ComponentKind::voltage_source())
                .with_pin(pin("pos", SignalDomain::Analog, PinDirection::Passive))
                .with_pin(pin("neg", SignalDomain::Analog, PinDirection::Passive)),
        )
        .with_component(Component::new("gnd", ComponentKind::ground()).with_pin(pin(
            "gnd",
            SignalDomain::Reference,
            PinDirection::Passive,
        )))
        .with_component(
            Component::new("adc", ComponentKind::adc_bridge())
                .with_pin(pin("analog_in", SignalDomain::Analog, PinDirection::Input))
                .with_pin(pin(
                    "digital_out",
                    SignalDomain::Digital,
                    PinDirection::Output,
                ))
                .with_parameter(
                    "low_threshold",
                    ParameterValue::Quantity(Quantity::new(0.8, Unit::Volt)),
                )
                .with_parameter(
                    "high_threshold",
                    ParameterValue::Quantity(Quantity::new(2.0, Unit::Volt)),
                ),
        )
        .with_component(
            Component::new("dac", ComponentKind::dac_bridge())
                .with_pin(pin(
                    "digital_in",
                    SignalDomain::Digital,
                    PinDirection::Input,
                ))
                .with_pin(pin(
                    "analog_out",
                    SignalDomain::Analog,
                    PinDirection::Output,
                ))
                .with_parameter(
                    "low_voltage",
                    ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
                )
                .with_parameter(
                    "high_voltage",
                    ParameterValue::Quantity(Quantity::new(3.3, Unit::Volt)),
                ),
        )
        .with_net(
            Net::new("analog_in")
                .connect(NetEndpoint::new("source", "pos"))
                .connect(NetEndpoint::new("adc", "analog_in")),
        )
        .with_net(
            Net::new("ground")
                .connect(NetEndpoint::new("source", "neg"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        )
        .with_net(
            Net::new("logic")
                .connect(NetEndpoint::new("adc", "digital_out"))
                .connect(NetEndpoint::new("dac", "digital_in")),
        )
        .with_net(Net::new("analog_out").connect(NetEndpoint::new("dac", "analog_out")))
}

#[test]
fn r4_public_bridge_contract_is_valid() {
    let report = validate_circuit(&bridge_circuit());
    assert!(report.is_valid(), "{:#?}", report.issues);
    assert_eq!(ComponentKind::adc_bridge().as_str(), "adc_bridge");
    assert_eq!(ComponentKind::dac_bridge().as_str(), "dac_bridge");
}

#[test]
fn mixed_engine_owns_only_mixed_transient() {
    let capabilities = MixedSignalEngine::default().capabilities();
    assert!(capabilities.mixed_signal);
    assert!(!capabilities.analog);
    assert!(!capabilities.digital);
    assert!(
        capabilities
            .analyses
            .contains(&AnalysisKind::MixedSignalTransient)
    );
    assert!(capabilities.supports(&Analysis::MixedSignalTransient {
        step: 1e-9,
        stop: 1e-6,
    }));
    assert!(!capabilities.supports(&Analysis::Transient {
        step: 1e-9,
        stop: 1e-6,
    }));
}

#[test]
fn direct_reference_to_digital_join_is_rejected() {
    let circuit = Circuit::new()
        .with_component(Component::new("ref", ComponentKind::ground()).with_pin(pin(
            "gnd",
            SignalDomain::Reference,
            PinDirection::Passive,
        )))
        .with_component(
            Component::new("input", ComponentKind::logic_input())
                .with_pin(pin("out", SignalDomain::Digital, PinDirection::Output))
                .with_parameter("value", ParameterValue::Boolean(false)),
        )
        .with_net(
            Net::new("illegal")
                .connect(NetEndpoint::new("ref", "gnd"))
                .connect(NetEndpoint::new("input", "out")),
        );
    let report = validate_circuit(&circuit);
    assert!(report.issues.iter().any(|issue| {
        issue.code == "mixed_signal_bridge_required"
            && issue.severity == ontologyx_sim_core::IssueSeverity::Error
    }));
}

#[test]
fn invalid_adc_threshold_window_is_rejected_before_engine_execution() {
    let mut circuit = bridge_circuit();
    let adc = circuit
        .components
        .iter_mut()
        .find(|component| component.id.as_str() == "adc")
        .unwrap();
    adc.parameters.insert(
        "low_threshold".to_owned(),
        ParameterValue::Quantity(Quantity::new(2.5, Unit::Volt)),
    );
    let report = validate_circuit(&circuit);
    assert!(report.issues.iter().any(|issue| {
        issue.code == "invalid_adc_threshold_window"
            && issue.severity == ontologyx_sim_core::IssueSeverity::Error
    }));
}
