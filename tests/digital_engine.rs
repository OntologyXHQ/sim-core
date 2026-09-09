use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, DigitalEngine, ExecutionControl, LogicValue, Net,
    NetEndpoint, ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain,
    SimulationEngine, SimulationRequest, Simulator, Unit, Waveform, validate_circuit,
};

fn digital_pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn logic_input(id: &str, value: LogicValue) -> Component {
    let parameter = match value {
        LogicValue::Zero => ParameterValue::Integer(0),
        LogicValue::One => ParameterValue::Integer(1),
        LogicValue::X => ParameterValue::Text("x".to_owned()),
        LogicValue::Z => ParameterValue::Text("z".to_owned()),
    };
    Component::new(id, ComponentKind::logic_input())
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter("value", parameter)
}

fn sink(id: &str) -> Component {
    Component::new(id, ComponentKind::logic_output())
        .with_pin(digital_pin("in", PinDirection::Input))
}

fn binary_gate(id: &str, kind: ComponentKind) -> Component {
    Component::new(id, kind)
        .with_pin(digital_pin("a", PinDirection::Input))
        .with_pin(digital_pin("b", PinDirection::Input))
        .with_pin(digital_pin("out", PinDirection::Output))
}

fn unary_gate(id: &str, kind: ComponentKind, delay: f64) -> Component {
    Component::new(id, kind)
        .with_pin(digital_pin("in", PinDirection::Input))
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "delay",
            ParameterValue::Quantity(Quantity::new(delay, Unit::Second)),
        )
}

fn digital_request(circuit: Circuit, probe: NetEndpoint, stop: f64) -> SimulationRequest {
    SimulationRequest {
        circuit,
        analysis: Analysis::DigitalTransient { stop },
        probes: vec![Probe {
            endpoint: probe,
            alias: Some("out".to_owned()),
        }],
    }
}

#[test]
fn four_state_logic_is_conservative() {
    assert_eq!(LogicValue::Zero.logic_not(), LogicValue::One);
    assert_eq!(LogicValue::One.logic_not(), LogicValue::Zero);
    assert_eq!(LogicValue::X.logic_not(), LogicValue::X);
    assert_eq!(LogicValue::Z.logic_not(), LogicValue::X);

    assert_eq!(LogicValue::Zero.logic_and(LogicValue::X), LogicValue::Zero);
    assert_eq!(LogicValue::One.logic_and(LogicValue::One), LogicValue::One);
    assert_eq!(LogicValue::One.logic_and(LogicValue::Z), LogicValue::X);

    assert_eq!(LogicValue::One.logic_or(LogicValue::X), LogicValue::One);
    assert_eq!(
        LogicValue::Zero.logic_or(LogicValue::Zero),
        LogicValue::Zero
    );
    assert_eq!(LogicValue::Zero.logic_or(LogicValue::Z), LogicValue::X);

    assert_eq!(LogicValue::One.logic_xor(LogicValue::Zero), LogicValue::One);
    assert_eq!(LogicValue::One.logic_xor(LogicValue::One), LogicValue::Zero);
    assert_eq!(LogicValue::One.logic_xor(LogicValue::X), LogicValue::X);
}

#[test]
fn four_state_logic_serialization_is_stable() {
    assert_eq!(
        serde_json::to_string(&LogicValue::Zero).unwrap(),
        "\"zero\""
    );
    assert_eq!(serde_json::to_string(&LogicValue::One).unwrap(), "\"one\"");
    assert_eq!(serde_json::to_string(&LogicValue::X).unwrap(), "\"x\"");
    assert_eq!(serde_json::to_string(&LogicValue::Z).unwrap(), "\"z\"");
}

#[test]
fn digital_engine_solves_combinational_and_gate() {
    let circuit = Circuit::new()
        .with_component(logic_input("a", LogicValue::One))
        .with_component(logic_input("b", LogicValue::One))
        .with_component(binary_gate("g1", ComponentKind::and_gate()))
        .with_component(sink("out"))
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("g1", "a")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("g1", "b")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("g1", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = digital_request(circuit, NetEndpoint::new("out", "in"), 1e-6);
    let result = DigitalEngine::new().simulate(&request).unwrap();

    assert_eq!(result.engine.as_str(), "digital");
    assert_eq!(result.waveforms.len(), 1);
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected a digital waveform");
    };
    assert_eq!(waveform.signal.as_str(), "out");
    assert_eq!(waveform.transitions.len(), 1);
    assert_eq!(waveform.transitions[0].time, 0.0);
    assert_eq!(waveform.transitions[0].value, LogicValue::One);
}

