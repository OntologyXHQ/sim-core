use ontologyx_sim_core::{
    CoSimulationConfig, CoSimulationEndpoint, CoSimulationLink, CoSimulationParticipant,
    CoSimulationPort, CoSimulationScheduler, CoSimulationTime, CoSimulationValue, EngineError,
    ExecutionControl, LogicValue, PinDirection, Waveform,
};

struct Source {
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
    value: LogicValue,
}

impl Source {
    fn new() -> Self {
        Self {
            ports: vec![CoSimulationPort::digital("out", PinDirection::Output)],
            time: CoSimulationTime::ZERO,
            value: LogicValue::Zero,
        }
    }
}

impl CoSimulationParticipant for Source {
    fn id(&self) -> &str {
        "source"
    }

    fn ports(&self) -> &[CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> CoSimulationTime {
        self.time
    }

    fn next_event_time(&self) -> Option<CoSimulationTime> {
        (self.time < CoSimulationTime::from_picoseconds(5_000))
            .then_some(CoSimulationTime::from_picoseconds(5_000))
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        _control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.time = target;
        if target >= CoSimulationTime::from_picoseconds(5_000) {
            self.value = LogicValue::One;
        }
        Ok(())
    }

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError> {
        if port != "out" {
            return Err(EngineError::new("example_port_missing", port));
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
            "example_input_unsupported",
            "source has no input",
        ))
    }
}

struct Inverter {
    ports: Vec<CoSimulationPort>,
    time: CoSimulationTime,
    input: LogicValue,
    output: LogicValue,
}

impl Inverter {
    fn new() -> Self {
        Self {
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

impl CoSimulationParticipant for Inverter {
    fn id(&self) -> &str {
        "inverter"
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
            return Err(EngineError::new("example_port_missing", port));
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
                "example_input_invalid",
                "invalid inverter input write",
            ));
        }
        let CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new(
                "example_domain_invalid",
                "inverter expects digital input",
            ));
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut scheduler = CoSimulationScheduler::new(CoSimulationConfig::new(
        CoSimulationTime::from_picoseconds(10_000),
        CoSimulationTime::from_picoseconds(10_000),
    ));
    scheduler.add_participant(Source::new())?;
    scheduler.add_participant(Inverter::new())?;
    scheduler.connect(CoSimulationLink::new(
        CoSimulationEndpoint::new("source", "out"),
        CoSimulationEndpoint::new("inverter", "in"),
    ));

    let report = scheduler.run(&ExecutionControl::default())?;
    for waveform in &report.result.waveforms {
        if let Waveform::Digital(waveform) = waveform {
            println!(
                "{}: {} transition(s)",
                waveform.signal.as_str(),
                waveform.transitions.len()
            );
        }
    }
    Ok(())
}
