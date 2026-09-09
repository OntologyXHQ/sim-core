use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, DigitalEngine, LogicValue, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine,
    SimulationRequest, Unit, Waveform, XSpiceEngine,
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

fn binary_gate(id: &str, kind: ComponentKind, delay: f64) -> Component {
    Component::new(id, kind)
        .with_pin(digital_pin("a", PinDirection::Input))
        .with_pin(digital_pin("b", PinDirection::Input))
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "delay",
            ParameterValue::Quantity(Quantity::new(delay, Unit::Second)),
        )
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

fn request(circuit: Circuit, probe: NetEndpoint, stop: f64) -> SimulationRequest {
    SimulationRequest {
        circuit,
        analysis: Analysis::DigitalTransient { stop },
        probes: vec![Probe {
            endpoint: probe,
            alias: Some("out".to_owned()),
        }],
    }
}

fn require_engine() -> Option<XSpiceEngine> {
    let engine = XSpiceEngine::default();
    let info = engine.info();
    if info.available && info.xspice_available {
        eprintln!("using XSPICE: {info:?}");
        return Some(engine);
    }
    if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_XSPICE").is_some() {
        panic!("XSPICE is required for this verification run but is unavailable: {info:?}");
    }
    eprintln!("XSPICE unavailable; integration check skipped: {info:?}");
    None
}

fn digital(result: &ontologyx_sim_core::SimulationResult) -> &ontologyx_sim_core::DigitalWaveform {
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected digital waveform");
    };
    waveform
}

fn last_value(result: &ontologyx_sim_core::SimulationResult) -> LogicValue {
    digital(result).transitions.last().unwrap().value
}

#[test]
fn real_xspice_matches_reference_and_gate_logic() {
    let Some(xspice) = require_engine() else {
        return;
    };
    let delay = 2e-9;
    let circuit = Circuit::new()
        .with_component(logic_input("a", LogicValue::One))
        .with_component(logic_input("b", LogicValue::One))
        .with_component(binary_gate("g1", ComponentKind::and_gate(), delay))
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
    let request = request(circuit, NetEndpoint::new("out", "in"), 20e-9);
    let reference = DigitalEngine::new().simulate(&request).unwrap();
    let external = xspice.simulate(&request).unwrap();

    assert_eq!(last_value(&reference), LogicValue::One);
    assert_eq!(last_value(&external), last_value(&reference));
    assert_eq!(external.engine.as_str(), "xspice");
}

#[test]
fn real_xspice_matches_reference_inverter_delay() {
    let Some(xspice) = require_engine() else {
        return;
    };
    let delay = 5e-9;
    let clock = Component::new("source", ComponentKind::digital_clock())
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(20e-9, Unit::Second)),
        )
        .with_parameter("duty_cycle", ParameterValue::Number(0.5))
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(clock)
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
    let request = request(circuit, NetEndpoint::new("out", "in"), 18e-9);
    let reference = DigitalEngine::new().simulate(&request).unwrap();
    let external = xspice.simulate(&request).unwrap();
    let reference_waveform = digital(&reference);
    let external_waveform = digital(&external);
    let expected_fall = 10e-9 + delay;

    let reference_transition = reference_waveform
        .transitions
        .iter()
        .find(|transition| transition.value == LogicValue::Zero && transition.time > 10e-9)
        .expect("reference inverter should fall after the first clock edge");
    let external_transition = external_waveform
        .transitions
        .iter()
        .find(|transition| transition.value == LogicValue::Zero && transition.time > 10e-9)
        .expect("XSPICE inverter should fall after the first clock edge");
    assert!((reference_transition.time - expected_fall).abs() < 1e-18);
    assert!((external_transition.time - expected_fall).abs() <= 2e-15);
}

