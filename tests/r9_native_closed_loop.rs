use std::path::PathBuf;

use ontologyx_sim_core::{
    AdcCoSimulationParticipant, Circuit, CoSimulationConfig, CoSimulationEndpoint,
    CoSimulationLink, CoSimulationScheduler, CoSimulationTime, Component, ComponentKind,
    DacCoSimulationParticipant, ExecutionControl, LogicValue, ModelDefinition, Net, NetEndpoint,
    NgSpiceCoSimulationBinding, NgSpiceCoSimulationConfig, NgSpiceCoSimulationParticipant,
    ParameterValue, Pin, PinDirection, Quantity, RenodeCoSimulationBinding,
    RenodeCoSimulationConfig, RenodeCoSimulationParticipant, SignalDomain, Unit,
    VerilatorCoSimulationParticipant, Waveform,
};

fn required_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_R9_NATIVE_CLOSED_LOOP").is_some() => {
            panic!("{name} is required for the R9.4 native closed-loop proof")
        }
        _ => None,
    }
}

fn analog_pin(id: &str) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
}

fn ground() -> Component {
    Component::new("gnd", ComponentKind::ground()).with_pin(Pin::new(
        "gnd",
        "gnd",
        SignalDomain::Reference,
        PinDirection::Passive,
    ))
}

fn resistor(id: &str, ohms: f64) -> Component {
    Component::new(id, ComponentKind::resistor())
        .with_pin(analog_pin("a"))
        .with_pin(analog_pin("b"))
        .with_parameter(
            "resistance",
            ParameterValue::Quantity(Quantity::new(ohms, Unit::Ohm)),
        )
}

fn analog_path() -> Circuit {
    Circuit::new()
        .with_component(resistor("r1", 1_000.0))
        .with_component(resistor("r2", 1_000_000.0))
        .with_component(ground())
        .with_net(Net::new("vin").connect(NetEndpoint::new("r1", "a")))
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("r2", "a")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("r2", "b"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        )
}

fn digital_waveform<'a>(
    report: &'a ontologyx_sim_core::CoSimulationReport,
    signal: &str,
) -> &'a ontologyx_sim_core::DigitalWaveform {
    report
        .result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Digital(waveform) if waveform.signal.as_str() == signal => Some(waveform),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing digital waveform {signal}"))
}

fn analog_waveform<'a>(
    report: &'a ontologyx_sim_core::CoSimulationReport,
    signal: &str,
) -> &'a ontologyx_sim_core::AnalogWaveform {
    report
        .result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Analog(waveform) if waveform.signal.as_str() == signal => Some(waveform),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing analog waveform {signal}"))
}

#[test]
fn real_renode_verilator_sharedspice_loop_returns_feedback_to_firmware() {
    let Some(port) = required_env("ONTOLOGYX_SIM_R9_RENODE_PORT") else {
        eprintln!("skipping native R9.4 proof: ONTOLOGYX_SIM_R9_RENODE_PORT is unset");
        return;
    };
    let Some(renode_helper) = required_env("ONTOLOGYX_SIM_R9_RENODE_HELPER") else {
        eprintln!("skipping native R9.4 proof: ONTOLOGYX_SIM_R9_RENODE_HELPER is unset");
        return;
    };
    let Some(ngspice_helper) = required_env("ONTOLOGYX_SIM_R9_NGSPICE_HELPER") else {
        eprintln!("skipping native R9.4 proof: ONTOLOGYX_SIM_R9_NGSPICE_HELPER is unset");
        return;
    };

    let quantum = CoSimulationTime::from_picoseconds(50_000);
    let stop = CoSimulationTime::from_picoseconds(5_000_000);
    let renode = RenodeCoSimulationParticipant::new(
        "mcu",
        RenodeCoSimulationConfig::new(port, "oxsim")
            .with_helper(PathBuf::from(renode_helper))
            .with_quantum(quantum),
        vec![
            RenodeCoSimulationBinding::output("fw_out", "gpioPortD", 12),
            RenodeCoSimulationBinding::input("fw_in", "gpioPortD", 13),
        ],
    )
    .unwrap();
    let rtl = VerilatorCoSimulationParticipant::new(
        "rtl",
        ModelDefinition::system_verilog_module(
            "feedback_wire",
            "feedback_wire",
            "module feedback_wire(input logic a, output logic y); assign y = a; endmodule",
        ),
        vec![
            ontologyx_sim_core::CoSimulationPort::digital("a", PinDirection::Input),
            ontologyx_sim_core::CoSimulationPort::digital("y", PinDirection::Output),
        ],
    )
    .unwrap();
    let dac = DacCoSimulationParticipant::new("dac", 0.0, 3.3).unwrap();
    let analog = NgSpiceCoSimulationParticipant::new(
        "analog",
        analog_path(),
        NgSpiceCoSimulationConfig::new(stop, CoSimulationTime::from_picoseconds(1_000))
            .with_helper(PathBuf::from(ngspice_helper)),
        vec![
            NgSpiceCoSimulationBinding::input("vin", "vin"),
            NgSpiceCoSimulationBinding::output("vout", "out"),
        ],
    )
    .unwrap();
    let adc = AdcCoSimulationParticipant::new("adc", 0.8, 2.0).unwrap();

    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig::new(stop, quantum));
    scheduler.add_participant(renode).unwrap();
    scheduler.add_participant(rtl).unwrap();
    scheduler.add_participant(dac).unwrap();
    scheduler.add_participant(analog).unwrap();
    scheduler.add_participant(adc).unwrap();
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("mcu", "fw_out"),
        CoSimulationEndpoint::new("rtl", "a"),
    ));
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("rtl", "y"),
        CoSimulationEndpoint::new("dac", "digital_in"),
    ));
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("dac", "analog_out"),
        CoSimulationEndpoint::new("analog", "vin"),
    ));
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("analog", "vout"),
        CoSimulationEndpoint::new("adc", "analog_in"),
    ));
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("adc", "digital_out"),
        CoSimulationEndpoint::new("mcu", "fw_in"),
    ));

    let report = scheduler.run(&ExecutionControl::default()).unwrap();
    assert_eq!(report.scheduler.macro_steps, 100);
    assert!(report.scheduler.delta_cycles >= 4);

    let firmware_out = digital_waveform(&report, "mcu.fw_out");
    let feedback = digital_waveform(&report, "adc.digital_out");
    let analog = analog_waveform(&report, "analog.vout");

    assert!(
        firmware_out.transitions.len() >= 2,
        "firmware output never reacted to returned feedback: {firmware_out:?}"
    );
    assert!(
        feedback.transitions.len() >= 2,
        "ADC feedback never crossed both logic regions: {feedback:?}"
    );
    assert!(
        analog.values.iter().any(|value| *value <= 0.8),
        "analog path never reached the ADC low region: {:?}",
        analog.values
    );
    assert!(
        analog.values.iter().any(|value| *value >= 2.0),
        "analog path never reached the ADC high region: {:?}",
        analog.values
    );
    assert!(
        firmware_out
            .transitions
            .iter()
            .any(|transition| transition.value == LogicValue::Zero)
    );
    assert!(
        firmware_out
            .transitions
            .iter()
            .any(|transition| transition.value == LogicValue::One)
    );
}
