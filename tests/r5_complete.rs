use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, DigitalEngine, EngineRegistry, ModelDefinition,
    ModelKind, ModelLanguage, Net, NetEndpoint, ParameterValue, Pin, PinDirection, Probe,
    SignalDomain, SimulationRequest, VERILATOR_ENGINE_ID, VerilatorEngine, validate_circuit,
};

fn digital_pin(id: &str, name: &str, direction: PinDirection) -> Pin {
    Pin::new(id, name, SignalDomain::Digital, direction)
}

fn hdl_request() -> SimulationRequest {
    let model = ModelDefinition::system_verilog_module(
        "inv",
        "inv",
        "module inv(input logic a, output logic y); assign y = ~a; endmodule",
    );
    let input = Component::new("input", ComponentKind::logic_input())
        .with_pin(digital_pin("out", "out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(1));
    let dut = Component::new("dut", ComponentKind::hdl_module())
        .with_pin(digital_pin("a", "a", PinDirection::Input))
        .with_pin(digital_pin("y", "y", PinDirection::Output))
        .with_parameter("model", ParameterValue::Text("inv".into()));
    let output = Component::new("output", ComponentKind::logic_output()).with_pin(digital_pin(
        "in",
        "in",
        PinDirection::Input,
    ));
    let circuit = Circuit::new()
        .with_model(model)
        .with_component(input)
        .with_component(dut)
        .with_component(output)
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("input", "out"))
                .connect(NetEndpoint::new("dut", "a")),
        )
        .with_net(
            Net::new("y")
                .connect(NetEndpoint::new("dut", "y"))
                .connect(NetEndpoint::new("output", "in")),
        );
    SimulationRequest {
        circuit,
        analysis: Analysis::DigitalTransient { stop: 1e-6 },
        probes: vec![Probe {
            endpoint: NetEndpoint::new("dut", "y"),
            alias: Some("y".into()),
        }],
    }
}

#[test]
fn r5_hdl_model_contract_round_trips() {
    let model = ModelDefinition::verilog_module(
        "counter",
        "counter",
        "module counter(input clk, output reg q); endmodule",
    );
    assert_eq!(model.kind, ModelKind::Module);
    assert_eq!(model.language, ModelLanguage::Verilog);
    let json = serde_json::to_string(&model).unwrap();
    let restored: ModelDefinition = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, model);
}

#[test]
fn valid_hdl_module_is_admitted_by_circuit_validation() {
    let request = hdl_request();
    let report = validate_circuit(&request.circuit);
    assert!(report.is_valid(), "{:#?}", report.issues);
}

#[test]
fn hdl_module_rejects_non_digital_pin_domain() {
    let model = ModelDefinition::system_verilog_module(
        "bad",
        "bad",
        "module bad(input logic a); endmodule",
    );
    let dut = Component::new("dut", ComponentKind::hdl_module())
        .with_pin(Pin::new(
            "a",
            "a",
            SignalDomain::Analog,
            PinDirection::Input,
        ))
        .with_parameter("model", ParameterValue::Text("bad".into()));
    let report = validate_circuit(&Circuit::new().with_model(model).with_component(dut));
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "hdl_pin_domain")
    );
}

#[test]
fn request_aware_engine_selection_routes_hdl_to_verilator() {
    let request = hdl_request();
    let mut registry = EngineRegistry::new();
    registry.register(DigitalEngine::new());
    registry.register(VerilatorEngine::default());
    let selected = registry.select_request(&request).expect("R5 HDL engine");
    assert_eq!(selected.id().as_str(), VERILATOR_ENGINE_ID);
}
