use ontologyx_sim_core::{
    Analysis, Circuit, Component, ComponentKind, DigitalEngine, LogicValue, LogicVector, Net,
    NetEndpoint, ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain,
    SimulationEngine, SimulationRequest, Unit, Waveform,
};

fn pin(id: &str, direction: PinDirection) -> Pin {
    Pin::new(id, id, SignalDomain::Digital, direction)
}

fn input(id: &str, value: LogicValue) -> Component {
    let parameter = match value {
        LogicValue::Zero => ParameterValue::Integer(0),
        LogicValue::One => ParameterValue::Integer(1),
        LogicValue::X => ParameterValue::Text("x".to_owned()),
        LogicValue::Z => ParameterValue::Text("z".to_owned()),
    };
    Component::new(id, ComponentKind::logic_input())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter("value", parameter)
}

fn clock(id: &str, period: f64) -> Component {
    Component::new(id, ComponentKind::digital_clock())
        .with_pin(pin("out", PinDirection::Output))
        .with_parameter(
            "period",
            ParameterValue::Quantity(Quantity::new(period, Unit::Second)),
        )
        .with_parameter("duty_cycle", ParameterValue::Number(0.5))
        .with_parameter("initial", ParameterValue::Integer(0))
}

fn sink(id: &str) -> Component {
    Component::new(id, ComponentKind::logic_output()).with_pin(pin("in", PinDirection::Input))
}

fn request(circuit: Circuit, endpoint: NetEndpoint, stop: f64) -> SimulationRequest {
    SimulationRequest {
        circuit,
        analysis: Analysis::DigitalTransient { stop },
        probes: vec![Probe {
            endpoint,
            alias: Some("out".to_owned()),
        }],
    }
}

fn last_value(result: &ontologyx_sim_core::SimulationResult) -> LogicValue {
    let Waveform::Digital(waveform) = &result.waveforms[0] else {
        panic!("expected digital waveform");
    };
    waveform.transitions.last().unwrap().value
}

fn run(circuit: Circuit, endpoint: NetEndpoint, stop: f64) -> LogicValue {
    last_value(
        &DigitalEngine::new()
            .simulate(&request(circuit, endpoint, stop))
            .unwrap(),
    )
}

fn q_component(id: &str, kind: ComponentKind, inputs: &[&str]) -> Component {
    let mut component = Component::new(id, kind);
    for input in inputs {
        component = component.with_pin(pin(input, PinDirection::Input));
    }
    component
        .with_pin(pin("q", PinDirection::Output))
        .with_pin(pin("nq", PinDirection::Output))
}

#[test]
fn logic_vector_round_trips_known_bus_values() {
    let vector = LogicVector::from_u64(8, 0b1010_0110).unwrap();
    assert_eq!(vector.width(), 8);
    assert_eq!(vector.to_u64(), Some(0b1010_0110));
    assert_eq!(
        LogicVector::new(vec![LogicValue::One, LogicValue::X]).to_u64(),
        None
    );
}

