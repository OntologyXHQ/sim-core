use std::collections::BTreeMap;

use ontologyx_sim_core::{
    AnalysisKind, CoSimulationConfig, CoSimulationEndpoint, CoSimulationLink,
    CoSimulationParticipant, CoSimulationPort, CoSimulationScheduler, CoSimulationTime,
    CoSimulationValue, DigitalWaveform, EngineError, ExecutionControl, ExecutionPolicy, LogicValue,
    PinDirection, SignalDomain, Unit, Waveform,
};

#[derive(Clone)]
struct DigitalSource {
    id: String,
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
    value: LogicValue,
    events: Vec<(CoSimulationTime, LogicValue)>,
    next_event: usize,
}

impl DigitalSource {
    fn new(id: &str, value: LogicValue, events: Vec<(u64, LogicValue)>) -> Self {
        Self {
            id: id.to_owned(),
            ports: vec![CoSimulationPort::digital("out", PinDirection::Output)],
            time: CoSimulationTime::ZERO,
            value,
            events: events
                .into_iter()
                .map(|(time, value)| (CoSimulationTime::from_picoseconds(time), value))
                .collect(),
            next_event: 0,
        }
    }
}

impl CoSimulationParticipant for DigitalSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.time
    }

    fn next_event_time(&self) -> Option<CoSimulationTime> {
        self.events.get(self.next_event).map(|event| event.0)
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        _control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        while let Some((time, value)) = self.events.get(self.next_event).copied() {
            if time > target {
                break;
            }
            self.value = value;
            self.next_event += 1;
        }
        self.time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != "out" {
            return Err(EngineError::new("test_port_missing", port));
        }
        Ok(CoSimulationValue::Digital(self.value))
    }

    fn write_input(
        &mut self,
        _port: &str,
        _time: CoSimulationTime,
        _value: &CoSimulationValue,
        _control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        Err(EngineError::new(
            "test_input_unsupported",
            "source has no input",
        ))
    }
}

struct DigitalNot {
    id: String,
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
    input: LogicValue,
    output: LogicValue,
}

impl DigitalNot {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            ports: vec![
                CoSimulationPort::digital("in", PinDirection::Input),
                CoSimulationPort::digital("out", PinDirection::Output),
            ],
            time: CoSimulationTime::ZERO,
            input: LogicValue::X,
            output: LogicValue::X,
        }
    }
}

impl CoSimulationParticipant for DigitalNot {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.time
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        _control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != "out" {
            return Err(EngineError::new("test_port_missing", port));
        }
        Ok(CoSimulationValue::Digital(self.output))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: CoSimulationTime,
        value: &CoSimulationValue,
        _control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        if port != "in" {
            return Err(EngineError::new("test_port_missing", port));
        }
        if time != self.time {
            return Err(EngineError::new(
                "test_time_mismatch",
                "input time mismatch",
            ));
        }
        let CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new("test_domain_mismatch", "expected digital"));
        };
        if self.input == *value {
            return Ok(false);
        }
        self.input = *value;
        self.output = match value {
            LogicValue::Zero => LogicValue::One,
            LogicValue::One => LogicValue::Zero,
            LogicValue::X | LogicValue::Z => LogicValue::X,
        };
        Ok(true)
    }
}

struct DigitalBuffer {
    id: String,
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
    input: LogicValue,
    output: LogicValue,
}

impl DigitalBuffer {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            ports: vec![
                CoSimulationPort::digital("in", PinDirection::Input),
                CoSimulationPort::digital("out", PinDirection::Output),
            ],
            time: CoSimulationTime::ZERO,
            input: LogicValue::X,
            output: LogicValue::X,
        }
    }
}

impl CoSimulationParticipant for DigitalBuffer {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.time
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        _control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != "out" {
            return Err(EngineError::new("test_port_missing", port));
        }
        Ok(CoSimulationValue::Digital(self.output))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: CoSimulationTime,
        value: &CoSimulationValue,
        _control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        if port != "in" {
            return Err(EngineError::new("test_port_missing", port));
        }
        if time != self.time {
            return Err(EngineError::new(
                "test_time_mismatch",
                "input time mismatch",
            ));
        }
        let CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new("test_domain_mismatch", "expected digital"));
        };
        if self.input == *value {
            return Ok(false);
        }
        self.input = *value;
        self.output = *value;
        Ok(true)
    }
}

