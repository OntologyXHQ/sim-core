//! Deterministic co-simulation scheduler substrate.
//!
//! R9.1 deliberately owns only the master-algorithm contract: an integer
//! picosecond timebase, participant/port/link validation, bounded macro steps,
//! same-time delta-cycle settling, cancellation/timeout checks, and normalized
//! output waveforms. Backend-specific incremental sessions for Renode,
//! Verilator, ngspice/XSPICE and the built-in digital engine plug into the
//! `CoSimulationParticipant` trait in subsequent R9 adapter work.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    AnalogAxis, AnalogWaveform, AnalysisKind, CIRCUIT_SCHEMA_VERSION, Diagnostic, DiagnosticLevel,
    DigitalTransition, DigitalWaveform, EngineError, EngineId, ExecutionControl, LogicValue,
    PinDirection, SignalDomain, SignalId, SimulationResult, Unit, VERSION, Waveform,
};

pub const COSIM_SCHEDULER_ID: &str = "co-simulation";
pub const COSIM_TIMEBASE_HZ: u64 = 1_000_000_000_000;
pub const DEFAULT_MAX_DELTA_CYCLES: usize = 64;
pub const DEFAULT_MAX_COSIM_STEPS: usize = 1_000_000;

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct CoSimulationTime(pub u64);

impl CoSimulationTime {
    pub const ZERO: Self = Self(0);

    pub const fn from_picoseconds(picoseconds: u64) -> Self {
        Self(picoseconds)
    }

    pub const fn as_picoseconds(self) -> u64 {
        self.0
    }

    pub fn from_seconds(seconds: f64) -> Result<Self, EngineError> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(EngineError::new(
                "cosim_time_invalid",
                "co-simulation time must be finite and non-negative",
            ));
        }
        let ticks = seconds * COSIM_TIMEBASE_HZ as f64;
        if ticks > u64::MAX as f64 {
            return Err(EngineError::new(
                "cosim_time_invalid",
                "co-simulation time exceeds the picosecond timebase range",
            ));
        }
        Ok(Self(ticks.round() as u64))
    }

    pub fn as_seconds(self) -> f64 {
        self.0 as f64 / COSIM_TIMEBASE_HZ as f64
    }

    fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoSimulationPort {
    pub name: String,
    pub domain: SignalDomain,
    pub direction: PinDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Unit>,
}

impl CoSimulationPort {
    pub fn digital(name: impl Into<String>, direction: PinDirection) -> Self {
        Self {
            name: name.into(),
            domain: SignalDomain::Digital,
            direction,
            unit: None,
        }
    }

    pub fn analog(name: impl Into<String>, direction: PinDirection, unit: Unit) -> Self {
        Self {
            name: name.into(),
            domain: SignalDomain::Analog,
            direction,
            unit: Some(unit),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "domain", content = "value", rename_all = "snake_case")]
pub enum CoSimulationValue {
    Digital(LogicValue),
    Analog(f64),
}

impl CoSimulationValue {
    pub const fn domain(&self) -> SignalDomain {
        match self {
            Self::Digital(_) => SignalDomain::Digital,
            Self::Analog(_) => SignalDomain::Analog,
        }
    }

