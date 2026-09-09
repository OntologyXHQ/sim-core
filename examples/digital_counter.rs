use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, DigitalEngine, LogicValue, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine,
    SimulationRequest, Unit, Waveform,
};

fn pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn main() {
    let clock = Component::new("clk", ComponentKind::digital_clock())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(10e-9, Unit::Second)),
        )
        .with_parameter("duty_cycle", ParameterValue::Number(0.5))
        .with_parameter("initial", ParameterValue::Integer(0));
    let enable = Component::new("enable", ComponentKind::logic_input())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(1));
    let counter = Component::new("counter", ComponentKind::counter())
        .with_pin(pin("clk", PinDirection::Input))
        .with_pin(pin("enable", PinDirection::Input))
        .with_pin(pin("q0", PinDirection::Output))
        .with_pin(pin("q1", PinDirection::Output))
        .with_pin(pin("q2", PinDirection::Output))
        .with_parameter("width", ParameterValue::Integer(3))
        .with_parameter("initial", ParameterValue::Integer(0));
    let sink = Component::new("out", ComponentKind::logic_output())
        .with_pin(pin("in", PinDirection::Input));

    let circuit = Circuit::new()
        .with_component(clock)
        .with_component(enable)
        .with_component(counter)
        .with_component(sink)
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("counter", "clk")),
        )
        .with_net(
            Net::new("enable")
                .connect(NetEndpoint::new("enable", "out"))
                .connect(NetEndpoint::new("counter", "enable")),
        )
        .with_net(
            Net::new("q0")
                .connect(NetEndpoint::new("counter", "q0"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("q1").connect(NetEndpoint::new("counter", "q1")))
        .with_net(Net::new("q2").connect(NetEndpoint::new("counter", "q2")));

    let result = DigitalEngine::new()
        .simulate(&SimulationRequest {
            circuit,
            analysis: Analysis::DigitalTransient { stop: 26e-9 },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("out", "in"),
                alias: Some("q0".to_owned()),
            }],
        })
        .expect("counter simulation failed");

    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected digital waveform");
    };
    let value = waveform
        .transitions
        .last()
        .map(|transition| transition.value)
        .unwrap_or(LogicValue::X);
    println!("counter q0 = {value:?}");
}
