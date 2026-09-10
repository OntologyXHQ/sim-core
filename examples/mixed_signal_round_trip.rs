use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, MixedSignalEngine, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine,
    SimulationRequest, Unit, Waveform,
};

fn pin(id: &str, domain: SignalDomain, direction: PinDirection) -> Pin {
    Pin::new(id, id, domain, direction)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let circuit = Circuit::new()
        .with_component(
            Component::new("vin", ComponentKind::voltage_source())
                .with_pin(pin("pos", SignalDomain::Analog, PinDirection::Passive))
                .with_pin(pin("neg", SignalDomain::Analog, PinDirection::Passive))
                .with_parameter(
                    "pulse_high",
                    ParameterValue::Quantity(Quantity::new(3.3, Unit::Volt)),
                )
                .with_parameter(
                    "pulse_delay",
                    ParameterValue::Quantity(Quantity::new(1e-6, Unit::Second)),
                )
                .with_parameter(
                    "pulse_width",
                    ParameterValue::Quantity(Quantity::new(4e-6, Unit::Second)),
                )
                .with_parameter(
                    "pulse_period",
                    ParameterValue::Quantity(Quantity::new(8e-6, Unit::Second)),
                ),
        )
        .with_component(Component::new("gnd", ComponentKind::ground()).with_pin(pin(
            "gnd",
            SignalDomain::Analog,
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
            Component::new("inv", ComponentKind::not_gate())
                .with_pin(pin("in", SignalDomain::Digital, PinDirection::Input))
                .with_pin(pin("out", SignalDomain::Digital, PinDirection::Output)),
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
        .with_component(
            Component::new("load", ComponentKind::resistor())
                .with_pin(pin("a", SignalDomain::Analog, PinDirection::Passive))
                .with_pin(pin("b", SignalDomain::Analog, PinDirection::Passive))
                .with_parameter(
                    "resistance",
                    ParameterValue::Quantity(Quantity::new(10_000.0, Unit::Ohm)),
                ),
        )
        .with_net(
            Net::new("vin")
                .connect(NetEndpoint::new("vin", "pos"))
                .connect(NetEndpoint::new("adc", "analog_in")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("vin", "neg"))
                .connect(NetEndpoint::new("gnd", "gnd"))
                .connect(NetEndpoint::new("load", "b")),
        )
        .with_net(
            Net::new("logic_in")
                .connect(NetEndpoint::new("adc", "digital_out"))
                .connect(NetEndpoint::new("inv", "in")),
        )
        .with_net(
            Net::new("logic_out")
                .connect(NetEndpoint::new("inv", "out"))
                .connect(NetEndpoint::new("dac", "digital_in")),
        )
        .with_net(
            Net::new("vout")
                .connect(NetEndpoint::new("dac", "analog_out"))
                .connect(NetEndpoint::new("load", "a")),
        );

    let request = SimulationRequest {
        circuit,
        analysis: Analysis::MixedSignalTransient {
            step: 10e-9,
            stop: 12e-6,
        },
        probes: vec![
            Probe {
                endpoint: NetEndpoint::new("adc", "digital_out"),
                alias: Some("logic_in".to_owned()),
            },
            Probe {
                endpoint: NetEndpoint::new("dac", "analog_out"),
                alias: Some("vout".to_owned()),
            },
        ],
    };

    let result = MixedSignalEngine::default().simulate(&request)?;
    for waveform in result.waveforms {
        match waveform {
            Waveform::Analog(waveform) => println!(
                "analog {}: {} points",
                waveform.signal.as_str(),
                waveform.values.len()
            ),
            Waveform::Digital(waveform) => println!(
                "digital {}: {} transitions",
                waveform.signal.as_str(),
                waveform.transitions.len()
            ),
        }
    }
    Ok(())
}