#[test]
fn all_binary_gate_kinds_map_to_reference_logic_semantics() {
    let cases = [
        (ComponentKind::and_gate(), LogicValue::One),
        (ComponentKind::or_gate(), LogicValue::One),
        (ComponentKind::xor_gate(), LogicValue::Zero),
        (ComponentKind::nand_gate(), LogicValue::Zero),
        (ComponentKind::nor_gate(), LogicValue::Zero),
        (ComponentKind::xnor_gate(), LogicValue::One),
    ];

    for (kind, expected) in cases {
        let circuit = Circuit::new()
            .with_component(logic_input("a", LogicValue::One))
            .with_component(logic_input("b", LogicValue::One))
            .with_component(binary_gate("g1", kind))
            .with_component(sink("out"))
            .with_net(
                Net::new("a")
                    .connect(NetEndpoint::new("a", "out"))
                    .connect(NetEndpoint::new("g1", "a")),
            )
            .with_net(
                Net::new("b")
                    .connect(NetEndpoint::new("b", "out"))
                    .connect(NetEndpoint::new("g1", "b")),
            )
            .with_net(
                Net::new("out")
                    .connect(NetEndpoint::new("g1", "out"))
                    .connect(NetEndpoint::new("out", "in")),
            );
        let request = digital_request(circuit, NetEndpoint::new("out", "in"), 1e-6);
        let result = DigitalEngine::new().simulate(&request).unwrap();
        let Waveform::Digital(waveform) = &result.waveforms[0] else {
            panic!("expected a digital waveform");
        };
        assert_eq!(waveform.transitions.last().unwrap().value, expected);
    }
}

#[test]
fn digital_engine_preserves_gate_delay_as_events() {
    let delay = 5e-9;
    let circuit = Circuit::new()
        .with_component(logic_input("source", LogicValue::One))
        .with_component(unary_gate("inv", ComponentKind::not_gate(), delay))
        .with_component(sink("out"))
        .with_net(
            Net::new("source")
                .connect(NetEndpoint::new("source", "out"))
                .connect(NetEndpoint::new("inv", "in")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("inv", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = digital_request(circuit, NetEndpoint::new("out", "in"), 20e-9);
    let result = DigitalEngine::new().simulate(&request).unwrap();
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected a digital waveform");
    };

    assert_eq!(waveform.transitions.len(), 2);
    assert_eq!(waveform.transitions[0].value, LogicValue::X);
    assert_eq!(waveform.transitions[0].time, 0.0);
    assert_eq!(waveform.transitions[1].value, LogicValue::Zero);
    assert!((waveform.transitions[1].time - delay).abs() < 1e-18);
}

#[test]
fn digital_clock_is_event_driven() {
    let clock = Component::new("clk", ComponentKind::digital_clock())
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(10e-9, Unit::Second)),
        )
        .with_parameter("duty_cycle", ParameterValue::Number(0.5))
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(clock)
        .with_component(sink("out"))
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = digital_request(circuit, NetEndpoint::new("out", "in"), 26e-9);
    let result = DigitalEngine::new().simulate(&request).unwrap();
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected a digital waveform");
    };

    let values = waveform
        .transitions
        .iter()
        .map(|transition| transition.value)
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![
            LogicValue::Zero,
            LogicValue::One,
            LogicValue::Zero,
            LogicValue::One,
            LogicValue::Zero,
            LogicValue::One,
        ]
    );
    assert!((waveform.transitions[1].time - 5e-9).abs() < 1e-18);
    assert!((waveform.transitions[5].time - 25e-9).abs() < 1e-18);
}

#[test]
fn multiple_digital_drivers_are_resolved_instead_of_rejected() {
    let circuit = Circuit::new()
        .with_component(logic_input("a", LogicValue::Zero))
        .with_component(logic_input("b", LogicValue::One))
        .with_component(sink("out"))
        .with_net(
            Net::new("contended")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let report = validate_circuit(&circuit);
    assert!(report.is_valid());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "multiple_digital_drivers_resolved")
    );
    let request = digital_request(circuit, NetEndpoint::new("out", "in"), 1e-6);
    let result = DigitalEngine::new().simulate(&request).unwrap();
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected digital waveform");
    };
    assert_eq!(waveform.transitions.last().unwrap().value, LogicValue::X);
}

#[test]
fn simulator_selects_the_digital_engine_by_analysis_domain() {
    let circuit = Circuit::new()
        .with_component(logic_input("source", LogicValue::One))
        .with_component(sink("out"))
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("source", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = digital_request(circuit, NetEndpoint::new("out", "in"), 1e-6);
    let mut simulator = Simulator::new();
    simulator.register_engine(DigitalEngine::new());
    let result = simulator.simulate(&request).unwrap();
    assert_eq!(result.engine.as_str(), "digital");
}

#[test]
fn digital_engine_honors_pre_cancelled_execution_control() {
    let circuit = Circuit::new()
        .with_component(logic_input("source", LogicValue::One))
        .with_component(sink("out"))
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("source", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = digital_request(circuit, NetEndpoint::new("out", "in"), 1.0);
    let control = ExecutionControl::default();
    control.cancellation.cancel();
    let error = DigitalEngine::new()
        .simulate_with_control(&request, &control)
        .unwrap_err();
    assert!(error.is_cancelled());
}
