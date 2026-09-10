use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, LogicValue, MixedSignalEngine, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine,
    SimulationRequest, Unit, Waveform,
};

fn analog_pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, direction)
}

fn digital_pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn request() -> SimulationRequest {
    let source = Component::new("vin", ComponentKind::voltage_source())
        .with_pin(analog_pin("pos", PinDirection::Passive))
        .with_pin(analog_pin("neg", PinDirection::Passive))
        .with_parameter(
            "pulse_low",
            ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
        )
        .with_parameter(
            "pulse_high",
            ParameterValue::Quantity(Quantity::new(3.3, Unit::Volt)),
        )
        .with_parameter(
            "pulse_delay",
            ParameterValue::Quantity(Quantity::new(1e-6, Unit::Second)),
        )
        .with_parameter(
            "pulse_rise",
            ParameterValue::Quantity(Quantity::new(1e-9, Unit::Second)),
        )
        .with_parameter(
            "pulse_fall",
            ParameterValue::Quantity(Quantity::new(1e-9, Unit::Second)),
        )
        .with_parameter(
            "pulse_width",
            ParameterValue::Quantity(Quantity::new(4e-6, Unit::Second)),
        )
        .with_parameter(
            "pulse_period",
            ParameterValue::Quantity(Quantity::new(8e-6, Unit::Second)),
        );
    let ground = Component::new("gnd", ComponentKind::ground())
        .with_pin(analog_pin("gnd", PinDirection::Passive));
    let adc = Component::new("adc", ComponentKind::adc_bridge())
        .with_pin(analog_pin("analog_in", PinDirection::Input))
        .with_pin(digital_pin("digital_out", PinDirection::Output))
        .with_parameter(
            "low_threshold",
            ParameterValue::Quantity(Quantity::new(0.8, Unit::Volt)),
        )
        .with_parameter(
            "high_threshold",
            ParameterValue::Quantity(Quantity::new(2.0, Unit::Volt)),
        )
        .with_parameter(
            "rise_delay",
            ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
        )
        .with_parameter(
            "fall_delay",
            ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
        );
    let inverter = Component::new("inv", ComponentKind::not_gate())
        .with_pin(digital_pin("in", PinDirection::Input))
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "delay",
            ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
        );
    let dac = Component::new("dac", ComponentKind::dac_bridge())
        .with_pin(digital_pin("digital_in", PinDirection::Input))
        .with_pin(analog_pin("analog_out", PinDirection::Output))
        .with_parameter(
            "low_voltage",
            ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
        )
        .with_parameter(
            "high_voltage",
            ParameterValue::Quantity(Quantity::new(3.3, Unit::Volt)),
        )
        .with_parameter(
            "rise_time",
            ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
        )
        .with_parameter(
            "fall_time",
            ParameterValue::Quantity(Quantity::new(2e-9, Unit::Second)),
        );
    let load = Component::new("load", ComponentKind::resistor())
        .with_pin(analog_pin("a", PinDirection::Passive))
        .with_pin(analog_pin("b", PinDirection::Passive))
        .with_parameter(
            "resistance",
            ParameterValue::Quantity(Quantity::new(10_000.0, Unit::Ohm)),
        );

    let circuit = Circuit::new()
        .with_component(source)
        .with_component(ground)
        .with_component(adc)
        .with_component(inverter)
        .with_component(dac)
        .with_component(load)
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
            Net::new("adc_logic")
                .connect(NetEndpoint::new("adc", "digital_out"))
                .connect(NetEndpoint::new("inv", "in")),
        )
        .with_net(
            Net::new("inv_logic")
                .connect(NetEndpoint::new("inv", "out"))
                .connect(NetEndpoint::new("dac", "digital_in")),
        )
        .with_net(
            Net::new("vout")
                .connect(NetEndpoint::new("dac", "analog_out"))
                .connect(NetEndpoint::new("load", "a")),
        );

    SimulationRequest {
        circuit,
        analysis: Analysis::MixedSignalTransient {
            step: 10e-9,
            stop: 12e-6,
        },
        probes: vec![
            Probe {
                endpoint: NetEndpoint::new("adc", "digital_out"),
                alias: Some("adc_logic".to_owned()),
            },
            Probe {
                endpoint: NetEndpoint::new("inv", "out"),
                alias: Some("inv_logic".to_owned()),
            },
            Probe {
                endpoint: NetEndpoint::new("dac", "analog_out"),
                alias: Some("vout".to_owned()),
            },
        ],
    }
}

fn require_engine() -> Option<MixedSignalEngine> {
    let engine = MixedSignalEngine::default();
    let info = engine.info();
    if info.available && info.xspice_available {
        eprintln!("using mixed-signal XSPICE: {info:?}");
        return Some(engine);
    }
    if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_MIXED_SIGNAL").is_some() {
        panic!("mixed-signal ngspice/XSPICE is required but unavailable: {info:?}");
    }
    eprintln!("mixed-signal ngspice/XSPICE unavailable; integration check skipped: {info:?}");
    None
}

#[test]
fn real_mixed_signal_round_trip_produces_analog_and_digital_waveforms() {
    let Some(engine) = require_engine() else {
        return;
    };
    let result = engine
        .simulate(&request())
        .expect("mixed-signal transient should succeed");
    assert_eq!(result.engine.as_str(), "mixed-signal-xspice");
    assert_eq!(
        result.analysis,
        ontologyx_sim_core::AnalysisKind::MixedSignalTransient
    );

    let mut analog_seen = false;
    let mut adc_values = Vec::new();
    let mut inverter_values = Vec::new();
    for waveform in &result.waveforms {
        match waveform {
            Waveform::Analog(waveform) if waveform.signal.as_str() == "vout" => {
                analog_seen = true;
                let min = waveform
                    .values
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min);
                let max = waveform
                    .values
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                assert!(
                    min < 0.5,
                    "DAC output never reached logic-low voltage: {min}"
                );
                assert!(
                    max > 2.5,
                    "DAC output never reached logic-high voltage: {max}"
                );
            }
            Waveform::Digital(waveform) if waveform.signal.as_str() == "adc_logic" => {
                adc_values.extend(
                    waveform
                        .transitions
                        .iter()
                        .map(|transition| transition.value),
                );
            }
            Waveform::Digital(waveform) if waveform.signal.as_str() == "inv_logic" => {
                inverter_values.extend(
                    waveform
                        .transitions
                        .iter()
                        .map(|transition| transition.value),
                );
            }
            _ => {}
        }
    }
    assert!(analog_seen);
    assert!(adc_values.contains(&LogicValue::Zero));
    assert!(adc_values.contains(&LogicValue::One));
    assert!(inverter_values.contains(&LogicValue::Zero));
    assert!(inverter_values.contains(&LogicValue::One));
}
