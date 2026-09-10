use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, FirmwareArtifact, LogicValue, Net, NetEndpoint,
    Pin, PinDirection, Probe, RENODE_ENGINE_ID, RenodeEngine, RenodeTarget, SignalDomain,
    SimulationEngine, SimulationRequest, Waveform,
};

fn require_renode(target: RenodeTarget) -> Option<RenodeEngine> {
    let engine = RenodeEngine::new(target);
    let info = engine.info();
    eprintln!("using Renode: {info:?}");
    if info.available {
        return Some(engine);
    }
    if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_RENODE").is_some() {
        panic!("Renode is required but was not discovered on PATH");
    }
    eprintln!("skipping real Renode proof because renode is unavailable");
    None
}

#[test]
fn real_renode_runs_cortex_m_firmware_and_observes_gpio() {
    let firmware =
        FirmwareArtifact::elf(include_bytes!("fixtures/stm32f4_gpio_toggle.elf").to_vec());
    let target = RenodeTarget::new(
        "mcu",
        "platforms/boards/stm32f4_discovery-kit.repl",
        firmware,
    )
    .with_gpio_observer("gpioPortD@12", "sysbus.gpioPortD.UserLED");
    let Some(engine) = require_renode(target) else {
        return;
    };

    let mcu = Component::new("mcu", ComponentKind::mcu()).with_pin(Pin::new(
        "pd12",
        "gpioPortD@12",
        SignalDomain::Digital,
        PinDirection::Output,
    ));
    let sink = Component::new("led", ComponentKind::logic_output()).with_pin(Pin::new(
        "in",
        "in",
        SignalDomain::Digital,
        PinDirection::Input,
    ));
    let circuit = Circuit::new()
        .with_component(mcu)
        .with_component(sink)
        .with_net(
            Net::new("gpio")
                .connect(NetEndpoint::new("mcu", "pd12"))
                .connect(NetEndpoint::new("led", "in")),
        );

    let result = engine
        .simulate(&SimulationRequest {
            circuit,
            analysis: Analysis::FirmwareTransient {
                step: 50e-6,
                stop: 2e-3,
            },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("led", "in"),
                alias: Some("pd12".into()),
            }],
        })
        .unwrap();

    assert_eq!(result.engine.as_str(), RENODE_ENGINE_ID);
    let waveform = result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Digital(waveform) if waveform.signal.as_str() == "pd12" => Some(waveform),
            _ => None,
        })
        .expect("PD12 digital waveform");
    assert!(
        waveform.transitions.len() >= 2,
        "{:#?}",
        waveform.transitions
    );
    assert!(
        waveform
            .transitions
            .iter()
            .any(|transition| transition.value == LogicValue::Zero)
    );
    assert!(
        waveform
            .transitions
            .iter()
            .any(|transition| transition.value == LogicValue::One)
    );
    assert!(
        waveform
            .transitions
            .iter()
            .all(|transition| transition.time <= 2e-3)
    );
}
