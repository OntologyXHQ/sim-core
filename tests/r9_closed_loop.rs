use ontologyx_sim_core::{
    AdcCoSimulationParticipant, Circuit, CoSimulationConfig, CoSimulationEndpoint,
    CoSimulationLink, CoSimulationParticipant, CoSimulationScheduler, CoSimulationTime,
    CoSimulationValue, Component, ComponentKind, DacCoSimulationParticipant, ExecutionControl,
    LogicValue, ModelDefinition, Net, NetEndpoint, NgSpiceCoSimulationBinding,
    NgSpiceCoSimulationConfig, NgSpiceCoSimulationParticipant, ParameterValue, Pin, PinDirection,
    Quantity, RenodeCoSimulationBinding, RenodeCoSimulationConfig, RenodeCoSimulationParticipant,
    SignalDomain, Unit, VerilatorCoSimulationParticipant, VerilatorEngine, Waveform,
};

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

fn external_analog_path() -> Circuit {
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

#[test]
fn r9_4_bridge_contracts_are_explicit_deterministic_and_fail_closed() {
    let control = ExecutionControl::default();
    let mut dac = DacCoSimulationParticipant::new("dac", 0.0, 3.3).unwrap();
    dac.initialize(&control).unwrap();
    assert_eq!(
        dac.read_output("analog_out").unwrap(),
        CoSimulationValue::Analog(0.0)
    );
    assert!(
        dac.write_input(
            "digital_in",
            CoSimulationTime::ZERO,
            &CoSimulationValue::Digital(LogicValue::One),
            &control,
        )
        .unwrap()
    );
    assert_eq!(
        dac.read_output("analog_out").unwrap(),
        CoSimulationValue::Analog(3.3)
    );
    let error = dac
        .write_input(
            "digital_in",
            CoSimulationTime::ZERO,
            &CoSimulationValue::Digital(LogicValue::X),
            &control,
        )
        .expect_err("X must be resolved before the two-state DAC boundary");
    assert_eq!(error.code(), "dac_cosim_two_state_input");

    let mut adc = AdcCoSimulationParticipant::new("adc", 0.8, 2.0).unwrap();
    adc.initialize(&control).unwrap();
    adc.write_input(
        "analog_in",
        CoSimulationTime::ZERO,
        &CoSimulationValue::Analog(2.5),
        &control,
    )
    .unwrap();
    assert_eq!(
        adc.read_output("digital_out").unwrap(),
        CoSimulationValue::Digital(LogicValue::One)
    );
    adc.write_input(
        "analog_in",
        CoSimulationTime::ZERO,
        &CoSimulationValue::Analog(1.4),
        &control,
    )
    .unwrap();
    assert_eq!(
        adc.read_output("digital_out").unwrap(),
        CoSimulationValue::Digital(LogicValue::One),
        "hysteresis band must preserve prior known output"
    );
    adc.write_input(
        "analog_in",
        CoSimulationTime::ZERO,
        &CoSimulationValue::Analog(0.4),
        &control,
    )
    .unwrap();
    assert_eq!(
        adc.read_output("digital_out").unwrap(),
        CoSimulationValue::Digital(LogicValue::Zero)
    );
}

#[test]
fn r9_4_feedback_firmware_fixture_is_self_contained_and_targets_pd12_pd13() {
    let firmware = include_bytes!("fixtures/stm32f4_cosim_feedback.elf");
    assert_eq!(&firmware[..4], b"\x7fELF");
    let source = include_str!("fixtures/stm32f4_cosim_feedback.c");
    assert!(source.contains("GPIOD_IDR"));
    assert!(source.contains("GPIOD_BSRR"));
    assert!(source.contains("1u << 13"));
    assert!(source.contains("1u << 12u"));
}

#[cfg(unix)]
mod unix_closed_loop {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempExecutable {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempExecutable {
        fn new(name: &str, body: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ontologyx-sim-core-r9-4-{}-{nonce}-{counter}",
                std::process::id()
            ));
            fs::create_dir(&dir).unwrap();
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
            Self { dir, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempExecutable {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn require_verilator() -> bool {
        let info = VerilatorEngine::default().info();
        eprintln!("using Verilator for R9.4: {info:?}");
        if info.available {
            return true;
        }
        if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_R9_CLOSED_LOOP").is_some() {
            panic!("Verilator is required for the R9.4 closed-loop proof");
        }
        eprintln!("skipping R9.4 portable closed-loop proof because verilator is unavailable");
        false
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

    #[test]
    fn scheduler_closes_firmware_rtl_analog_feedback_loop_on_one_timeline() {
        if !require_verilator() {
            return;
        }

        // This helper speaks the exact R9.3 Renode participant protocol. It models
        // firmware semantics only: on each virtual-time advance, PD12 becomes the
        // inverse of the externally driven PD13 input.
        let renode_helper = TempExecutable::new(
            "fake-renode-feedback-helper",
            r#"
time=0
output=1
input=0
printf 'STATE %s %s %s\n' "$time" "$output" "$input"
while IFS= read -r command; do
  case "$command" in
    'SET 1 0') input=0 ;;
    'SET 1 1') input=1 ;;
    'ADV '*)
      time=${command#ADV }
      if [ "$input" = 1 ]; then output=0; else output=1; fi
      ;;
    QUIT) exit 0 ;;
    *) exit 9 ;;
  esac
  printf 'STATE %s %s %s\n' "$time" "$output" "$input"
done
"#,
        );

        // This helper speaks the exact R9.3 SharedSpice protocol and represents
        // a unity-gain analog path. Native R9.4 replaces it with SharedSpice.
        let ngspice_helper = TempExecutable::new(
            "fake-ngspice-feedback-helper",
            r#"
time=0
output=0.0
printf 'STATE %s %s\n' "$time" "$output"
while IFS= read -r command; do
  case "$command" in
    'SET 0 '*)
      value=${command#SET 0 }
      case "$value" in
        0*|-0*) output=0.0 ;;
        *) output=3.3 ;;
      esac
      ;;
    'ADV '*) time=${command#ADV } ;;
    QUIT) exit 0 ;;
    *) exit 9 ;;
  esac
  printf 'STATE %s %s\n' "$time" "$output"
