use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, LogicValue, ModelDefinition, Net, NetEndpoint,
    ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain, SimulationEngine,
    SimulationRequest, Unit, VerilatorEngine, Waveform,
};

fn digital_pin(id: &str, name: &str, direction: PinDirection) -> Pin {
    Pin::new(id, name, SignalDomain::Digital, direction)
}

fn require_verilator() -> Option<VerilatorEngine> {
    let engine = VerilatorEngine::default();
    let info = engine.info();
    eprintln!("using Verilator: {info:?}");
    if info.available {
        return Some(engine);
    }
    if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_VERILATOR").is_some() {
        panic!("Verilator is required but was not discovered on PATH");
    }
    eprintln!("skipping real Verilator proof because verilator is unavailable");
    None
}

#[test]
fn real_verilator_runs_systemverilog_clocked_module() {
    let Some(engine) = require_verilator() else {
        return;
    };
    let model = ModelDefinition::system_verilog_module(
        "toggle",
        "toggle",
        r#"module toggle(input logic clk, output logic q);
  always_ff @(posedge clk) q <= ~q;
endmodule"#,
    );
    let clock = Component::new("clk", ComponentKind::digital_clock())
        .with_pin(digital_pin("out", "out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(10e-9, Unit::Second)),
        )
        .with_parameter("initial", ParameterValue::Integer(0));
    let dut = Component::new("dut", ComponentKind::hdl_module())
        .with_pin(digital_pin("clk", "clk", PinDirection::Input))
        .with_pin(digital_pin("q", "q", PinDirection::Output))
        .with_parameter("model", ParameterValue::Text("toggle".into()));
    let sink = Component::new("sink", ComponentKind::logic_output()).with_pin(digital_pin(
        "in",
        "in",
        PinDirection::Input,
    ));
    let circuit = Circuit::new()
        .with_model(model)
        .with_component(clock)
        .with_component(dut)
        .with_component(sink)
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("dut", "clk")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("dut", "q"))
                .connect(NetEndpoint::new("sink", "in")),
        );
    let result = engine
        .simulate(&SimulationRequest {
            circuit,
            analysis: Analysis::DigitalTransient { stop: 31e-9 },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("dut", "q"),
                alias: Some("q".into()),
            }],
        })
        .unwrap();
    let waveform = result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Digital(waveform) if waveform.signal.as_str() == "q" => Some(waveform),
            _ => None,
        })
        .expect("q digital waveform");
    assert!(
        waveform.transitions.len() >= 4,
        "{:#?}",
        waveform.transitions
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
            .any(|transition| transition.value == LogicValue::Zero)
    );
}

#[test]
fn real_verilator_runs_plain_verilog_module() {
    let Some(engine) = require_verilator() else {
        return;
    };
    let model = ModelDefinition::verilog_module(
        "and2",
        "and2",
        "module and2(input a, input b, output y); assign y = a & b; endmodule",
    );
    let a = Component::new("a", ComponentKind::logic_input())
        .with_pin(digital_pin("out", "out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(1));
    let b = Component::new("b", ComponentKind::logic_input())
        .with_pin(digital_pin("out", "out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(1));
    let dut = Component::new("dut", ComponentKind::hdl_module())
        .with_pin(digital_pin("a", "a", PinDirection::Input))
        .with_pin(digital_pin("b", "b", PinDirection::Input))
        .with_pin(digital_pin("y", "y", PinDirection::Output))
        .with_parameter("model", ParameterValue::Text("and2".into()));
    let circuit = Circuit::new()
        .with_model(model)
        .with_component(a)
        .with_component(b)
        .with_component(dut)
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("dut", "a")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("dut", "b")),
        )
        .with_net(Net::new("y").connect(NetEndpoint::new("dut", "y")));
    let result = engine
        .simulate(&SimulationRequest {
            circuit,
            analysis: Analysis::DigitalTransient { stop: 1e-9 },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("dut", "y"),
                alias: Some("y".into()),
            }],
        })
        .unwrap();
    let waveform = result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Digital(waveform) if waveform.signal.as_str() == "y" => Some(waveform),
            _ => None,
        })
        .expect("y digital waveform");
    assert_eq!(waveform.transitions.last().unwrap().value, LogicValue::One);
}
