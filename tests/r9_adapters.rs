use ontologyx_sim_core::{
    Analysis, Circuit, CoSimulationConfig, CoSimulationEndpoint, CoSimulationLink,
    CoSimulationParticipant, CoSimulationPort, CoSimulationScheduler, CoSimulationTime,
    CoSimulationValue, Component, ComponentKind, DigitalCoSimulationBinding,
    DigitalCoSimulationParticipant, ExecutionControl, LogicValue, ModelDefinition, Net,
    NetEndpoint, ParameterValue, Pin, PinDirection, Quantity, SignalDomain, SimulationRequest,
    Unit, VerilatorCoSimulationParticipant, VerilatorEngine, Waveform,
};

fn digital_pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn clock_request(stop: f64) -> SimulationRequest {
    let clock = Component::new("clock", ComponentKind::digital_clock())
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(10e-9, Unit::Second)),
        )
        .with_parameter("duty_cycle", ParameterValue::Number(0.5))
        .with_parameter("initial", ParameterValue::Integer(0));
    SimulationRequest {
        circuit: Circuit::new()
            .with_component(clock)
            .with_net(Net::new("clk").connect(NetEndpoint::new("clock", "out"))),
        analysis: Analysis::DigitalTransient { stop },
        probes: vec![],
    }
}

fn inverter_request(stop: f64) -> SimulationRequest {
    let inverter = Component::new("inv", ComponentKind::not_gate())
        .with_pin(digital_pin("in", PinDirection::Input))
        .with_pin(digital_pin("out", PinDirection::Output));
    SimulationRequest {
        circuit: Circuit::new()
            .with_component(inverter)
            .with_net(Net::new("in").connect(NetEndpoint::new("inv", "in")))
            .with_net(Net::new("out").connect(NetEndpoint::new("inv", "out"))),
        analysis: Analysis::DigitalTransient { stop },
        probes: vec![],
    }
}

#[test]
fn digital_participant_reuses_r3_four_state_event_runtime() {
    let mut participant = DigitalCoSimulationParticipant::new(
        "digital",
        inverter_request(20e-9),
        vec![
            DigitalCoSimulationBinding::input("in", "in"),
            DigitalCoSimulationBinding::output("out", "out"),
        ],
    )
    .unwrap();
    let control = ExecutionControl::default();
    participant.initialize(&control).unwrap();

    assert!(
        participant
            .write_input(
                "in",
                CoSimulationTime::ZERO,
                &CoSimulationValue::Digital(LogicValue::One),
                &control,
            )
            .unwrap()
    );
    assert_eq!(
        participant.read_output("out").unwrap(),
        CoSimulationValue::Digital(LogicValue::Zero)
    );

    assert!(
        participant
            .write_input(
                "in",
                CoSimulationTime::ZERO,
                &CoSimulationValue::Digital(LogicValue::Z),
                &control,
            )
            .unwrap()
    );
    assert_eq!(
        participant.read_output("out").unwrap(),
        CoSimulationValue::Digital(LogicValue::X)
    );
}

#[test]
fn digital_participant_rejects_external_input_with_internal_driver() {
    let source = Component::new("source", ComponentKind::logic_input())
        .with_pin(digital_pin("out", PinDirection::Output))
        .with_parameter("value", ParameterValue::Integer(1));
    let circuit = Circuit::new()
        .with_component(source)
        .with_net(Net::new("in").connect(NetEndpoint::new("source", "out")));
    let error = DigitalCoSimulationParticipant::new(
        "bad",
        SimulationRequest {
            circuit,
            analysis: Analysis::DigitalTransient { stop: 1e-9 },
            probes: vec![],
        },
        vec![DigitalCoSimulationBinding::input("in", "in")],
    )
    .err()
    .expect("internally driven external input must fail");
    assert_eq!(error.code, "digital_cosim_input_driven");
}

#[test]
fn verilator_participant_contract_is_scalar_digital_and_two_state() {
    let model = ModelDefinition::system_verilog_module(
        "not1",
        "not1",
        "module not1(input logic a, output logic y); assign y = ~a; endmodule",
    );
    let participant = VerilatorCoSimulationParticipant::new(
        "rtl",
        model.clone(),
        vec![
            CoSimulationPort::digital("a", PinDirection::Input),
            CoSimulationPort::digital("y", PinDirection::Output),
        ],
    );
    assert!(participant.is_ok());

    let error = VerilatorCoSimulationParticipant::new(
        "rtl",
        model,
        vec![CoSimulationPort::analog(
            "a",
            PinDirection::Input,
            Unit::Volt,
        )],
    )
    .err()
    .expect("analog direct port must fail");
    assert_eq!(error.code, "verilator_cosim_port_invalid");
}

fn require_verilator() -> bool {
    let info = VerilatorEngine::default().info();
    eprintln!("using Verilator for R9.2: {info:?}");
    if info.available {
        return true;
    }
    if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_R9_VERILATOR").is_some() {
        panic!("Verilator is required for the R9.2 real co-simulation proof");
    }
    eprintln!("skipping R9.2 real Verilator proof because verilator is unavailable");
    false
}

#[test]
fn real_scheduler_chains_digital_clock_verilator_state_and_digital_logic() {
    if !require_verilator() {
        return;
    }
    let stop = 30e-9;
    let clock = DigitalCoSimulationParticipant::new(
        "clock",
        clock_request(stop),
        vec![DigitalCoSimulationBinding::output("out", "clk")],
    )
    .unwrap();
    let rtl = VerilatorCoSimulationParticipant::new(
        "rtl",
        ModelDefinition::system_verilog_module(
            "toggle",
            "toggle",
            "module toggle(input logic clk, output logic q); always_ff @(posedge clk) q <= ~q; endmodule",
        ),
        vec![
            CoSimulationPort::digital("clk", PinDirection::Input),
            CoSimulationPort::digital("q", PinDirection::Output),
        ],
    )
    .unwrap();
    let inverter = DigitalCoSimulationParticipant::new(
        "sink",
        inverter_request(stop),
        vec![
            DigitalCoSimulationBinding::input("in", "in"),
            DigitalCoSimulationBinding::output("out", "out"),
        ],
    )
    .unwrap();

    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig::new(
        CoSimulationTime::from_picoseconds(30_000),
        CoSimulationTime::from_picoseconds(20_000),
    ));
    scheduler.add_participant(clock).unwrap();
    scheduler.add_participant(rtl).unwrap();
    scheduler.add_participant(inverter).unwrap();
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("clock", "out"),
        CoSimulationEndpoint::new("rtl", "clk"),
    ));
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("rtl", "q"),
        CoSimulationEndpoint::new("sink", "in"),
    ));

    let report = scheduler.run(&ExecutionControl::default()).unwrap();
    let q = report
        .result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Digital(waveform) if waveform.signal.as_str() == "rtl.q" => Some(waveform),
            _ => None,
        })
        .expect("rtl.q waveform");
    let out = report
        .result
        .waveforms
        .iter()
        .find_map(|waveform| match waveform {
            Waveform::Digital(waveform) if waveform.signal.as_str() == "sink.out" => Some(waveform),
            _ => None,
        })
        .expect("sink.out waveform");

    assert!(q.transitions.iter().any(|transition| transition.time > 0.0));
    assert!(
        q.transitions
            .iter()
            .any(|transition| transition.value == LogicValue::One)
    );
    assert!(
        q.transitions
            .iter()
            .any(|transition| transition.value == LogicValue::Zero)
    );
    assert_eq!(
        out.transitions.last().unwrap().value,
        q.transitions.last().unwrap().value.logic_not()
    );
}
