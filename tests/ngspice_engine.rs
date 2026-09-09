use ontologyx_sim_core::{
    AcScale, Analysis, AxisKind, Circuit, Component, ComponentKind, ModelDefinition, Net,
    NetEndpoint, NgSpiceEngine, ParameterValue, Pin, PinDirection, Probe, Quantity, SignalDomain,
    SimulationEngine, SimulationRequest, Unit, Waveform,
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

fn capacitor(id: &str, farads: f64) -> Component {
    Component::new(id, ComponentKind::capacitor())
        .with_pin(analog_pin("a"))
        .with_pin(analog_pin("b"))
        .with_parameter(
            "capacitance",
            ParameterValue::Quantity(Quantity::new(farads, Unit::Farad)),
        )
}

fn voltage_source() -> Component {
    Component::new("v1", ComponentKind::voltage_source())
        .with_pin(analog_pin("pos"))
        .with_pin(analog_pin("neg"))
}

fn probe_out() -> Vec<Probe> {
    vec![Probe {
        endpoint: NetEndpoint::new("r1", "b"),
        alias: Some("vout".to_owned()),
    }]
}

fn divider_circuit(source: Component) -> Circuit {
    Circuit::new()
        .with_component(source)
        .with_component(resistor("r1", 1000.0))
        .with_component(resistor("r2", 1000.0))
        .with_component(ground())
        .with_net(
            Net::new("vin")
                .connect(NetEndpoint::new("v1", "pos"))
                .connect(NetEndpoint::new("r1", "a")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("r2", "a")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("v1", "neg"))
                .connect(NetEndpoint::new("r2", "b"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        )
}

fn rc_circuit(source: Component) -> Circuit {
    Circuit::new()
        .with_component(source)
        .with_component(resistor("r1", 1000.0))
        .with_component(capacitor("c1", 1e-6))
        .with_component(ground())
        .with_net(
            Net::new("vin")
                .connect(NetEndpoint::new("v1", "pos"))
                .connect(NetEndpoint::new("r1", "a")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("c1", "a")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("v1", "neg"))
                .connect(NetEndpoint::new("c1", "b"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        )
}

fn require_engine() -> Option<NgSpiceEngine> {
    let engine = NgSpiceEngine::default();
    let info = engine.info();
    if info.available {
        eprintln!("using ngspice: {info:?}");
        return Some(engine);
    }
    if std::env::var_os("ONTOLOGYX_SIM_REQUIRE_NGSPICE").is_some() {
        panic!("ngspice is required for this verification run but was not found: {info:?}");
    }
    eprintln!("ngspice unavailable; integration check skipped");
    None
}

fn analog(result: &ontologyx_sim_core::SimulationResult) -> &ontologyx_sim_core::AnalogWaveform {
    let Waveform::Analog(waveform) = &result.waveforms[0] else {
        panic!("expected analog waveform");
    };
    waveform
}

#[test]
fn real_ngspice_solves_operating_point_voltage_divider() {
    let Some(engine) = require_engine() else {
        return;
    };
    let source = voltage_source().with_parameter(
        "dc",
        ParameterValue::Quantity(Quantity::new(5.0, Unit::Volt)),
    );
    let request = SimulationRequest {
        circuit: divider_circuit(source),
        analysis: Analysis::OperatingPoint,
        probes: probe_out(),
    };
    let result = engine
        .simulate(&request)
        .expect("ngspice OP simulation should succeed");
    assert_eq!(result.metadata.core_version, ontologyx_sim_core::VERSION);
    assert!(result.metadata.engine_version.is_some());
    assert_eq!(result.stats.waveform_count, 1);
    assert_eq!(result.stats.point_count, 1);
    let waveform = analog(&result);
    assert_eq!(waveform.signal.as_str(), "vout");
    assert_eq!(waveform.axis.kind, AxisKind::Scalar);
    assert_eq!(waveform.values.len(), 1);
    assert!(
        (waveform.values[0] - 2.5).abs() < 1e-6,
        "unexpected divider voltage: {:?}",
        waveform.values
    );
}

#[test]
fn real_ngspice_solves_dc_sweep() {
    let Some(engine) = require_engine() else {
        return;
    };
    let source = voltage_source().with_parameter(
        "dc",
        ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
    );
    let request = SimulationRequest {
        circuit: divider_circuit(source),
        analysis: Analysis::DcSweep {
            source: "v1".to_owned(),
            start: 0.0,
            stop: 5.0,
            step: 1.0,
        },
        probes: probe_out(),
    };
    let result = engine
        .simulate(&request)
        .expect("ngspice DC simulation should succeed");
    let waveform = analog(&result);
    assert_eq!(waveform.axis.kind, AxisKind::DcSweep);
    assert!(
        waveform.values.len() >= 6,
        "unexpected DC points: {:?}",
        waveform.values
    );
    assert!((waveform.values.last().copied().unwrap() - 2.5).abs() < 1e-6);
}

#[test]
fn real_ngspice_solves_transient_rc_charge() {
    let Some(engine) = require_engine() else {
        return;
    };
    let source = voltage_source()
        .with_parameter(
            "dc",
            ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
        )
        .with_parameter(
            "pulse_high",
            ParameterValue::Quantity(Quantity::new(5.0, Unit::Volt)),
        )
        .with_parameter(
            "pulse_delay",
            ParameterValue::Quantity(Quantity::new(1e-6, Unit::Second)),
        )
        .with_parameter(
            "pulse_width",
            ParameterValue::Quantity(Quantity::new(5e-3, Unit::Second)),
        )
        .with_parameter(
            "pulse_period",
            ParameterValue::Quantity(Quantity::new(10e-3, Unit::Second)),
        );
    let request = SimulationRequest {
        circuit: rc_circuit(source),
        analysis: Analysis::Transient {
            step: 10e-6,
            stop: 3e-3,
        },
        probes: probe_out(),
    };
    let result = engine
        .simulate(&request)
        .expect("ngspice transient simulation should succeed");
    let waveform = analog(&result);
    assert_eq!(waveform.axis.kind, AxisKind::Time);
    assert!(waveform.values.len() > 10);
    assert!(
        waveform.values[0].abs() < 0.1,
        "unexpected initial RC voltage: {}",
        waveform.values[0]
    );
    assert!(
        waveform.values.last().copied().unwrap() > 4.0,
        "RC output did not charge: {:?}",
        waveform.values.last()
    );
}

#[test]
fn real_ngspice_solves_complex_ac_rc_lowpass() {
    let Some(engine) = require_engine() else {
        return;
    };
    let source = voltage_source()
        .with_parameter(
            "dc",
            ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
        )
        .with_parameter(
            "ac_magnitude",
            ParameterValue::Quantity(Quantity::new(1.0, Unit::Volt)),
        );
    let request = SimulationRequest {
        circuit: rc_circuit(source),
        analysis: Analysis::AcSweep {
            scale: AcScale::Decade,
            points: 10,
            start_hz: 10.0,
            stop_hz: 10_000.0,
        },
        probes: probe_out(),
    };
    let result = engine
        .simulate(&request)
        .expect("ngspice AC simulation should succeed");
    let waveform = analog(&result);
    assert_eq!(waveform.axis.kind, AxisKind::Frequency);
    let imaginary = waveform
        .imaginary
        .as_ref()
        .expect("AC output must preserve imaginary values");
    let first = waveform.values[0].hypot(imaginary[0]);
    let last_index = waveform.values.len() - 1;
    let last = waveform.values[last_index].hypot(imaginary[last_index]);
    assert!(first > 0.9, "unexpected low-frequency gain: {first}");
    assert!(last < 0.1, "unexpected high-frequency gain: {last}");
}

fn named_voltage_source(id: &str, volts: f64) -> Component {
    Component::new(id, ComponentKind::voltage_source())
        .with_pin(analog_pin("pos"))
        .with_pin(analog_pin("neg"))
        .with_parameter(
            "dc",
            ParameterValue::Quantity(Quantity::new(volts, Unit::Volt)),
        )
}

fn probe(component: &str, pin: &str, alias: &str) -> Vec<Probe> {
    vec![Probe {
        endpoint: NetEndpoint::new(component, pin),
        alias: Some(alias.to_owned()),
    }]
}

#[test]
fn real_ngspice_solves_diode_model() {
    let Some(engine) = require_engine() else {
        return;
    };
    let diode = Component::new("d1", ComponentKind::diode())
        .with_pin(analog_pin("anode"))
        .with_pin(analog_pin("cathode"))
        .with_parameter("model", ParameterValue::Text("diode-basic".to_owned()));
    let circuit = Circuit::new()
        .with_model(ModelDefinition::spice_device(
            "diode-basic",
            "DTEST",
            ".model DTEST D (IS=1e-14 N=1)",
        ))
        .with_component(named_voltage_source("v1", 5.0))
        .with_component(resistor("r1", 1000.0))
        .with_component(diode)
        .with_component(ground())
        .with_net(
            Net::new("vin")
                .connect(NetEndpoint::new("v1", "pos"))
                .connect(NetEndpoint::new("r1", "a")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("d1", "anode")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("v1", "neg"))
                .connect(NetEndpoint::new("d1", "cathode"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        );
    let request = SimulationRequest {
        circuit,
        analysis: Analysis::OperatingPoint,
        probes: probe("d1", "anode", "vdiode"),
    };
    let result = engine
        .simulate(&request)
        .expect("diode model simulation should succeed");
    let voltage = analog(&result).values[0];
    assert!(
        (0.4..1.0).contains(&voltage),
        "unexpected diode forward voltage: {voltage}"
    );
}

#[test]
fn real_ngspice_solves_bjt_model() {
    let Some(engine) = require_engine() else {
        return;
    };
    let transistor = Component::new("q1", ComponentKind::bjt())
        .with_pin(analog_pin("collector"))
        .with_pin(analog_pin("base"))
        .with_pin(analog_pin("emitter"))
        .with_parameter("model", ParameterValue::Text("npn-basic".to_owned()));
    let circuit = Circuit::new()
        .with_model(ModelDefinition::spice_device(
            "npn-basic",
            "QTEST",
            ".model QTEST NPN (IS=1e-15 BF=100 VAF=100)",
        ))
        .with_component(named_voltage_source("vcc", 5.0))
        .with_component(named_voltage_source("vb", 0.7))
        .with_component(resistor("r1", 1000.0))
        .with_component(transistor)
        .with_component(ground())
        .with_net(
            Net::new("vcc")
                .connect(NetEndpoint::new("vcc", "pos"))
                .connect(NetEndpoint::new("r1", "a")),
        )
        .with_net(
            Net::new("collector")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("q1", "collector")),
        )
        .with_net(
            Net::new("base")
                .connect(NetEndpoint::new("vb", "pos"))
                .connect(NetEndpoint::new("q1", "base")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("vcc", "neg"))
                .connect(NetEndpoint::new("vb", "neg"))
                .connect(NetEndpoint::new("q1", "emitter"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        );
    let request = SimulationRequest {
        circuit,
        analysis: Analysis::OperatingPoint,
        probes: probe("q1", "collector", "vcollector"),
    };
    let result = engine
        .simulate(&request)
        .expect("BJT model simulation should succeed");
    let voltage = analog(&result).values[0];
    assert!(
        voltage.is_finite() && (0.0..5.01).contains(&voltage),
        "unexpected BJT collector voltage: {voltage}"
    );
}

#[test]
fn real_ngspice_solves_mosfet_model() {
    let Some(engine) = require_engine() else {
        return;
    };
    let transistor = Component::new("m1", ComponentKind::mosfet())
        .with_pin(analog_pin("drain"))
        .with_pin(analog_pin("gate"))
        .with_pin(analog_pin("source"))
        .with_pin(analog_pin("bulk"))
        .with_parameter("model", ParameterValue::Text("nmos-basic".to_owned()));
    let circuit = Circuit::new()
        .with_model(ModelDefinition::spice_device(
            "nmos-basic",
            "MTEST",
            ".model MTEST NMOS (LEVEL=1 VTO=1 KP=0.01 LAMBDA=0.01)",
        ))
        .with_component(named_voltage_source("vdd", 5.0))
        .with_component(named_voltage_source("vg", 5.0))
        .with_component(resistor("r1", 1000.0))
        .with_component(transistor)
        .with_component(ground())
        .with_net(
            Net::new("vdd")
                .connect(NetEndpoint::new("vdd", "pos"))
                .connect(NetEndpoint::new("r1", "a")),
        )
        .with_net(
            Net::new("drain")
                .connect(NetEndpoint::new("r1", "b"))
                .connect(NetEndpoint::new("m1", "drain")),
        )
        .with_net(
            Net::new("gate")
                .connect(NetEndpoint::new("vg", "pos"))
                .connect(NetEndpoint::new("m1", "gate")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("vdd", "neg"))
                .connect(NetEndpoint::new("vg", "neg"))
                .connect(NetEndpoint::new("m1", "source"))
                .connect(NetEndpoint::new("m1", "bulk"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        );
    let request = SimulationRequest {
        circuit,
        analysis: Analysis::OperatingPoint,
        probes: probe("m1", "drain", "vdrain"),
    };
    let result = engine
        .simulate(&request)
        .expect("MOSFET model simulation should succeed");
    let voltage = analog(&result).values[0];
    assert!(
        voltage.is_finite() && voltage < 1.0,
        "NMOS did not pull drain low: {voltage}"
    );
}

#[test]
fn real_ngspice_solves_op_amp_subcircuit_model() {
    let Some(engine) = require_engine() else {
        return;
    };
    let op_amp = Component::new("u1", ComponentKind::op_amp())
        .with_pin(analog_pin("non_inverting"))
        .with_pin(analog_pin("inverting"))
        .with_pin(analog_pin("positive_supply"))
        .with_pin(analog_pin("negative_supply"))
        .with_pin(analog_pin("output"))
        .with_parameter("model", ParameterValue::Text("opamp-basic".to_owned()));
    let circuit = Circuit::new()
        .with_model(ModelDefinition::spice_subcircuit(
            "opamp-basic",
            "OX_OPAMP",
            ".subckt OX_OPAMP INP INN VCC VEE OUT\nEGAIN NINT 0 INP INN 100000\nROUT NINT OUT 10\n.ends OX_OPAMP",
        ))
        .with_component(named_voltage_source("vin", 1.0))
        .with_component(named_voltage_source("vcc", 5.0))
        .with_component(named_voltage_source("vee", -5.0))
        .with_component(op_amp)
        .with_component(resistor("load", 10000.0))
        .with_component(ground())
        .with_net(
            Net::new("vin")
                .connect(NetEndpoint::new("vin", "pos"))
                .connect(NetEndpoint::new("u1", "non_inverting")),
        )
        .with_net(
            Net::new("vcc")
                .connect(NetEndpoint::new("vcc", "pos"))
                .connect(NetEndpoint::new("u1", "positive_supply")),
        )
        .with_net(
            Net::new("vee")
                .connect(NetEndpoint::new("vee", "pos"))
                .connect(NetEndpoint::new("u1", "negative_supply")),
        )
        .with_net(
            Net::new("out")
                .connect(NetEndpoint::new("u1", "output"))
                .connect(NetEndpoint::new("u1", "inverting"))
                .connect(NetEndpoint::new("load", "a")),
        )
        .with_net(
            Net::new("gnd")
                .connect(NetEndpoint::new("vin", "neg"))
                .connect(NetEndpoint::new("vcc", "neg"))
                .connect(NetEndpoint::new("vee", "neg"))
                .connect(NetEndpoint::new("load", "b"))
                .connect(NetEndpoint::new("gnd", "gnd")),
        );
    let request = SimulationRequest {
        circuit,
        analysis: Analysis::OperatingPoint,
        probes: probe("u1", "output", "vout"),
    };
    let result = engine
        .simulate(&request)
        .expect("op-amp subcircuit simulation should succeed");
    let voltage = analog(&result).values[0];
    assert!(
        (voltage - 1.0).abs() < 1e-3,
        "unexpected op-amp follower output: {voltage}"
    );
}