#[test]
fn real_xspice_matches_reference_clock_through_buffer() {
    let Some(xspice) = require_engine() else {
        return;
    };
    let delay = 2e-9;
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
        .with_component(unary_gate("buf", ComponentKind::buffer(), delay))
        .with_component(sink("out"))
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("buf", "in")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("buf", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = request(circuit, NetEndpoint::new("out", "in"), 28e-9);
    let reference = DigitalEngine::new().simulate(&request).unwrap();
    let external = xspice.simulate(&request).unwrap();
    let reference_waveform = digital(&reference);
    let external_waveform = digital(&external);
    let parity_start = 5e-9;

    assert_eq!(last_value(&external), last_value(&reference));
    let reference_known = reference_waveform
        .transitions
        .iter()
        .filter(|transition| transition.time >= parity_start)
        .filter(|transition| transition.value.is_known())
        .take(5)
        .collect::<Vec<_>>();
    let external_known = external_waveform
        .transitions
        .iter()
        .filter(|transition| transition.time >= parity_start)
        .filter(|transition| transition.value.is_known())
        .take(5)
        .collect::<Vec<_>>();
    assert_eq!(external_known.len(), reference_known.len());
    for (expected, actual) in reference_known.iter().zip(external_known.iter()) {
        assert_eq!(actual.value, expected.value);
        assert!((actual.time - expected.time).abs() <= 2e-15);
    }
}

#[test]
fn real_xspice_matches_reference_tri_state_resolution() {
    let Some(xspice) = require_engine() else {
        return;
    };
    let tri = |id: &str| {
        Component::new(id, ComponentKind::tri_state_buffer())
            .with_pin(digital_pin("in", PinDirection::Input))
            .with_pin(digital_pin("enable", PinDirection::Input))
            .with_pin(digital_pin("out", PinDirection::Output))
            .with_parameter(
                "delay",
                ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
            )
    };
    let circuit = Circuit::new()
        .with_component(logic_input("a", LogicValue::One))
        .with_component(logic_input("b", LogicValue::Zero))
        .with_component(logic_input("ena", LogicValue::One))
        .with_component(logic_input("enb", LogicValue::Zero))
        .with_component(tri("ta"))
        .with_component(tri("tb"))
        .with_component(sink("out"))
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("ta", "in")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("tb", "in")),
        )
        .with_net(
            Net::new("ena")
                .connect(NetEndpoint::new("ena", "out"))
                .connect(NetEndpoint::new("ta", "enable")),
        )
        .with_net(
            Net::new("enb")
                .connect(NetEndpoint::new("enb", "out"))
                .connect(NetEndpoint::new("tb", "enable")),
        )
        .with_net(
            Net::new("bus")
                .connect(NetEndpoint::new("ta", "out"))
                .connect(NetEndpoint::new("tb", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = request(circuit, NetEndpoint::new("out", "in"), 20e-9);
    let reference = DigitalEngine::new().simulate(&request).unwrap();
    let external = xspice.simulate(&request).unwrap();
    assert_eq!(last_value(&reference), LogicValue::One);
    assert_eq!(last_value(&external), last_value(&reference));
}

#[test]
fn real_xspice_matches_reference_d_flip_flop_after_causal_clock_edge() {
    let Some(xspice) = require_engine() else {
        return;
    };
    let dff = Component::new("ff", ComponentKind::d_flip_flop())
        .with_pin(digital_pin("d", PinDirection::Input))
        .with_pin(digital_pin("clk", PinDirection::Input))
        .with_pin(digital_pin("set", PinDirection::Input))
        .with_pin(digital_pin("reset", PinDirection::Input))
        .with_pin(digital_pin("q", PinDirection::Output))
        .with_pin(digital_pin("nq", PinDirection::Output))
        .with_parameter("initial", ParameterValue::Integer(0))
        .with_parameter(
            "delay",
            ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
        );
    let clock = Component::new("clk", ComponentKind::digital_clock())
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(10e-9, Unit::Second)),
        )
        .with_parameter("duty_cycle", ParameterValue::Number(0.5))
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(logic_input("d", LogicValue::One))
        .with_component(logic_input("set", LogicValue::Zero))
        .with_component(logic_input("reset", LogicValue::Zero))
        .with_component(clock)
        .with_component(dff)
        .with_component(sink("out"))
        .with_net(
            Net::new("d")
                .connect(NetEndpoint::new("d", "out"))
                .connect(NetEndpoint::new("ff", "d")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("ff", "clk")),
        )
        .with_net(
            Net::new("set")
                .connect(NetEndpoint::new("set", "out"))
                .connect(NetEndpoint::new("ff", "set")),
        )
        .with_net(
            Net::new("reset")
                .connect(NetEndpoint::new("reset", "out"))
                .connect(NetEndpoint::new("ff", "reset")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("ff", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("ff", "nq")));
    let request = request(circuit, NetEndpoint::new("out", "in"), 20e-9);
    let reference = DigitalEngine::new().simulate(&request).unwrap();
    let external = xspice.simulate(&request).unwrap();
    assert_eq!(last_value(&reference), LogicValue::One);
    assert_eq!(last_value(&external), LogicValue::One);
}
