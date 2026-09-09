use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, DigitalEngine, LogicValue, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, SignalDomain, SimulationEngine, SimulationRequest,
    Waveform,
};

fn pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn input(id: &str, value: bool) -> Component {
    Component::new(id, ComponentKind::logic_input())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Boolean(value))
}

fn main() {
    let gate = Component::new("and", ComponentKind::and_gate())
        .with_pin(pin("a", PinDirection::Input))
        .with_pin(pin("b", PinDirection::Input))
        .with_pin(pin("out", PinDirection::Output));
    let sink = Component::new("out", ComponentKind::logic_output())
        .with_pin(pin("in", PinDirection::Input));

    let circuit = Circuit::new()
        .with_component(input("a", true))
        .with_component(input("b", true))
        .with_component(gate)
        .with_component(sink)
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("and", "a")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("and", "b")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("and", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    let request = SimulationRequest {
        circuit,
        analysis: Analysis::DigitalTransient { stop: 1e-6 },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("out", "in"),
            alias: Some("out".to_owned()),
        }],
    };

    let result = DigitalEngine::new()
        .simulate(&request)
        .expect("digital simulation failed");
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected a digital waveform");
    };
    assert_eq!(waveform.transitions.last().unwrap().value, LogicValue::One);
    println!("out = {:?}", waveform.transitions.last().unwrap().value);
}