struct AnalogSource {
    id: String,
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
}

impl AnalogSource {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            ports: vec![CoSimulationPort::analog(
                "out",
                PinDirection::Output,
                Unit::Volt,
            )],
            time: CoSimulationTime::ZERO,
        }
    }
}

impl CoSimulationParticipant for AnalogSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.time
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        _control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != "out" {
            return Err(EngineError::new("test_port_missing", port));
        }
        Ok(CoSimulationValue::Analog(1.25))
    }

    fn write_input(
        &mut self,
        _port: &str,
        _time: CoSimulationTime,
        _value: &CoSimulationValue,
        _control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        Err(EngineError::new(
            "test_input_unsupported",
            "source has no input",
        ))
    }
}

struct Oscillator {
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
    input: LogicValue,
    output: LogicValue,
}

impl Oscillator {
    fn new() -> Self {
        Self {
            ports: vec![
                CoSimulationPort::digital("in", PinDirection::Input),
                CoSimulationPort::digital("out", PinDirection::Output),
            ],
            time: CoSimulationTime::ZERO,
            input: LogicValue::X,
            output: LogicValue::Zero,
        }
    }
}

impl CoSimulationParticipant for Oscillator {
    fn id(&self) -> &str {
        "osc"
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.time
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        _control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.time = target;
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != "out" {
            return Err(EngineError::new("test_port_missing", port));
        }
        Ok(CoSimulationValue::Digital(self.output))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: CoSimulationTime,
        value: &CoSimulationValue,
        _control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        if port != "in" || time != self.time {
            return Err(EngineError::new(
                "test_write_invalid",
                "invalid oscillator write",
            ));
        }
        let CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new("test_domain_mismatch", "expected digital"));
        };
        if self.input == *value {
            return Ok(false);
        }
        self.input = *value;
        self.output = match value {
            LogicValue::Zero => LogicValue::One,
            LogicValue::One => LogicValue::Zero,
            LogicValue::X | LogicValue::Z => LogicValue::Zero,
        };
        Ok(true)
    }
}

fn scheduler(order: &[&str]) -> CoSimulationScheduler {
    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig {
        stop: CoSimulationTime::from_picoseconds(20),
        max_step: CoSimulationTime::from_picoseconds(10),
        max_delta_cycles: 16,
        max_steps: 16,
    });
    for id in order {
        match *id {
            "src" => scheduler
                .add_participant(DigitalSource::new(
                    "src",
                    LogicValue::Zero,
                    vec![(5, LogicValue::One), (15, LogicValue::Zero)],
                ))
                .unwrap(),
            "inv" => scheduler.add_participant(DigitalNot::new("inv")).unwrap(),
            "buf" => scheduler
                .add_participant(DigitalBuffer::new("buf"))
                .unwrap(),
            _ => unreachable!(),
        }
    }
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("src", "out"),
        CoSimulationEndpoint::new("inv", "in"),
    ));
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("inv", "out"),
        CoSimulationEndpoint::new("buf", "in"),
    ));
    scheduler
}

fn digital_waveforms(result: &[Waveform]) -> BTreeMap<String, DigitalWaveform> {
    result
        .iter()
        .filter_map(|waveform| match waveform {
            Waveform::Digital(waveform) => {
                Some((waveform.signal.as_str().to_owned(), waveform.clone()))
            }
            Waveform::Analog(_) => None,
        })
        .collect()
}

#[test]
fn scheduler_is_registration_order_independent_and_preserves_event_time() {
    let control = ExecutionControl::default();
    let first = scheduler(&["src", "inv", "buf"]).run(&control).unwrap();
    let second = scheduler(&["buf", "src", "inv"]).run(&control).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.result.analysis, AnalysisKind::CoSimulationTransient);

    let waveforms = digital_waveforms(&first.result.waveforms);
    let output = &waveforms["buf.out"];
    assert_eq!(
        output.transitions,
        vec![
            ontologyx_sim_core::DigitalTransition {
                time: 0.0,
                value: LogicValue::One,
            },
            ontologyx_sim_core::DigitalTransition {
                time: 5e-12,
                value: LogicValue::Zero,
            },
            ontologyx_sim_core::DigitalTransition {
                time: 15e-12,
                value: LogicValue::One,
            },
        ]
    );
    assert_eq!(first.scheduler.macro_steps, 3);
}

