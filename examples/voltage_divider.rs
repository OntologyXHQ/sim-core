use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, Net, NetEndpoint, NgSpiceEngine, ParameterValue,
    Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine, SimulationRequest, Unit,
    Waveform,
};

fn analog_pin(id: &str) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
}

fn main() {
    let engine = NgSpiceEngine::default();
    if !engine.info().available {
        eprintln!("ngspice is required to run this example");
        std::process::exit(2);
    }

    let source = Component::new("v1", ComponentKind::voltage_source())
        .with_pin(analog_pin("pos"))
        .with_pin(analog_pin("neg"))
        .with_parameter(
            "dc",
            ParameterValue::Quantity(Quantity::new(5.0, Unit::Volt)),
        );
    let r1 = Component::new("r1", ComponentKind::resistor())
        .with_pin(analog_pin("a"))
        .with_pin(analog_pin("b"))
        .with_parameter(
            "resistance",
            ParameterValue::Quantity(Quantity::new(1000.0, Unit::Ohm)),
        );
    let r2 = Component::new("r2", ComponentKind::resistor())
        .with_pin(analog_pin("a"))
        .with_pin(analog_pin("b"))
        .with_parameter(
            "resistance",
            ParameterValue::Quantity(Quantity::new(1000.0, Unit::Ohm)),
        );
    let ground = Component::new("gnd", ComponentKind::ground()).with_pin(Pin::new(
        "gnd",
        "gnd",
        SignalDomain::Reference,
        PinDirection::Passive,
    ));

    let circuit = Circuit::new()
        .with_component(source)
        .with_component(r1)
        .with_component(r2)
        .with_component(ground)
        .with_net(
            Net::new("vin")
                .connect(NetEndpoint::new("v1", "pos"))
                .connect(NetEndpoint::new("r1", "a")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("r2", "a")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("v1", "neg"))
                .connect(NetEndpoint::new("r2", "b"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        );

    let request = SimulationRequest {
        circuit,
        analysis: Analysis::OperatingPoint,
        probes: vec![Probe {
            endpoint: NetEndpoint::new("r1", "b"),
            alias: Some("vout".to_owned()),
        }],
    };

    let result = engine
        .simulate(&request)
        .expect("voltage-divider simulation failed");
    let Waveform::Analog(waveform) = &result.waveforms[0] else {
        panic!("expected an analog waveform");
    };
    println!("vout = {:.6} V", waveform.values[0]);
}