#[test]
fn tri_state_multi_driver_resolution_handles_release_and_contention() {
    let tri_a = Component::new("ta", ComponentKind::tri_state_buffer())
        .with_pin(pin("in", PinDirection::Input))
        .with_pin(pin("enable", PinDirection::Input))
        .with_pin(pin("out", PinDirection::Output));
    let tri_b = Component::new("tb", ComponentKind::tri_state_buffer())
        .with_pin(pin("in", PinDirection::Input))
        .with_pin(pin("enable", PinDirection::Input))
        .with_pin(pin("out", PinDirection::Output));
    let circuit = Circuit::new()
        .with_component(input("a", LogicValue::One))
        .with_component(input("b", LogicValue::Zero))
        .with_component(input("ena", LogicValue::One))
        .with_component(input("enb", LogicValue::Zero))
        .with_component(tri_a)
        .with_component(tri_b)
        .with_component(sink("out"))
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("ta", "in")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("tb", "in")),
        )
        .with_net(
            Net::new("ena")
                .connect(NetEndpoint::new("ena", "out"))
                .connect(NetEndpoint::new("ta", "enable")),
        )
        .with_net(
            Net::new("enb")
                .connect(NetEndpoint::new("enb", "out"))
                .connect(NetEndpoint::new("tb", "enable")),
        )
        .with_net(
            Net::new("bus")
                .connect(NetEndpoint::new("ta", "out"))
                .connect(NetEndpoint::new("tb", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::One
    );

    let contended = Circuit::new()
        .with_component(input("a", LogicValue::One))
        .with_component(input("b", LogicValue::Zero))
        .with_component(input("ena", LogicValue::One))
        .with_component(input("enb", LogicValue::One))
        .with_component(
            Component::new("ta", ComponentKind::tri_state_buffer())
                .with_pin(pin("in", PinDirection::Input))
                .with_pin(pin("enable", PinDirection::Input))
                .with_pin(pin("out", PinDirection::Output)),
        )
        .with_component(
            Component::new("tb", ComponentKind::tri_state_buffer())
                .with_pin(pin("in", PinDirection::Input))
                .with_pin(pin("enable", PinDirection::Input))
                .with_pin(pin("out", PinDirection::Output)),
        )
        .with_component(sink("out"))
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("ta", "in")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("tb", "in")),
        )
        .with_net(
            Net::new("ena")
                .connect(NetEndpoint::new("ena", "out"))
                .connect(NetEndpoint::new("ta", "enable")),
        )
        .with_net(
            Net::new("enb")
                .connect(NetEndpoint::new("enb", "out"))
                .connect(NetEndpoint::new("tb", "enable")),
        )
        .with_net(
            Net::new("bus")
                .connect(NetEndpoint::new("ta", "out"))
                .connect(NetEndpoint::new("tb", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    assert_eq!(
        run(contended, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::X
    );
}

#[test]
fn mux_demux_and_decoder_have_canonical_selection_semantics() {
    let mux = Component::new("mux", ComponentKind::mux2())
        .with_pin(pin("a", PinDirection::Input))
        .with_pin(pin("b", PinDirection::Input))
        .with_pin(pin("sel", PinDirection::Input))
        .with_pin(pin("out", PinDirection::Output));
    let circuit = Circuit::new()
        .with_component(input("a", LogicValue::Zero))
        .with_component(input("b", LogicValue::One))
        .with_component(input("sel", LogicValue::One))
        .with_component(mux)
        .with_component(sink("out"))
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("mux", "a")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("mux", "b")),
        )
        .with_net(
            Net::new("sel")
                .connect(NetEndpoint::new("sel", "out"))
                .connect(NetEndpoint::new("mux", "sel")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("mux", "out"))
                .connect(NetEndpoint::new("out", "in")),
        );
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::One
    );

    let decoder = Component::new("dec", ComponentKind::decoder2_to_4())
        .with_pin(pin("a", PinDirection::Input))
        .with_pin(pin("b", PinDirection::Input))
        .with_pin(pin("enable", PinDirection::Input))
        .with_pin(pin("y0", PinDirection::Output))
        .with_pin(pin("y1", PinDirection::Output))
        .with_pin(pin("y2", PinDirection::Output))
        .with_pin(pin("y3", PinDirection::Output));
    let circuit = Circuit::new()
        .with_component(input("a", LogicValue::Zero))
        .with_component(input("b", LogicValue::One))
        .with_component(input("en", LogicValue::One))
        .with_component(decoder)
        .with_component(sink("out"))
        .with_net(
            Net::new("a")
                .connect(NetEndpoint::new("a", "out"))
                .connect(NetEndpoint::new("dec", "a")),
        )
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("b", "out"))
                .connect(NetEndpoint::new("dec", "b")),
        )
        .with_net(
            Net::new("en")
                .connect(NetEndpoint::new("en", "out"))
                .connect(NetEndpoint::new("dec", "enable")),
        )
        .with_net(
            Net::new("y2")
                .connect(NetEndpoint::new("dec", "y2"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("y0").connect(NetEndpoint::new("dec", "y0")))
        .with_net(Net::new("y1").connect(NetEndpoint::new("dec", "y1")))
        .with_net(Net::new("y3").connect(NetEndpoint::new("dec", "y3")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::One
    );
}

#[test]
fn latch_and_flip_flop_families_hold_state() {
    let d_latch = q_component("l", ComponentKind::d_latch(), &["d", "enable"]);
    let circuit = Circuit::new()
        .with_component(input("d", LogicValue::One))
        .with_component(input("en", LogicValue::One))
        .with_component(d_latch)
        .with_component(sink("out"))
        .with_net(
            Net::new("d")
                .connect(NetEndpoint::new("d", "out"))
                .connect(NetEndpoint::new("l", "d")),
        )
        .with_net(
            Net::new("en")
                .connect(NetEndpoint::new("en", "out"))
                .connect(NetEndpoint::new("l", "enable")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("l", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("l", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::One
    );

    let dff = q_component("ff", ComponentKind::d_flip_flop(), &["d", "clk"]);
    let circuit = Circuit::new()
        .with_component(input("d", LogicValue::One))
        .with_component(clock("clk", 10e-9))
        .with_component(dff)
        .with_component(sink("out"))
        .with_net(
            Net::new("d")
                .connect(NetEndpoint::new("d", "out"))
                .connect(NetEndpoint::new("ff", "d")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("ff", "clk")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("ff", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("ff", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 7e-9),
        LogicValue::One
    );

    let jk = q_component("jk", ComponentKind::jk_flip_flop(), &["j", "k", "clk"])
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(input("j", LogicValue::One))
        .with_component(input("k", LogicValue::One))
        .with_component(clock("clk", 10e-9))
        .with_component(jk)
        .with_component(sink("out"))
        .with_net(
            Net::new("j")
                .connect(NetEndpoint::new("j", "out"))
                .connect(NetEndpoint::new("jk", "j")),
        )
        .with_net(
            Net::new("k")
                .connect(NetEndpoint::new("k", "out"))
                .connect(NetEndpoint::new("jk", "k")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("jk", "clk")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("jk", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("jk", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 7e-9),
        LogicValue::One
    );

    let tff = q_component("tff", ComponentKind::t_flip_flop(), &["t", "clk"])
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(input("t", LogicValue::One))
        .with_component(clock("clk", 10e-9))
        .with_component(tff)
        .with_component(sink("out"))
        .with_net(
            Net::new("t")
                .connect(NetEndpoint::new("t", "out"))
                .connect(NetEndpoint::new("tff", "t")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("tff", "clk")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("tff", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("tff", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 17e-9),
        LogicValue::Zero
    );

    let sr = q_component("sr", ComponentKind::sr_flip_flop(), &["s", "r", "clk"])
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(input("s", LogicValue::One))
        .with_component(input("r", LogicValue::Zero))
        .with_component(clock("clk", 10e-9))
        .with_component(sr)
        .with_component(sink("out"))
        .with_net(
            Net::new("s")
                .connect(NetEndpoint::new("s", "out"))
                .connect(NetEndpoint::new("sr", "s")),
        )
        .with_net(
            Net::new("r")
                .connect(NetEndpoint::new("r", "out"))
                .connect(NetEndpoint::new("sr", "r")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("sr", "clk")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("sr", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("sr", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 7e-9),
        LogicValue::One
    );
}

#[test]
fn demux_and_sr_latch_cover_remaining_r3_primitives() {
    let demux = Component::new("demux", ComponentKind::demux2())
        .with_pin(pin("in", PinDirection::Input))
        .with_pin(pin("sel", PinDirection::Input))
        .with_pin(pin("a", PinDirection::Output))
        .with_pin(pin("b", PinDirection::Output));
    let circuit = Circuit::new()
        .with_component(input("data", LogicValue::One))
        .with_component(input("sel", LogicValue::One))
        .with_component(demux)
        .with_component(sink("out"))
        .with_net(
            Net::new("data")
                .connect(NetEndpoint::new("data", "out"))
                .connect(NetEndpoint::new("demux", "in")),
        )
        .with_net(
            Net::new("sel")
                .connect(NetEndpoint::new("sel", "out"))
                .connect(NetEndpoint::new("demux", "sel")),
        )
        .with_net(Net::new("a").connect(NetEndpoint::new("demux", "a")))
        .with_net(
            Net::new("b")
                .connect(NetEndpoint::new("demux", "b"))
                .connect(NetEndpoint::new("out", "in")),
        );
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::One
    );

    let latch = q_component("sr", ComponentKind::sr_latch(), &["s", "r"])
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(input("s", LogicValue::One))
        .with_component(input("r", LogicValue::Zero))
        .with_component(latch)
        .with_component(sink("out"))
        .with_net(
            Net::new("s")
                .connect(NetEndpoint::new("s", "out"))
                .connect(NetEndpoint::new("sr", "s")),
        )
        .with_net(
            Net::new("r")
                .connect(NetEndpoint::new("r", "out"))
                .connect(NetEndpoint::new("sr", "r")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("sr", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("sr", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 1e-6),
        LogicValue::One
    );
}

#[test]
fn falling_edge_flip_flop_is_supported_by_reference_engine() {
    let dff = q_component("ff", ComponentKind::d_flip_flop(), &["d", "clk"])
        .with_parameter("edge", ParameterValue::Text("falling".to_owned()))
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(input("d", LogicValue::One))
        .with_component(clock("clk", 10e-9))
        .with_component(dff)
        .with_component(sink("out"))
        .with_net(
            Net::new("d")
                .connect(NetEndpoint::new("d", "out"))
                .connect(NetEndpoint::new("ff", "d")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("ff", "clk")),
        )
        .with_net(
            Net::new("q")
                .connect(NetEndpoint::new("ff", "q"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("nq").connect(NetEndpoint::new("ff", "nq")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 12e-9),
        LogicValue::One
    );
}

#[test]
fn register_and_counter_operate_as_width_aware_bus_primitives() {
    let register = Component::new("reg", ComponentKind::register())
        .with_pin(pin("d0", PinDirection::Input))
        .with_pin(pin("d1", PinDirection::Input))
        .with_pin(pin("clk", PinDirection::Input))
        .with_pin(pin("enable", PinDirection::Input))
        .with_pin(pin("q0", PinDirection::Output))
        .with_pin(pin("q1", PinDirection::Output))
        .with_parameter("width", ParameterValue::Integer(2));
    let circuit = Circuit::new()
        .with_component(input("d0", LogicValue::One))
        .with_component(input("d1", LogicValue::Zero))
        .with_component(input("en", LogicValue::One))
        .with_component(clock("clk", 10e-9))
        .with_component(register)
        .with_component(sink("out"))
        .with_net(
            Net::new("d0")
                .connect(NetEndpoint::new("d0", "out"))
                .connect(NetEndpoint::new("reg", "d0")),
        )
        .with_net(
            Net::new("d1")
                .connect(NetEndpoint::new("d1", "out"))
                .connect(NetEndpoint::new("reg", "d1")),
        )
        .with_net(
            Net::new("en")
                .connect(NetEndpoint::new("en", "out"))
                .connect(NetEndpoint::new("reg", "enable")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("reg", "clk")),
        )
        .with_net(
            Net::new("q0")
                .connect(NetEndpoint::new("reg", "q0"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("q1").connect(NetEndpoint::new("reg", "q1")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 7e-9),
        LogicValue::One
    );

    let counter = Component::new("ctr", ComponentKind::counter())
        .with_pin(pin("clk", PinDirection::Input))
        .with_pin(pin("enable", PinDirection::Input))
        .with_pin(pin("q0", PinDirection::Output))
        .with_pin(pin("q1", PinDirection::Output))
        .with_pin(pin("q2", PinDirection::Output))
        .with_parameter("width", ParameterValue::Integer(3))
        .with_parameter("initial", ParameterValue::Integer(0));
    let circuit = Circuit::new()
        .with_component(input("en", LogicValue::One))
        .with_component(clock("clk", 10e-9))
        .with_component(counter)
        .with_component(sink("out"))
        .with_net(
            Net::new("en")
                .connect(NetEndpoint::new("en", "out"))
                .connect(NetEndpoint::new("ctr", "enable")),
        )
        .with_net(
            Net::new("clk")
                .connect(NetEndpoint::new("clk", "out"))
                .connect(NetEndpoint::new("ctr", "clk")),
        )
        .with_net(
            Net::new("q0")
                .connect(NetEndpoint::new("ctr", "q0"))
                .connect(NetEndpoint::new("out", "in")),
        )
        .with_net(Net::new("q1").connect(NetEndpoint::new("ctr", "q1")))
        .with_net(Net::new("q2").connect(NetEndpoint::new("ctr", "q2")));
    assert_eq!(
        run(circuit, NetEndpoint::new("out", "in"), 26e-9),
        LogicValue::One
    );
}