#[test]
fn same_time_links_settle_as_bounded_delta_cycles() {
    let report = scheduler(&["buf", "inv", "src"])
        .run(&ExecutionControl::default())
        .unwrap();
    assert!(report.scheduler.delta_cycles >= 2);
    assert_eq!(
        digital_waveforms(&report.result.waveforms)["buf.out"].transitions[0].value,
        LogicValue::One
    );
}

#[test]
fn combinational_oscillation_fails_closed() {
    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig {
        stop: CoSimulationTime::from_picoseconds(10),
        max_step: CoSimulationTime::from_picoseconds(10),
        max_delta_cycles: 4,
        max_steps: 8,
    });
    scheduler.add_participant(Oscillator::new()).unwrap();
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("osc", "out"),
        CoSimulationEndpoint::new("osc", "in"),
    ));
    let error = scheduler.run(&ExecutionControl::default()).unwrap_err();
    assert_eq!(error.code(), "cosim_delta_cycle_limit");
}

#[test]
fn domain_crossing_requires_explicit_bridge_participant() {
    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig::new(
        CoSimulationTime::from_picoseconds(10),
        CoSimulationTime::from_picoseconds(10),
    ));
    scheduler
        .add_participant(AnalogSource::new("analog"))
        .unwrap();
    scheduler
        .add_participant(DigitalBuffer::new("digital"))
        .unwrap();
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("analog", "out"),
        CoSimulationEndpoint::new("digital", "in"),
    ));
    let error = scheduler.run(&ExecutionControl::default()).unwrap_err();
    assert_eq!(error.code(), "cosim_domain_mismatch");
}

#[test]
fn cancellation_and_config_limits_are_fail_closed() {
    let cancellation = ontologyx_sim_core::CancellationToken::new();
    cancellation.cancel();
    let control = ExecutionControl::new(ExecutionPolicy::default()).with_cancellation(cancellation);
    let error = scheduler(&["src", "inv", "buf"]).run(&control).unwrap_err();
    assert_eq!(error.code(), "execution_cancelled");

    let invalid = CoSimulationConfig {
        stop: CoSimulationTime::from_picoseconds(10),
        max_step: CoSimulationTime::ZERO,
        max_delta_cycles: 1,
        max_steps: 1,
    };
    assert_eq!(
        invalid.validate().unwrap_err().code(),
        "cosim_config_invalid"
    );
}

#[test]
fn ports_reject_mixed_reference_and_incompatible_units() {
    struct BadParticipant {
        ports: Vec<CoSimulationPort>,
    }
    impl CoSimulationParticipant for BadParticipant {
        fn id(&self) -> &str {
            "bad"
        }
        fn ports(&self) -> &[CoSimulationPort] {
            &self.ports
        }
        fn current_time(&self) -> CoSimulationTime {
            CoSimulationTime::ZERO
        }
        fn advance_to(
            &mut self,
            _target: CoSimulationTime,
            _control: &ExecutionControl,
        ) -> Result<(), EngineError> {
            Ok(())
        }
        fn read_output(&self, _port: &str) -> Result<CoSimulationValue, EngineError> {
            Ok(CoSimulationValue::Digital(LogicValue::Zero))
        }
        fn write_input(
            &mut self,
            _port: &str,
            _time: CoSimulationTime,
            _value: &CoSimulationValue,
            _control: &ExecutionControl,
        ) -> Result<bool, EngineError> {
            Ok(false)
        }
    }

    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig::new(
        CoSimulationTime::from_picoseconds(10),
        CoSimulationTime::from_picoseconds(10),
    ));
    let error = scheduler
        .add_participant(BadParticipant {
            ports: vec![CoSimulationPort {
                name: "mixed".to_owned(),
                domain: SignalDomain::Mixed,
                direction: PinDirection::Output,
                unit: None,
            }],
        })
        .unwrap_err();
    assert_eq!(error.code(), "cosim_port_invalid");
}