done
"#,
        );

        let renode = RenodeCoSimulationParticipant::new(
            "mcu",
            RenodeCoSimulationConfig::new("12345", "oxsim").with_helper(renode_helper.path()),
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
            external_analog_path(),
            NgSpiceCoSimulationConfig::new(
                CoSimulationTime::from_picoseconds(6_000),
                CoSimulationTime::from_picoseconds(1_000),
            )
            .with_helper(ngspice_helper.path()),
            vec![
                NgSpiceCoSimulationBinding::input("vin", "vin"),
                NgSpiceCoSimulationBinding::output("vout", "out"),
            ],
        )
        .unwrap();
        let adc = AdcCoSimulationParticipant::new("adc", 0.8, 2.0).unwrap();

        let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig::new(
            CoSimulationTime::from_picoseconds(6_000),
            CoSimulationTime::from_picoseconds(10_000),
        ));
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
        assert_eq!(report.scheduler.macro_steps, 6);
        assert!(report.scheduler.delta_cycles >= 4);

        let firmware_out = digital_waveform(&report, "mcu.fw_out");
        let feedback = digital_waveform(&report, "adc.digital_out");
        assert!(firmware_out.transitions.len() >= 6, "{firmware_out:?}");
        assert!(feedback.transitions.len() >= 6, "{feedback:?}");
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
        assert_eq!(
            feedback.transitions.last().unwrap().value,
            firmware_out.transitions.last().unwrap().value,
            "the settled analog feedback must follow the current firmware output"
        );
    }
}
