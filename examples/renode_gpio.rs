use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, FirmwareArtifact, Net, NetEndpoint, Pin,
    PinDirection, Probe, RenodeEngine, RenodeTarget, SignalDomain, SimulationEngine,
    SimulationRequest, Waveform,
};

fn main() -> Result<(), ontologyx_sim_core::EngineError> {
    let target = RenodeTarget::new(
        "mcu",
        "platforms/boards/stm32f4_discovery-kit.repl",
        FirmwareArtifact::elf(include_bytes!("../tests/fixtures/stm32f4_gpio_toggle.elf").to_vec()),
    )
    .with_gpio_observer("gpioPortD@12", "sysbus.gpioPortD.UserLED");
    let engine = RenodeEngine::new(target);
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
    let request = SimulationRequest {
        circuit: Circuit::new()
            .with_component(mcu)
            .with_component(sink)
            .with_net(
                Net::new("gpio")
                    .connect(NetEndpoint::new("mcu", "pd12"))
                    .connect(NetEndpoint::new("led", "in")),
            ),
        analysis: Analysis::FirmwareTransient {
            step: 50e-6,
            stop: 2e-3,
        },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("led", "in"),
            alias: Some("pd12".into()),
        }],
    };
    let result = engine.simulate(&request)?;
    if let Some(Waveform::Digital(waveform)) = result.waveforms.first() {
        println!("firmware pd12: {} transitions", waveform.transitions.len());
    }
    Ok(())
}