    fn validate(&self) -> Result<(), EngineError> {
        if matches!(self, Self::Analog(value) if !value.is_finite()) {
            return Err(EngineError::new(
                "cosim_value_invalid",
                "analog co-simulation values must be finite",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct CoSimulationEndpoint {
    pub participant: String,
    pub port: String,
}

impl CoSimulationEndpoint {
    pub fn new(participant: impl Into<String>, port: impl Into<String>) -> Self {
        Self {
            participant: participant.into(),
            port: port.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct CoSimulationLink {
    pub source: CoSimulationEndpoint,
    pub target: CoSimulationEndpoint,
}

impl CoSimulationLink {
    pub fn new(source: CoSimulationEndpoint, target: CoSimulationEndpoint) -> Self {
        Self { source, target }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoSimulationConfig {
    pub stop: CoSimulationTime,
    pub max_step: CoSimulationTime,
    pub max_delta_cycles: usize,
    pub max_steps: usize,
}

impl CoSimulationConfig {
    pub fn new(stop: CoSimulationTime, max_step: CoSimulationTime) -> Self {
        Self {
            stop,
            max_step,
            max_delta_cycles: DEFAULT_MAX_DELTA_CYCLES,
            max_steps: DEFAULT_MAX_COSIM_STEPS,
        }
    }

    pub fn validate(self) -> Result<(), EngineError> {
        if self.stop == CoSimulationTime::ZERO {
            return Err(EngineError::new(
                "cosim_config_invalid",
                "co-simulation stop time must be greater than zero",
            ));
        }
        if self.max_step == CoSimulationTime::ZERO {
            return Err(EngineError::new(
                "cosim_config_invalid",
                "co-simulation max_step must be greater than zero",
            ));
        }
        if self.max_delta_cycles == 0 {
            return Err(EngineError::new(
                "cosim_config_invalid",
                "co-simulation max_delta_cycles must be greater than zero",
            ));
        }
        if self.max_steps == 0 {
            return Err(EngineError::new(
                "cosim_config_invalid",
                "co-simulation max_steps must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoSimulationStats {
    pub macro_steps: usize,
    pub delta_cycles: usize,
    pub participant_advances: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoSimulationReport {
    pub result: SimulationResult,
    pub scheduler: CoSimulationStats,
}

/// Incremental backend session used by the deterministic co-simulation master.
///
/// Participants must use `CoSimulationTime` as their externally visible clock.
/// `next_event_time`, when present, must be strictly greater than `current_time`.
/// `write_input` is a same-time operation: implementations must settle any
/// immediately-causal output changes before returning and report whether the
/// effective input changed. This lets the scheduler iterate deterministic
/// delta cycles without embedding solver-specific semantics.
pub trait CoSimulationParticipant: Send {
    fn id(&self) -> &str;
    fn ports(&self) -> &[CoSimulationPort];
    fn current_time(&self) -> CoSimulationTime;

    fn initialize(&mut self, control: &ExecutionControl) -> Result<(), EngineError> {
        control.policy.validate()?;
        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                "co-simulation was cancelled before participant initialization",
            ));
        }
        Ok(())
    }

    fn next_event_time(&self) -> Option<CoSimulationTime> {
        None
    }

    fn advance_to(
        &mut self,
        target: CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError>;

    fn read_output(&self, port: &str) -> Result<CoSimulationValue, EngineError>;

    fn write_input(
        &mut self,
        port: &str,
        time: CoSimulationTime,
        value: &CoSimulationValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError>;
}

pub struct CoSimulationScheduler {
    config: CoSimulationConfig,
    participants: BTreeMap<String, Box<dyn CoSimulationParticipant>>,
    links: Vec<CoSimulationLink>,
}

impl CoSimulationScheduler {
    pub fn new(config: CoSimulationConfig) -> Self {
        Self {
            config,
            participants: BTreeMap::new(),
            links: Vec::new(),
        }
    }

    pub const fn config(&self) -> CoSimulationConfig {
        self.config
    }

    pub fn add_participant<P>(&mut self, participant: P) -> Result<(), EngineError>
    where
        P: CoSimulationParticipant + 'static,
    {
        let id = participant.id().trim().to_owned();
        if id.is_empty() {
            return Err(EngineError::new(
                "cosim_participant_invalid",
                "co-simulation participant id must not be empty",
            ));
        }
        if self.participants.contains_key(&id) {
            return Err(EngineError::new(
                "cosim_participant_duplicate",
                format!("co-simulation participant `{id}` is already registered"),
            ));
        }
        validate_ports(&id, participant.ports())?;
        self.participants.insert(id, Box::new(participant));
        Ok(())
    }

    pub fn connect(&mut self, link: CoSimulationLink) -> &mut Self {
        self.links.push(link);
        self
    }

    pub fn run(&mut self, control: &ExecutionControl) -> Result<CoSimulationReport, EngineError> {
        self.config.validate()?;
        control.policy.validate()?;
        let started = Instant::now();
        check_control(control, started)?;

        let links = self.validated_links()?;
        for (id, participant) in &mut self.participants {
            participant
                .initialize(control)
                .map_err(|error| participant_error(id, "initialize", error))?;
            if participant.current_time() != CoSimulationTime::ZERO {
                return Err(EngineError::new(
                    "cosim_participant_time_invalid",
                    format!(
                        "participant `{id}` initialized at {} ps instead of time zero",
                        participant.current_time().as_picoseconds()
                    ),
                ));
            }
        }

        let mut stats = CoSimulationStats::default();
        let mut recorder = WaveformRecorder::new(&self.participants)?;
        stats.delta_cycles +=
            self.settle_links(&links, CoSimulationTime::ZERO, control, started)?;
        recorder.record(CoSimulationTime::ZERO, &self.participants)?;

        let mut now = CoSimulationTime::ZERO;
        while now < self.config.stop {
            check_control(control, started)?;
            if stats.macro_steps >= self.config.max_steps {
                return Err(EngineError::new(
                    "cosim_step_limit",
                    format!(
                        "co-simulation exceeded the configured {} macro-step limit",
                        self.config.max_steps
                    ),
                ));
            }

            let target = self.next_time(now)?;
            if target <= now {
                return Err(EngineError::new(
                    "cosim_time_stalled",
                    format!(
                        "co-simulation could not advance beyond {} ps",
                        now.as_picoseconds()
                    ),
                ));
            }

            for (id, participant) in &mut self.participants {
                check_control(control, started)?;
                participant
                    .advance_to(target, control)
                    .map_err(|error| participant_error(id, "advance", error))?;
                stats.participant_advances += 1;
                if participant.current_time() != target {
                    return Err(EngineError::new(
                        "cosim_participant_time_invalid",
                        format!(
                            "participant `{id}` advanced to {} ps instead of requested {} ps",
                            participant.current_time().as_picoseconds(),
                            target.as_picoseconds()
                        ),
                    ));
                }
            }

            stats.delta_cycles += self.settle_links(&links, target, control, started)?;
            recorder.record(target, &self.participants)?;
            stats.macro_steps += 1;
            now = target;
        }

        let waveforms = recorder.finish();
        let diagnostics = vec![Diagnostic {
            level: DiagnosticLevel::Info,
            code: "cosim_deterministic_scheduler".to_owned(),
            message: format!(
                "co-simulation completed {} macro step(s), {} delta cycle(s), and {} participant advance(s) across {} participant(s)",
                stats.macro_steps,
                stats.delta_cycles,
                stats.participant_advances,
                self.participants.len()
            ),
        }];
        let result = SimulationResult::new(
            EngineId::new(COSIM_SCHEDULER_ID),
            Some(VERSION.to_owned()),
            AnalysisKind::CoSimulationTransient,
            CIRCUIT_SCHEMA_VERSION,
            waveforms,
            diagnostics,
        );
        Ok(CoSimulationReport {
            result,
            scheduler: stats,
        })
    }

    fn validated_links(&self) -> Result<Vec<CoSimulationLink>, EngineError> {
        if self.participants.is_empty() {
            return Err(EngineError::new(
                "cosim_participant_missing",
                "co-simulation requires at least one participant",
            ));
        }

        let mut links = self.links.clone();
        links.sort();
        let mut targets = BTreeSet::new();
        for link in &links {
            let source = find_port(&self.participants, &link.source)?;
            let target = find_port(&self.participants, &link.target)?;
            if !matches!(
                source.direction,
                PinDirection::Output | PinDirection::Bidirectional
            ) {
                return Err(EngineError::new(
                    "cosim_link_invalid",
                    format!(
                        "source `{}.{}` is not an output/bidirectional port",
                        link.source.participant, link.source.port
                    ),
                ));
            }
            if !matches!(
                target.direction,
                PinDirection::Input | PinDirection::Bidirectional
            ) {
                return Err(EngineError::new(
                    "cosim_link_invalid",
                    format!(
                        "target `{}.{}` is not an input/bidirectional port",
                        link.target.participant, link.target.port
                    ),
                ));
            }
            if source.domain != target.domain {
                return Err(EngineError::new(
                    "cosim_domain_mismatch",
                    format!(
                        "link `{}.{}` -> `{}.{}` crosses {:?} -> {:?}; use an explicit bridge participant",
                        link.source.participant,
                        link.source.port,
                        link.target.participant,
                        link.target.port,
                        source.domain,
                        target.domain
                    ),
                ));
            }
            if source.unit != target.unit && source.domain == SignalDomain::Analog {
                return Err(EngineError::new(
                    "cosim_unit_mismatch",
                    format!(
                        "analog link `{}.{}` -> `{}.{}` has incompatible units",
                        link.source.participant,
                        link.source.port,
                        link.target.participant,
                        link.target.port
                    ),
                ));
            }
            if !targets.insert(link.target.clone()) {
                return Err(EngineError::new(
                    "cosim_multiple_drivers",
                    format!(
                        "target `{}.{}` has more than one scheduler link; resolve drivers explicitly inside a participant",
                        link.target.participant, link.target.port
                    ),
                ));
            }
        }
        Ok(links)
    }

    fn next_time(&self, now: CoSimulationTime) -> Result<CoSimulationTime, EngineError> {
        let mut target = now
            .saturating_add(self.config.max_step)
            .min(self.config.stop);
        for (id, participant) in &self.participants {
            if let Some(event) = participant.next_event_time() {
                if event <= now {
                    return Err(EngineError::new(
                        "cosim_event_time_invalid",
                        format!(
                            "participant `{id}` reported next event {} ps at current time {} ps; next_event_time must be strictly future",
                            event.as_picoseconds(),
                            now.as_picoseconds()
                        ),
                    ));
                }
                target = target.min(event);
            }
        }
        Ok(target)
    }

    fn settle_links(
        &mut self,
        links: &[CoSimulationLink],
        time: CoSimulationTime,
        control: &ExecutionControl,
        started: Instant,
    ) -> Result<usize, EngineError> {
        for changed_cycles in 0..self.config.max_delta_cycles {
            check_control(control, started)?;
            let mut deliveries = Vec::with_capacity(links.len());
            for link in links {
                let source = self
                    .participants
                    .get(&link.source.participant)
                    .expect("validated co-simulation source participant");
                let value = source
                    .read_output(&link.source.port)
                    .map_err(|error| participant_error(source.id(), "read output", error))?;
                value.validate()?;
                let source_port = source
                    .ports()
                    .iter()
                    .find(|port| port.name == link.source.port)
                    .expect("validated co-simulation source port");
                if value.domain() != source_port.domain {
                    return Err(EngineError::new(
                        "cosim_value_domain_mismatch",
                        format!(
                            "participant `{}.{}` returned {:?} for a {:?} port",
                            link.source.participant,
                            link.source.port,
                            value.domain(),
                            source_port.domain
                        ),
                    ));
                }
                deliveries.push((link.target.clone(), value));
            }

            let mut changed = false;
            for (target_endpoint, value) in deliveries {
                let target = self
                    .participants
                    .get_mut(&target_endpoint.participant)
                    .expect("validated co-simulation target participant");
                let target_id = target.id().to_owned();
                changed |= target
                    .write_input(&target_endpoint.port, time, &value, control)
                    .map_err(|error| participant_error(&target_id, "write input", error))?;
                if target.current_time() != time {
                    return Err(EngineError::new(
                        "cosim_participant_time_invalid",
                        format!(
                            "participant `{target_id}` changed time while settling same-time inputs"
                        ),
                    ));
                }
            }

            if !changed {
                return Ok(changed_cycles);
            }
        }

        Err(EngineError::new(
            "cosim_delta_cycle_limit",
            format!(
                "co-simulation did not settle within {} same-time delta cycles at {} ps",
                self.config.max_delta_cycles,
                time.as_picoseconds()
            ),
        ))
    }
}

fn validate_ports(id: &str, ports: &[CoSimulationPort]) -> Result<(), EngineError> {
    if ports.is_empty() {
        return Err(EngineError::new(
            "cosim_participant_invalid",
            format!("participant `{id}` must expose at least one port"),
        ));
    }
    let mut names = BTreeSet::new();
    for port in ports {
        if port.name.trim().is_empty() {
            return Err(EngineError::new(
                "cosim_port_invalid",
                format!("participant `{id}` contains an empty port name"),
            ));
        }
        if !names.insert(port.name.clone()) {
            return Err(EngineError::new(
                "cosim_port_duplicate",
                format!("participant `{id}` contains duplicate port `{}`", port.name),
            ));
        }
        match port.domain {
            SignalDomain::Digital => {
                if port.unit.is_some() {
                    return Err(EngineError::new(
                        "cosim_port_invalid",
                        format!(
                            "digital port `{id}.{}` must not declare an analog unit",
                            port.name
                        ),
                    ));
                }
            }
            SignalDomain::Analog => {
                if port.unit.is_none() {
                    return Err(EngineError::new(
                        "cosim_port_invalid",
                        format!("analog port `{id}.{}` must declare a unit", port.name),
                    ));
                }
            }
            SignalDomain::Mixed | SignalDomain::Reference => {
                return Err(EngineError::new(
                    "cosim_port_invalid",
                    format!(
                        "scheduler port `{id}.{}` must be explicitly analog or digital; mixed/reference boundaries belong inside participants",
                        port.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn find_port<'a>(
    participants: &'a BTreeMap<String, Box<dyn CoSimulationParticipant>>,
    endpoint: &CoSimulationEndpoint,
) -> Result<&'a CoSimulationPort, EngineError> {
    let participant = participants.get(&endpoint.participant).ok_or_else(|| {
        EngineError::new(
            "cosim_link_invalid",
            format!(
                "co-simulation link references unknown participant `{}`",
                endpoint.participant
            ),
        )
    })?;
    participant
        .ports()
        .iter()
        .find(|port| port.name == endpoint.port)
        .ok_or_else(|| {
            EngineError::new(
                "cosim_link_invalid",
                format!(
                    "co-simulation link references unknown port `{}.{}`",
                    endpoint.participant, endpoint.port
                ),
            )
        })
}

fn participant_error(id: &str, operation: &str, error: EngineError) -> EngineError {
    EngineError {
        code: error.code,
        message: format!(
            "participant `{id}` failed to {operation}: {}",
            error.message
        ),
        retryable: error.retryable,
    }
}

fn check_control(control: &ExecutionControl, started: Instant) -> Result<(), EngineError> {
    if control.cancellation.is_cancelled() {
        return Err(EngineError::new(
            "execution_cancelled",
            "co-simulation execution was cancelled",
        ));
    }
    if control.policy.timeout_ms > 0
        && started.elapsed() >= Duration::from_millis(control.policy.timeout_ms)
    {
        return Err(EngineError::new(
            "execution_timeout",
            format!(
                "co-simulation exceeded {} ms execution timeout",
                control.policy.timeout_ms
            ),
        ));
    }
    Ok(())
}

enum RecordedSignal {
    Digital {
        signal: SignalId,
        transitions: Vec<DigitalTransition>,
    },
    Analog {
        signal: SignalId,
        unit: Unit,
        times: Vec<f64>,
        values: Vec<f64>,
    },
}

struct WaveformRecorder {
    signals: BTreeMap<(String, String), RecordedSignal>,
}

impl WaveformRecorder {
    fn new(
        participants: &BTreeMap<String, Box<dyn CoSimulationParticipant>>,
    ) -> Result<Self, EngineError> {
        let mut signals = BTreeMap::new();
        for (participant_id, participant) in participants {
            for port in participant.ports() {
                if !matches!(
                    port.direction,
                    PinDirection::Output | PinDirection::Bidirectional
                ) {
                    continue;
                }
                let signal = SignalId::new(format!("{participant_id}.{}", port.name));
                let recorded = match port.domain {
                    SignalDomain::Digital => RecordedSignal::Digital {
                        signal,
                        transitions: Vec::new(),
                    },
                    SignalDomain::Analog => RecordedSignal::Analog {
                        signal,
                        unit: port.unit.ok_or_else(|| {
                            EngineError::new(
                                "cosim_port_invalid",
                                format!(
                                    "analog output `{}.{}` is missing a unit",
                                    participant_id, port.name
                                ),
                            )
                        })?,
                        times: Vec::new(),
                        values: Vec::new(),
                    },
                    SignalDomain::Mixed | SignalDomain::Reference => {
                        return Err(EngineError::new(
                            "cosim_port_invalid",
                            format!(
                                "output `{}.{}` must be explicitly analog or digital",
                                participant_id, port.name
                            ),
                        ));
                    }
                };
                signals.insert((participant_id.clone(), port.name.clone()), recorded);
            }
        }
        Ok(Self { signals })
    }

    fn record(
        &mut self,
        time: CoSimulationTime,
        participants: &BTreeMap<String, Box<dyn CoSimulationParticipant>>,
    ) -> Result<(), EngineError> {
        for ((participant_id, port_name), recorded) in &mut self.signals {
            let participant = participants
                .get(participant_id)
                .expect("recorder participant exists");
            let value = participant
                .read_output(port_name)
                .map_err(|error| participant_error(participant_id, "record output", error))?;
            value.validate()?;
            match (recorded, value) {
                (
                    RecordedSignal::Digital { transitions, .. },
                    CoSimulationValue::Digital(value),
                ) => {
                    if transitions.last().is_none_or(|last| last.value != value) {
                        transitions.push(DigitalTransition {
                            time: time.as_seconds(),
                            value,
                        });
                    }
                }
                (
                    RecordedSignal::Analog { times, values, .. },
                    CoSimulationValue::Analog(value),
                ) => {
                    let seconds = time.as_seconds();
                    if times.last().is_some_and(|last| *last == seconds) {
                        if let Some(last) = values.last_mut() {
                            *last = value;
                        }
                    } else {
                        times.push(seconds);
                        values.push(value);
                    }
                }
                (_, value) => {
                    return Err(EngineError::new(
                        "cosim_value_domain_mismatch",
                        format!(
                            "participant `{participant_id}.{port_name}` returned {:?} while recording a different port domain",
                            value.domain()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<Waveform> {
        self.signals
            .into_values()
            .map(|signal| match signal {
                RecordedSignal::Digital {
                    signal,
                    transitions,
                } => Waveform::Digital(DigitalWaveform {
                    signal,
                    transitions,
                }),
                RecordedSignal::Analog {
                    signal,
                    unit,
                    times,
                    values,
                } => Waveform::Analog(AnalogWaveform {
                    signal,
                    unit,
                    axis: AnalogAxis {
                        kind: crate::AxisKind::Time,
                        unit: Unit::Second,
                        values: times,
                    },
                    values,
                    imaginary: None,
                }),
            })
            .collect()
    }
}
