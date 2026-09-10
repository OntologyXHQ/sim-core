use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, ModelDefinition, Net, NetEndpoint, ParameterValue,
    Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine, SimulationRequest, Unit,
    VerilatorEngine, Waveform,
};

fn digital_pin(id: &str, name: &str, direction: PinDirection) -> Pin {
    Pin::new(id, name, SignalDomain::Digital, direction)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = ModelDefinition::system_verilog_module(
        "counter",
        "counter",
        r#"module counter(input logic clk, output logic [1:0] q);
  always_ff @(posedge clk) q <= q + 2'b01;
endmodule"#,
    );
    let clock = Component::new("clock", ComponentKind::digital_clock())
        .with_pin(digital_pin("out", "out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(10e-9, Unit::Second)),
        );
    let dut = Component::new("dut", ComponentKind::hdl_module())
        .with_pin(digital_pin("clk", "clk", PinDirection::Input))
        .with_pin(digital_pin("q0", "q[0]", PinDirection::Output))
        .with_pin(digital_pin("q1", "q[1]", PinDirection::Output))
        .with_parameter("model", ParameterValue::Text("counter".into()));
    let circuit = Circuit::new()
        .with_model(model)
        .with_component(clock)
        .with_component(dut)
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clock", "out"))
                .connect(NetEndpoint::new("dut", "clk")),
        )
        .with_net(Net::new("q0").connect(NetEndpoint::new("dut", "q0")))
        .with_net(Net::new("q1").connect(NetEndpoint::new("dut", "q1")));
    let result = VerilatorEngine::default().simulate(&SimulationRequest {
        circuit,
        analysis: Analysis::DigitalTransient { stop: 31e-9 },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("dut", "q0"),
            alias: Some("counter.q0".into()),
        }],
    })?;
    for waveform in result.waveforms {
        if let Waveform::Digital(waveform) = waveform {
            println!(
                "{}: {} transitions",
                waveform.signal.as_str(),
                waveform.transitions.len()
            );
        }
    }
    Ok(())
}
