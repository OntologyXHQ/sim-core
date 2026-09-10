use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    Analysis, AnalysisKind, Circuit, Component, ComponentKind, Diagnostic, DiagnosticLevel,
    DigitalTransition, DigitalWaveform, EngineCapabilities, EngineError, EngineId,
    ExecutionControl, LogicValue, ParameterValue, PinDirection, Probe, Quantity, SignalDomain,
    SignalId, SimulationEngine, SimulationRequest, SimulationResult, Unit, VERSION, Waveform,
};

pub const DIGITAL_ENGINE_ID: &str = "digital";
pub const MAX_DIGITAL_EVENTS: usize = 1_000_000;
pub const MAX_DIGITAL_BUS_WIDTH: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicVector(pub Vec<LogicValue>);

impl LogicVector {
    pub fn new(values: Vec<LogicValue>) -> Self {
        Self(values)
    }

    pub fn width(&self) -> usize {
        self.0.len()
    }

    pub fn from_u64(width: usize, value: u64) -> Result<Self, EngineError> {
        validate_width(width)?;
        Ok(Self(
            (0..width)
                .map(|bit| {
                    if (value >> bit) & 1 == 1 {
                        LogicValue::One
                    } else {
                        LogicValue::Zero
                    }
                })
                .collect(),
        ))
    }

    pub fn to_u64(&self) -> Option<u64> {
        if self.0.len() > 64 || self.0.iter().any(|value| !value.is_known()) {
            return None;
        }
        let mut result = 0_u64;
        for (bit, value) in self.0.iter().enumerate() {
            if *value == LogicValue::One {
                result |= 1_u64 << bit;
            }
        }
        Some(result)
    }

    pub fn values(&self) -> &[LogicValue] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GateKind {
    Buffer,
    Not,
    And,
    Or,
    Xor,
    Nand,
    Nor,
    Xnor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EdgeKind {
    Rising,
    Falling,
}

#[derive(Clone, Debug)]
enum DigitalPrimitive {
    Constant(LogicValue),
    Clock {
        period: f64,
        duty_cycle: f64,
        initial: LogicValue,
    },
    Gate(GateKind),
    TriState,
    Mux2,
    Demux2,
    Decoder2To4,
    DLatch,
    SRLatch,
    DFlipFlop {
        edge: EdgeKind,
    },
    JKFlipFlop {
        edge: EdgeKind,
    },
    TFlipFlop {
        edge: EdgeKind,
    },
    SRFlipFlop {
        edge: EdgeKind,
    },
    Register {
        width: usize,
        edge: EdgeKind,
    },
    Counter {
        width: usize,
        edge: EdgeKind,
    },
    Sink,
}

#[derive(Clone, Debug)]
struct CompiledDigitalComponent {
    primitive: DigitalPrimitive,
    input_nets: BTreeMap<String, String>,
    output_nets: Vec<(String, String)>,
    delay: f64,
    initial_scalar: LogicValue,
    initial_vector: Vec<LogicValue>,
}

#[derive(Clone, Debug)]
struct RuntimeState {
    q: LogicValue,
    bits: Vec<LogicValue>,
    previous_clock: LogicValue,
}

impl RuntimeState {
    fn for_component(component: &CompiledDigitalComponent) -> Self {
        Self {
            q: component.initial_scalar,
            bits: component.initial_vector.clone(),
            previous_clock: LogicValue::X,
        }
    }
}

impl CompiledDigitalComponent {
    fn input(&self, pin: &str, net_values: &BTreeMap<String, LogicValue>) -> LogicValue {
        self.input_nets
            .get(pin)
            .and_then(|net| net_values.get(net))
            .copied()
            .unwrap_or(LogicValue::Zero)
    }

    fn evaluate(
        &self,
        net_values: &BTreeMap<String, LogicValue>,
        state: &mut RuntimeState,
    ) -> Option<Vec<LogicValue>> {
        let output = match self.primitive {
            DigitalPrimitive::Gate(kind) => {
                let value = match kind {
                    GateKind::Buffer => self.input("in", net_values).logic_buffer(),
                    GateKind::Not => self.input("in", net_values).logic_not(),
                    GateKind::And => self
                        .input("a", net_values)
                        .logic_and(self.input("b", net_values)),
                    GateKind::Or => self
                        .input("a", net_values)
                        .logic_or(self.input("b", net_values)),
                    GateKind::Xor => self
                        .input("a", net_values)
                        .logic_xor(self.input("b", net_values)),
                    GateKind::Nand => self
                        .input("a", net_values)
                        .logic_and(self.input("b", net_values))
                        .logic_not(),
                    GateKind::Nor => self
                        .input("a", net_values)
                        .logic_or(self.input("b", net_values))
                        .logic_not(),
                    GateKind::Xnor => self
                        .input("a", net_values)
                        .logic_xor(self.input("b", net_values))
                        .logic_not(),
                };
                vec![value]
            }
            DigitalPrimitive::TriState => {
                let input = self.input("in", net_values);
                let enable = self.input("enable", net_values).logic_buffer();
                vec![match enable {
                    LogicValue::Zero => LogicValue::Z,
                    LogicValue::One => input.logic_buffer(),
                    LogicValue::X | LogicValue::Z => LogicValue::X,
                }]
            }
            DigitalPrimitive::Mux2 => {
                let select = self.input("sel", net_values).logic_buffer();
                vec![match select {
                    LogicValue::Zero => self.input("a", net_values).logic_buffer(),
                    LogicValue::One => self.input("b", net_values).logic_buffer(),
                    LogicValue::X | LogicValue::Z => {
                        let a = self.input("a", net_values).logic_buffer();
                        let b = self.input("b", net_values).logic_buffer();
                        if a == b { a } else { LogicValue::X }
                    }
                }]
            }
            DigitalPrimitive::Demux2 => {
                let input = self.input("in", net_values).logic_buffer();
                let select = self.input("sel", net_values).logic_buffer();
                match select {
                    LogicValue::Zero => vec![input, LogicValue::Zero],
                    LogicValue::One => vec![LogicValue::Zero, input],
                    LogicValue::X | LogicValue::Z => vec![LogicValue::X, LogicValue::X],
                }
            }
            DigitalPrimitive::Decoder2To4 => {
                let enable = self.input("enable", net_values).logic_buffer();
                if enable == LogicValue::Zero {
                    vec![LogicValue::Zero; 4]
                } else if enable != LogicValue::One {
                    vec![LogicValue::X; 4]
                } else {
                    let a = self.input("a", net_values).logic_buffer();
                    let b = self.input("b", net_values).logic_buffer();
                    if !a.is_known() || !b.is_known() {
                        vec![LogicValue::X; 4]
                    } else {
                        let a_bit = if a == LogicValue::One { 1_usize } else { 0 };
                        let b_bit = if b == LogicValue::One { 1_usize } else { 0 };
                        let index = a_bit | (b_bit << 1);
                        (0..4)
                            .map(|position| {
                                if position == index {
                                    LogicValue::One
                                } else {
                                    LogicValue::Zero
                                }
                            })
                            .collect()
                    }
                }
            }
            DigitalPrimitive::DLatch => {
                if let Some(async_value) = async_set_reset(self, net_values) {
                    state.q = async_value;
                } else {
                    match self.input("enable", net_values).logic_buffer() {
                        LogicValue::One => state.q = self.input("d", net_values).logic_buffer(),
                        LogicValue::Zero => {}
                        LogicValue::X | LogicValue::Z => state.q = LogicValue::X,
                    }
                }
                q_outputs(state.q, self.output_nets.len())
            }
            DigitalPrimitive::SRLatch => {
                if let Some(async_value) = async_set_reset(self, net_values) {
                    state.q = async_value;
                } else {
                    state.q = sr_next(
                        state.q,
                        self.input("s", net_values),
                        self.input("r", net_values),
                    );
                }
                q_outputs(state.q, self.output_nets.len())
            }
            DigitalPrimitive::DFlipFlop { edge } => {
                let clock = self.input("clk", net_values).logic_buffer();
                if let Some(async_value) = async_set_reset(self, net_values) {
                    state.q = async_value;
                } else if is_edge(state.previous_clock, clock, edge) {
                    state.q = self.input("d", net_values).logic_buffer();
                } else if clock == LogicValue::X {
                    state.q = state.q.logic_buffer();
                }
                state.previous_clock = clock;
                q_outputs(state.q, self.output_nets.len())
            }
            DigitalPrimitive::JKFlipFlop { edge } => {
                let clock = self.input("clk", net_values).logic_buffer();
                if let Some(async_value) = async_set_reset(self, net_values) {
                    state.q = async_value;
                } else if is_edge(state.previous_clock, clock, edge) {
                    let j = self.input("j", net_values).logic_buffer();
                    let k = self.input("k", net_values).logic_buffer();
                    state.q = match (j, k) {
                        (LogicValue::Zero, LogicValue::Zero) => state.q,
                        (LogicValue::Zero, LogicValue::One) => LogicValue::Zero,
                        (LogicValue::One, LogicValue::Zero) => LogicValue::One,
                        (LogicValue::One, LogicValue::One) => toggle(state.q),
                        _ => LogicValue::X,
                    };
                }
                state.previous_clock = clock;
                q_outputs(state.q, self.output_nets.len())
            }
            DigitalPrimitive::TFlipFlop { edge } => {
                let clock = self.input("clk", net_values).logic_buffer();
                if let Some(async_value) = async_set_reset(self, net_values) {
                    state.q = async_value;
                } else if is_edge(state.previous_clock, clock, edge) {
                    state.q = match self.input("t", net_values).logic_buffer() {
                        LogicValue::Zero => state.q,
                        LogicValue::One => toggle(state.q),
                        LogicValue::X | LogicValue::Z => LogicValue::X,
                    };
                }
                state.previous_clock = clock;
                q_outputs(state.q, self.output_nets.len())
            }
            DigitalPrimitive::SRFlipFlop { edge } => {
                let clock = self.input("clk", net_values).logic_buffer();
                if let Some(async_value) = async_set_reset(self, net_values) {
                    state.q = async_value;
                } else if is_edge(state.previous_clock, clock, edge) {
                    state.q = sr_next(
                        state.q,
                        self.input("s", net_values),
                        self.input("r", net_values),
                    );
                }
                state.previous_clock = clock;
                q_outputs(state.q, self.output_nets.len())
            }
            DigitalPrimitive::Register { width, edge } => {
                let clock = self.input("clk", net_values).logic_buffer();
                if self.input_nets.contains_key("reset")
                    && self.input("reset", net_values) == LogicValue::One
                {
                    state.bits = vec![LogicValue::Zero; width];
                } else if is_edge(state.previous_clock, clock, edge) {
                    let enable = if self.input_nets.contains_key("enable") {
                        self.input("enable", net_values).logic_buffer()
                    } else {
                        LogicValue::One
                    };
                    match enable {
                        LogicValue::Zero => {}
                        LogicValue::One => {
                            state.bits = (0..width)
                                .map(|bit| {
                                    self.input(&format!("d{bit}"), net_values).logic_buffer()
                                })
                                .collect();
                        }
                        LogicValue::X | LogicValue::Z => {
                            state.bits = vec![LogicValue::X; width];
                        }
                    }
                }
                state.previous_clock = clock;
                state.bits.clone()
            }
            DigitalPrimitive::Counter { width, edge } => {
                let clock = self.input("clk", net_values).logic_buffer();
                if self.input_nets.contains_key("reset")
                    && self.input("reset", net_values) == LogicValue::One
                {
                    state.bits = vec![LogicValue::Zero; width];
                } else if is_edge(state.previous_clock, clock, edge) {
                    let enable = if self.input_nets.contains_key("enable") {
                        self.input("enable", net_values).logic_buffer()
                    } else {
                        LogicValue::One
                    };
                    state.bits = match enable {
                        LogicValue::Zero => state.bits.clone(),
                        LogicValue::One => increment_bits(&state.bits),
                        LogicValue::X | LogicValue::Z => vec![LogicValue::X; width],
                    };
                }
                state.previous_clock = clock;
                state.bits.clone()
            }
            DigitalPrimitive::Constant(_)
            | DigitalPrimitive::Clock { .. }
            | DigitalPrimitive::Sink => return None,
        };
        Some(output)
    }
}

#[derive(Clone, Debug)]
struct CompiledProbe {
    signal: String,
    net: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DriverKey {
    component: usize,
    output: usize,
}

#[derive(Clone, Debug)]
struct DigitalProgram {
    components: Vec<CompiledDigitalComponent>,
    dependents: BTreeMap<String, Vec<usize>>,
    net_drivers: BTreeMap<String, Vec<DriverKey>>,
    nets: Vec<String>,
    probes: Vec<CompiledProbe>,
}

impl DigitalProgram {
    fn compile(request: &SimulationRequest) -> Result<Self, EngineError> {
        let Analysis::DigitalTransient { stop } = &request.analysis else {
            return Err(EngineError::new(
                "digital_unsupported_analysis",
                "the built-in digital engine only supports digital_transient analysis",
            ));
        };
        if !stop.is_finite() || *stop <= 0.0 {
            return Err(EngineError::new(
                "digital_invalid_analysis",
                "digital transient stop time must be finite and greater than zero",
            ));
        }

        let endpoint_net = endpoint_net_map(&request.circuit)?;
        let mut components = Vec::with_capacity(request.circuit.components.len());
        let mut dependents: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut net_drivers: BTreeMap<String, Vec<DriverKey>> = BTreeMap::new();

        for component in &request.circuit.components {
            let compiled = compile_component(component, &endpoint_net)?;
            let component_index = components.len();
            for net in compiled.input_nets.values() {
                dependents
                    .entry(net.clone())
                    .or_default()
                    .push(component_index);
            }
            for (output_index, (_, net)) in compiled.output_nets.iter().enumerate() {
                net_drivers.entry(net.clone()).or_default().push(DriverKey {
                    component: component_index,
                    output: output_index,
                });
            }
            components.push(compiled);
        }

        let nets = request
            .circuit
            .nets
            .iter()
            .map(|net| net.id.as_str().to_owned())
            .collect::<Vec<_>>();
        if nets.is_empty() {
            return Err(EngineError::new(
                "digital_no_nets",
                "digital simulation requires at least one connected net",
            ));
        }

        let probes = compile_probes(request, &endpoint_net)?;
        Ok(Self {
            components,
            dependents,
            net_drivers,
            nets,
            probes,
        })
    }
}

#[derive(Clone, Debug)]
struct ScheduledEvent {
    time: f64,
    sequence: u64,
    driver: DriverKey,
    value: LogicValue,
}

impl PartialEq for ScheduledEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time.to_bits() == other.time.to_bits() && self.sequence == other.sequence
    }
}

impl Eq for ScheduledEvent {}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .time
            .total_cmp(&self.time)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

#[derive(Clone, Debug, Default)]
pub struct DigitalEngine;

impl DigitalEngine {
    pub fn new() -> Self {
        Self
    }

    fn run(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, EngineError> {
        control.policy.validate()?;
        if control.cancellation.is_cancelled() {
            return Err(execution_cancelled());
        }

        let Analysis::DigitalTransient { stop } = &request.analysis else {
            return Err(EngineError::new(
                "digital_unsupported_analysis",
                "the built-in digital engine only supports digital_transient analysis",
            ));
        };
        let stop = *stop;
        let program = DigitalProgram::compile(request)?;
        let started = Instant::now();

        let mut net_values = program
            .nets
            .iter()
            .map(|net| (net.clone(), LogicValue::X))
            .collect::<BTreeMap<_, _>>();
        let mut histories = program
            .nets
            .iter()
            .map(|net| {
                (
                    net.clone(),
                    vec![DigitalTransition {
                        time: 0.0,
                        value: LogicValue::X,
                    }],
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut driver_values = BTreeMap::<DriverKey, LogicValue>::new();
        for drivers in program.net_drivers.values() {
            for driver in drivers {
                driver_values.insert(*driver, LogicValue::Z);
            }
        }
        let mut runtime_states = program
            .components
            .iter()
            .map(RuntimeState::for_component)
            .collect::<Vec<_>>();

        let mut queue = BinaryHeap::new();
        let mut sequence = 0_u64;
        let mut scheduled_events = 0_usize;

        for (component_index, component) in program.components.iter().enumerate() {
            match component.primitive {
                DigitalPrimitive::Constant(value) => {
                    schedule_output(
                        &mut queue,
                        &mut sequence,
                        &mut scheduled_events,
                        0.0,
                        component_index,
                        0,
                        value,
                    )?;
                }
                DigitalPrimitive::Clock {
                    period,
                    duty_cycle,
                    initial,
                } => {
                    schedule_clock(
                        &mut queue,
                        &mut sequence,
                        &mut scheduled_events,
                        component_index,
                        stop,
                        period,
                        duty_cycle,
                        initial,
                        control,
                        started,
                    )?;
                }
                DigitalPrimitive::DLatch
                | DigitalPrimitive::SRLatch
                | DigitalPrimitive::DFlipFlop { .. }
                | DigitalPrimitive::JKFlipFlop { .. }
                | DigitalPrimitive::TFlipFlop { .. }
                | DigitalPrimitive::SRFlipFlop { .. }
                | DigitalPrimitive::Register { .. }
                | DigitalPrimitive::Counter { .. } => {
                    let outputs = initial_outputs(component, &runtime_states[component_index]);
                    for (output_index, value) in outputs.into_iter().enumerate() {
                        schedule_output(
                            &mut queue,
                            &mut sequence,
                            &mut scheduled_events,
                            0.0,
                            component_index,
                            output_index,
                            value,
                        )?;
                    }
                }
                DigitalPrimitive::Gate(_)
                | DigitalPrimitive::TriState
                | DigitalPrimitive::Mux2
                | DigitalPrimitive::Demux2
                | DigitalPrimitive::Decoder2To4
                | DigitalPrimitive::Sink => {}
            }
        }

        let mut processed_events = 0_usize;
        while let Some(first) = queue.pop() {
            check_execution(control, started)?;
            if first.time > stop {
                break;
            }
            let time = first.time;
            let mut batch = vec![first];
            while queue
                .peek()
                .is_some_and(|event| event.time.to_bits() == time.to_bits())
            {
                batch.push(queue.pop().expect("peeked event exists"));
            }

            processed_events += batch.len();
            if processed_events > MAX_DIGITAL_EVENTS {
                return Err(event_limit_error());
            }

            let mut affected_nets = BTreeSet::new();
            for event in batch {
                let previous = driver_values
                    .insert(event.driver, event.value)
                    .unwrap_or(LogicValue::Z);
                if previous != event.value {
                    let net = &program.components[event.driver.component].output_nets
                        [event.driver.output]
                        .1;
                    affected_nets.insert(net.clone());
                }
            }

            let mut affected_components = BTreeSet::new();
            for net in affected_nets {
                let resolved = resolve_net(
                    program
                        .net_drivers
                        .get(&net)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    &driver_values,
                );
                let current = net_values.get(&net).copied().unwrap_or(LogicValue::X);
                if resolved == current {
                    continue;
                }
                net_values.insert(net.clone(), resolved);
                push_transition(
                    histories
                        .get_mut(&net)
                        .expect("compiled digital net owns history"),
                    time,
                    resolved,
                );
                enforce_transition_budget(&histories, control)?;
                if let Some(dependents) = program.dependents.get(&net) {
                    affected_components.extend(dependents.iter().copied());
                }
            }

            for component_index in affected_components {
                let component = &program.components[component_index];
                let Some(values) =
                    component.evaluate(&net_values, &mut runtime_states[component_index])
                else {
                    continue;
                };
                for (output_index, value) in values.into_iter().enumerate() {
                    if output_index >= component.output_nets.len() {
                        break;
                    }
                    let output_time = time + component.delay;
                    if output_time <= stop {
                        schedule_output(
                            &mut queue,
                            &mut sequence,
                            &mut scheduled_events,
                            output_time,
                            component_index,
                            output_index,
                            value,
                        )?;
                    }
                }
            }
        }

        let waveforms = program
            .probes
            .into_iter()
            .map(|probe| {
                Waveform::Digital(DigitalWaveform {
                    signal: SignalId::new(probe.signal),
                    transitions: histories
                        .get(&probe.net)
                        .cloned()
                        .expect("compiled probe references known net"),
                })
            })
            .collect::<Vec<_>>();

        let mut diagnostics = Vec::<Diagnostic>::new();
        if program
            .net_drivers
            .values()
            .any(|drivers| drivers.len() > 1)
        {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Info,
                code: "digital_multi_driver_resolution".to_owned(),
                message: "digital nets with multiple drivers were resolved with four-state Z/X contention semantics".to_owned(),
            });
        }

        Ok(SimulationResult::new(
            self.id(),
            self.version(),
            request.analysis.kind(),
            request.circuit.schema_version,
            waveforms,
            diagnostics,
        ))
    }
}

impl SimulationEngine for DigitalEngine {
    fn id(&self) -> EngineId {
        EngineId::new(DIGITAL_ENGINE_ID)
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: false,
            digital: true,
            mixed_signal: false,
            analyses: BTreeSet::from([AnalysisKind::DigitalTransient]),
        }
    }

    fn version(&self) -> Option<String> {
        Some(VERSION.to_owned())
    }

    fn supports_request(&self, request: &SimulationRequest) -> bool {
        self.capabilities().supports(&request.analysis)
            && !request
                .circuit
                .components
                .iter()
                .any(|component| component.kind == ComponentKind::hdl_module())
    }

    fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult, EngineError> {
        self.run(request, &ExecutionControl::default())
    }

    fn simulate_with_control(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, EngineError> {
        self.run(request, control)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DigitalCoSimulationBinding {
    pub port: String,
    pub net: String,
    pub direction: PinDirection,
}

impl DigitalCoSimulationBinding {
    pub fn input(port: impl Into<String>, net: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            net: net.into(),
            direction: PinDirection::Input,
        }
    }

    pub fn output(port: impl Into<String>, net: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            net: net.into(),
            direction: PinDirection::Output,
        }
    }
}

pub struct DigitalCoSimulationParticipant {
    id: String,
    ports: Vec<crate::CoSimulationPort>,
    request: SimulationRequest,
    bindings: BTreeMap<String, DigitalCoSimulationBinding>,
    session: Option<DigitalParticipantSession>,
}

impl DigitalCoSimulationParticipant {
    pub fn new(
        id: impl Into<String>,
        request: SimulationRequest,
        bindings: Vec<DigitalCoSimulationBinding>,
    ) -> Result<Self, EngineError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(EngineError::new(
                "digital_cosim_participant_invalid",
                "digital co-simulation participant id must not be empty",
            ));
        }
        let program = DigitalProgram::compile(&request)?;
        let mut ports = Vec::with_capacity(bindings.len());
        let mut by_name = BTreeMap::new();
        for binding in bindings {
            if binding.port.trim().is_empty() || binding.net.trim().is_empty() {
                return Err(EngineError::new(
                    "digital_cosim_binding_invalid",
                    "digital co-simulation port and net names must not be empty",
                ));
            }
            if !program.nets.contains(&binding.net) {
                return Err(EngineError::new(
                    "digital_cosim_binding_invalid",
                    format!(
                        "digital co-simulation port `{}` references unknown net `{}`",
                        binding.port, binding.net
                    ),
                ));
            }
            if !matches!(
                binding.direction,
                PinDirection::Input | PinDirection::Output
            ) {
                return Err(EngineError::new(
                    "digital_cosim_binding_invalid",
                    format!(
                        "digital co-simulation port `{}` must be input or output",
                        binding.port
                    ),
                ));
            }
            if binding.direction == PinDirection::Input
                && program
                    .net_drivers
                    .get(&binding.net)
                    .is_some_and(|drivers| !drivers.is_empty())
            {
                return Err(EngineError::new(
                    "digital_cosim_input_driven",
                    format!(
                        "external input net `{}` already has an internal digital driver",
                        binding.net
                    ),
                ));
            }
            let port_name = binding.port.clone();
            if by_name.insert(port_name.clone(), binding).is_some() {
                return Err(EngineError::new(
                    "digital_cosim_binding_duplicate",
                    format!("digital co-simulation port `{port_name}` is mapped more than once"),
                ));
            }
            let binding = by_name.get(&port_name).expect("inserted binding");
            ports.push(crate::CoSimulationPort::digital(
                port_name,
                binding.direction,
            ));
        }
        if ports.is_empty() {
            return Err(EngineError::new(
                "digital_cosim_binding_missing",
                "digital co-simulation participant requires at least one port binding",
            ));
        }
        Ok(Self {
            id,
            ports,
            request,
            bindings: by_name,
            session: None,
        })
    }
}

impl crate::CoSimulationParticipant for DigitalCoSimulationParticipant {
    fn id(&self) -> &str {
        &self.id
    }

    fn ports(&self) -> &[crate::CoSimulationPort] {
        &self.ports
    }

    fn current_time(&self) -> crate::CoSimulationTime {
        self.session
            .as_ref()
            .map_or(crate::CoSimulationTime::ZERO, |session| {
                session.current_time
            })
    }

    fn initialize(&mut self, control: &ExecutionControl) -> Result<(), EngineError> {
        control.policy.validate()?;
        if control.cancellation.is_cancelled() {
            return Err(execution_cancelled());
        }
        let external_inputs = self
            .bindings
            .values()
            .filter(|binding| binding.direction == PinDirection::Input)
            .map(|binding| binding.net.clone())
            .collect::<BTreeSet<_>>();
        let mut session = DigitalParticipantSession::new(&self.request, external_inputs, control)?;
        session.advance_to(crate::CoSimulationTime::ZERO, control)?;
        self.session = Some(session);
        Ok(())
    }

    fn next_event_time(&self) -> Option<crate::CoSimulationTime> {
        self.session
            .as_ref()
            .and_then(DigitalParticipantSession::next_event_time)
    }

    fn advance_to(
        &mut self,
        target: crate::CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.session_mut()?.advance_to(target, control)
    }

    fn read_output(&self, port: &str) -> Result<crate::CoSimulationValue, EngineError> {
        let binding = self.bindings.get(port).ok_or_else(|| {
            EngineError::new(
                "digital_cosim_port_missing",
                format!(
                    "digital co-simulation participant `{}` has no port `{port}`",
                    self.id
                ),
            )
        })?;
        if binding.direction != PinDirection::Output {
            return Err(EngineError::new(
                "digital_cosim_port_direction",
                format!("digital co-simulation port `{port}` is not an output"),
            ));
        }
        Ok(crate::CoSimulationValue::Digital(
            self.session_ref()?.read_net(&binding.net)?,
        ))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: crate::CoSimulationTime,
        value: &crate::CoSimulationValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        let binding = self.bindings.get(port).cloned().ok_or_else(|| {
            EngineError::new(
                "digital_cosim_port_missing",
                format!(
                    "digital co-simulation participant `{}` has no port `{port}`",
                    self.id
                ),
            )
        })?;
        if binding.direction != PinDirection::Input {
            return Err(EngineError::new(
                "digital_cosim_port_direction",
                format!("digital co-simulation port `{port}` is not an input"),
            ));
        }
        let crate::CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new(
                "digital_cosim_domain_mismatch",
                format!("digital co-simulation port `{port}` requires a digital value"),
            ));
        };
        self.session_mut()?
            .write_external(&binding.net, time, *value, control)
    }
}

impl DigitalCoSimulationParticipant {
    fn session_ref(&self) -> Result<&DigitalParticipantSession, EngineError> {
        self.session.as_ref().ok_or_else(|| {
            EngineError::new(
                "digital_cosim_not_initialized",
                format!(
                    "digital co-simulation participant `{}` is not initialized",
                    self.id
                ),
            )
        })
    }

    fn session_mut(&mut self) -> Result<&mut DigitalParticipantSession, EngineError> {
        let id = self.id.clone();
        self.session.as_mut().ok_or_else(|| {
            EngineError::new(
                "digital_cosim_not_initialized",
                format!("digital co-simulation participant `{id}` is not initialized"),
            )
        })
    }
}

struct DigitalParticipantSession {
    program: DigitalProgram,
    stop: f64,
    current_time: crate::CoSimulationTime,
    net_values: BTreeMap<String, LogicValue>,
    driver_values: BTreeMap<DriverKey, LogicValue>,
    runtime_states: Vec<RuntimeState>,
    queue: BinaryHeap<ScheduledEvent>,
    sequence: u64,
    scheduled_events: usize,
    processed_events: usize,
    external_inputs: BTreeSet<String>,
    started: Instant,
}

impl DigitalParticipantSession {
    fn new(
        request: &SimulationRequest,
        external_inputs: BTreeSet<String>,
        control: &ExecutionControl,
    ) -> Result<Self, EngineError> {
        let Analysis::DigitalTransient { stop } = &request.analysis else {
            return Err(EngineError::new(
                "digital_cosim_analysis_invalid",
                "digital co-simulation participant requires digital_transient analysis",
            ));
        };
        let stop = *stop;
        let program = DigitalProgram::compile(request)?;
        for net in &external_inputs {
            if !program.nets.contains(net) {
                return Err(EngineError::new(
                    "digital_cosim_binding_invalid",
                    format!("external input references unknown net `{net}`"),
                ));
            }
            if program
                .net_drivers
                .get(net)
                .is_some_and(|drivers| !drivers.is_empty())
            {
                return Err(EngineError::new(
                    "digital_cosim_input_driven",
                    format!("external input net `{net}` already has an internal digital driver"),
                ));
            }
        }

        let net_values = program
            .nets
            .iter()
            .map(|net| (net.clone(), LogicValue::X))
            .collect::<BTreeMap<_, _>>();
        let mut driver_values = BTreeMap::<DriverKey, LogicValue>::new();
        for drivers in program.net_drivers.values() {
            for driver in drivers {
                driver_values.insert(*driver, LogicValue::Z);
            }
        }
        let runtime_states = program
            .components
            .iter()
            .map(RuntimeState::for_component)
            .collect::<Vec<_>>();
        let mut session = Self {
            program,
            stop,
            current_time: crate::CoSimulationTime::ZERO,
            net_values,
            driver_values,
            runtime_states,
            queue: BinaryHeap::new(),
            sequence: 0,
            scheduled_events: 0,
            processed_events: 0,
            external_inputs,
            started: Instant::now(),
        };
        session.schedule_initial(control)?;
        Ok(session)
    }

    fn schedule_initial(&mut self, control: &ExecutionControl) -> Result<(), EngineError> {
        for (component_index, component) in self.program.components.iter().enumerate() {
            match component.primitive {
                DigitalPrimitive::Constant(value) => schedule_output(
                    &mut self.queue,
                    &mut self.sequence,
                    &mut self.scheduled_events,
                    0.0,
                    component_index,
                    0,
                    value,
                )?,
                DigitalPrimitive::Clock {
                    period,
                    duty_cycle,
                    initial,
                } => schedule_clock(
                    &mut self.queue,
                    &mut self.sequence,
                    &mut self.scheduled_events,
                    component_index,
                    self.stop,
                    period,
                    duty_cycle,
                    initial,
                    control,
                    self.started,
                )?,
                DigitalPrimitive::DLatch
                | DigitalPrimitive::SRLatch
                | DigitalPrimitive::DFlipFlop { .. }
                | DigitalPrimitive::JKFlipFlop { .. }
                | DigitalPrimitive::TFlipFlop { .. }
                | DigitalPrimitive::SRFlipFlop { .. }
                | DigitalPrimitive::Register { .. }
                | DigitalPrimitive::Counter { .. } => {
                    let outputs = initial_outputs(component, &self.runtime_states[component_index]);
                    for (output_index, value) in outputs.into_iter().enumerate() {
                        schedule_output(
                            &mut self.queue,
                            &mut self.sequence,
                            &mut self.scheduled_events,
                            0.0,
                            component_index,
                            output_index,
                            value,
                        )?;
                    }
                }
                DigitalPrimitive::Gate(_)
                | DigitalPrimitive::TriState
                | DigitalPrimitive::Mux2
                | DigitalPrimitive::Demux2
                | DigitalPrimitive::Decoder2To4
                | DigitalPrimitive::Sink => {}
            }
        }
        Ok(())
    }

    fn next_event_time(&self) -> Option<crate::CoSimulationTime> {
        self.queue.peek().and_then(|event| {
            digital_seconds_to_future_cosim_time(event.time, self.current_time).ok()
        })
    }

    fn advance_to(
        &mut self,
        target: crate::CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        if target < self.current_time {
            return Err(EngineError::new(
                "digital_cosim_time_invalid",
                "digital co-simulation participant cannot move backwards in time",
            ));
        }
        let target_seconds = target.as_seconds();
        if target_seconds > self.stop + 0.5e-12 {
            return Err(EngineError::new(
                "digital_cosim_time_invalid",
                format!(
                    "digital co-simulation target exceeds request stop time of {} s",
                    self.stop
                ),
            ));
        }
        self.process_until(target_seconds, control)?;
        self.current_time = target;
        Ok(())
    }

    fn write_external(
        &mut self,
        net: &str,
        time: crate::CoSimulationTime,
        value: LogicValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        if time != self.current_time {
            return Err(EngineError::new(
                "digital_cosim_time_invalid",
                "digital co-simulation input writes must occur at the participant current time",
            ));
        }
        if !self.external_inputs.contains(net) {
            return Err(EngineError::new(
                "digital_cosim_binding_invalid",
                format!("net `{net}` is not an external digital input"),
            ));
        }
        let current = self.net_values.get(net).copied().unwrap_or(LogicValue::X);
        if current == value {
            return Ok(false);
        }
        self.net_values.insert(net.to_owned(), value);
        let dependents = self
            .program
            .dependents
            .get(net)
            .cloned()
            .unwrap_or_default();
        self.evaluate_components(&dependents, self.current_time.as_seconds(), control)?;
        self.process_until(self.current_time.as_seconds(), control)?;
        Ok(true)
    }

    fn read_net(&self, net: &str) -> Result<LogicValue, EngineError> {
        self.net_values.get(net).copied().ok_or_else(|| {
            EngineError::new(
                "digital_cosim_binding_invalid",
                format!("digital co-simulation output references unknown net `{net}`"),
            )
        })
    }

    fn process_until(
        &mut self,
        target: f64,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        loop {
            check_execution(control, self.started)?;
            let Some(next) = self.queue.peek() else {
                break;
            };
            if next.time > target + 0.5e-12 {
                break;
            }
            let first = self.queue.pop().expect("peeked digital event exists");
            let time = first.time;
            let mut batch = vec![first];
            while self
                .queue
                .peek()
                .is_some_and(|event| event.time.to_bits() == time.to_bits())
            {
                batch.push(self.queue.pop().expect("peeked digital event exists"));
            }
            self.processed_events += batch.len();
            if self.processed_events > MAX_DIGITAL_EVENTS {
                return Err(event_limit_error());
            }

            let mut affected_nets = BTreeSet::new();
            for event in batch {
                let previous = self
                    .driver_values
                    .insert(event.driver, event.value)
                    .unwrap_or(LogicValue::Z);
                if previous != event.value {
                    let net = &self.program.components[event.driver.component].output_nets
                        [event.driver.output]
                        .1;
                    affected_nets.insert(net.clone());
                }
            }

            let mut affected_components = BTreeSet::new();
            for net in affected_nets {
                let resolved = resolve_net(
                    self.program
                        .net_drivers
                        .get(&net)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    &self.driver_values,
                );
                let current = self.net_values.get(&net).copied().unwrap_or(LogicValue::X);
                if resolved == current {
                    continue;
                }
                self.net_values.insert(net.clone(), resolved);
                if let Some(dependents) = self.program.dependents.get(&net) {
                    affected_components.extend(dependents.iter().copied());
                }
            }
            let components = affected_components.into_iter().collect::<Vec<_>>();
            self.evaluate_components(&components, time, control)?;
        }
        Ok(())
    }

    fn evaluate_components(
        &mut self,
        component_indices: &[usize],
        time: f64,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        check_execution(control, self.started)?;
        for &component_index in component_indices {
            let component = &self.program.components[component_index];
            let Some(values) =
                component.evaluate(&self.net_values, &mut self.runtime_states[component_index])
            else {
                continue;
            };
            for (output_index, value) in values.into_iter().enumerate() {
                if output_index >= component.output_nets.len() {
                    break;
                }
                let output_time = time + component.delay;
                if output_time <= self.stop {
                    schedule_output(
                        &mut self.queue,
                        &mut self.sequence,
                        &mut self.scheduled_events,
                        output_time,
                        component_index,
                        output_index,
                        value,
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn digital_seconds_to_future_cosim_time(
    seconds: f64,
    current: crate::CoSimulationTime,
) -> Result<crate::CoSimulationTime, EngineError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(EngineError::new(
            "digital_cosim_time_invalid",
            "digital event time must be finite and non-negative",
        ));
    }
    let ticks = (seconds * crate::COSIM_TIMEBASE_HZ as f64).ceil();
    if ticks > u64::MAX as f64 {
        return Err(EngineError::new(
            "digital_cosim_time_invalid",
            "digital event time exceeds co-simulation timebase range",
        ));
    }
    let mut time = crate::CoSimulationTime::from_picoseconds(ticks as u64);
    if time <= current && seconds > current.as_seconds() {
        time =
            crate::CoSimulationTime::from_picoseconds(current.as_picoseconds().saturating_add(1));
    }
    Ok(time)
}

impl LogicValue {
    pub const fn is_known(self) -> bool {
        matches!(self, Self::Zero | Self::One)
    }

    pub const fn logic_buffer(self) -> Self {
        match self {
            Self::Zero => Self::Zero,
            Self::One => Self::One,
            Self::X | Self::Z => Self::X,
        }
    }

    pub const fn logic_not(self) -> Self {
        match self {
            Self::Zero => Self::One,
            Self::One => Self::Zero,
            Self::X | Self::Z => Self::X,
        }
    }

    pub const fn logic_and(self, other: Self) -> Self {
        match (self.logic_buffer(), other.logic_buffer()) {
            (Self::Zero, _) | (_, Self::Zero) => Self::Zero,
            (Self::One, Self::One) => Self::One,
            _ => Self::X,
        }
    }

    pub const fn logic_or(self, other: Self) -> Self {
        match (self.logic_buffer(), other.logic_buffer()) {
            (Self::One, _) | (_, Self::One) => Self::One,
            (Self::Zero, Self::Zero) => Self::Zero,
            _ => Self::X,
        }
    }

    pub const fn logic_xor(self, other: Self) -> Self {
        match (self.logic_buffer(), other.logic_buffer()) {
            (Self::Zero, Self::Zero) | (Self::One, Self::One) => Self::Zero,
            (Self::Zero, Self::One) | (Self::One, Self::Zero) => Self::One,
            _ => Self::X,
        }
    }
}

fn resolve_net(
    drivers: &[DriverKey],
    driver_values: &BTreeMap<DriverKey, LogicValue>,
) -> LogicValue {
    if drivers.is_empty() {
        return LogicValue::X;
    }
    let mut resolved = LogicValue::Z;
    for driver in drivers {
        let value = driver_values.get(driver).copied().unwrap_or(LogicValue::Z);
        match value {
            LogicValue::Z => {}
            LogicValue::X => return LogicValue::X,
            LogicValue::Zero | LogicValue::One => {
                if resolved == LogicValue::Z {
                    resolved = value;
                } else if resolved != value {
                    return LogicValue::X;
                }
            }
        }
    }
    resolved
}

fn async_set_reset(
    component: &CompiledDigitalComponent,
    net_values: &BTreeMap<String, LogicValue>,
) -> Option<LogicValue> {
    let set = if component.input_nets.contains_key("set") {
        component.input("set", net_values).logic_buffer()
    } else {
        LogicValue::Zero
    };
    let reset = if component.input_nets.contains_key("reset") {
        component.input("reset", net_values).logic_buffer()
    } else {
        LogicValue::Zero
    };
    match (set, reset) {
        (LogicValue::Zero, LogicValue::Zero) => None,
        (LogicValue::One, LogicValue::Zero) => Some(LogicValue::One),
        (LogicValue::Zero, LogicValue::One) => Some(LogicValue::Zero),
        (LogicValue::One, LogicValue::One) => Some(LogicValue::X),
        _ => Some(LogicValue::X),
    }
}

fn sr_next(current: LogicValue, set: LogicValue, reset: LogicValue) -> LogicValue {
    match (set.logic_buffer(), reset.logic_buffer()) {
        (LogicValue::Zero, LogicValue::Zero) => current,
        (LogicValue::One, LogicValue::Zero) => LogicValue::One,
        (LogicValue::Zero, LogicValue::One) => LogicValue::Zero,
        (LogicValue::One, LogicValue::One) => LogicValue::X,
        _ => LogicValue::X,
    }
}

fn toggle(value: LogicValue) -> LogicValue {
    match value {
        LogicValue::Zero => LogicValue::One,
        LogicValue::One => LogicValue::Zero,
        LogicValue::X | LogicValue::Z => LogicValue::X,
    }
}

fn q_outputs(q: LogicValue, count: usize) -> Vec<LogicValue> {
    match count {
        0 => Vec::new(),
        1 => vec![q],
        _ => vec![q, q.logic_not()],
    }
}

fn is_edge(previous: LogicValue, current: LogicValue, edge: EdgeKind) -> bool {
    match edge {
        EdgeKind::Rising => previous == LogicValue::Zero && current == LogicValue::One,
        EdgeKind::Falling => previous == LogicValue::One && current == LogicValue::Zero,
    }
}

fn increment_bits(bits: &[LogicValue]) -> Vec<LogicValue> {
    let vector = LogicVector::new(bits.to_vec());
    let Some(value) = vector.to_u64() else {
        return vec![LogicValue::X; bits.len()];
    };
    let mask = if bits.len() == 64 {
        u64::MAX
    } else {
        (1_u64 << bits.len()) - 1
    };
    LogicVector::from_u64(bits.len(), value.wrapping_add(1) & mask)
        .map(|vector| vector.0)
        .unwrap_or_else(|_| vec![LogicValue::X; bits.len()])
}

fn initial_outputs(component: &CompiledDigitalComponent, state: &RuntimeState) -> Vec<LogicValue> {
    match component.primitive {
        DigitalPrimitive::Register { .. } | DigitalPrimitive::Counter { .. } => state.bits.clone(),
        DigitalPrimitive::DLatch
        | DigitalPrimitive::SRLatch
        | DigitalPrimitive::DFlipFlop { .. }
        | DigitalPrimitive::JKFlipFlop { .. }
        | DigitalPrimitive::TFlipFlop { .. }
        | DigitalPrimitive::SRFlipFlop { .. } => q_outputs(state.q, component.output_nets.len()),
        _ => Vec::new(),
    }
}

fn endpoint_net_map(circuit: &Circuit) -> Result<BTreeMap<(String, String), String>, EngineError> {
    let mut map = BTreeMap::new();
    for net in &circuit.nets {
        for endpoint in &net.endpoints {
            let key = (
                endpoint.component.as_str().to_owned(),
                endpoint.pin.as_str().to_owned(),
            );
            if let Some(previous) = map.insert(key.clone(), net.id.as_str().to_owned()) {
                return Err(EngineError::new(
                    "digital_endpoint_on_multiple_nets",
                    format!(
                        "endpoint `{}.{}` is connected to both `{previous}` and `{}`",
                        key.0,
                        key.1,
                        net.id.as_str()
                    ),
                ));
            }
        }
    }
    Ok(map)
}

fn compile_component(
    component: &Component,
    endpoint_net: &BTreeMap<(String, String), String>,
) -> Result<CompiledDigitalComponent, EngineError> {
    let kind = component.kind.as_str();
    let mut input_pins = Vec::<String>::new();
    let mut output_pins = Vec::<String>::new();
    let primitive = match kind {
        "logic_input" => {
            output_pins.push("out".to_owned());
            DigitalPrimitive::Constant(required_logic_value(component, "value")?)
        }
        "digital_clock" => {
            output_pins.push("out".to_owned());
            DigitalPrimitive::Clock {
                period: required_positive_seconds(component, "period")?,
                duty_cycle: duty_cycle(component)?,
                initial: optional_logic_value(component, "initial")?.unwrap_or(LogicValue::Zero),
            }
        }
        "logic_output" => {
            input_pins.push("in".to_owned());
            DigitalPrimitive::Sink
        }
        "buffer" => gate_primitive(&mut input_pins, &mut output_pins, GateKind::Buffer, &["in"]),
        "not_gate" => gate_primitive(&mut input_pins, &mut output_pins, GateKind::Not, &["in"]),
        "and_gate" => gate_primitive(
            &mut input_pins,
            &mut output_pins,
            GateKind::And,
            &["a", "b"],
        ),
        "or_gate" => gate_primitive(&mut input_pins, &mut output_pins, GateKind::Or, &["a", "b"]),
        "xor_gate" => gate_primitive(
            &mut input_pins,
            &mut output_pins,
            GateKind::Xor,
            &["a", "b"],
        ),
        "nand_gate" => gate_primitive(
            &mut input_pins,
            &mut output_pins,
            GateKind::Nand,
            &["a", "b"],
        ),
        "nor_gate" => gate_primitive(
            &mut input_pins,
            &mut output_pins,
            GateKind::Nor,
            &["a", "b"],
        ),
        "xnor_gate" => gate_primitive(
            &mut input_pins,
            &mut output_pins,
            GateKind::Xnor,
            &["a", "b"],
        ),
        "tri_state_buffer" => {
            input_pins.extend(["in", "enable"].into_iter().map(str::to_owned));
            output_pins.push("out".to_owned());
            DigitalPrimitive::TriState
        }
        "mux2" => {
            input_pins.extend(["a", "b", "sel"].into_iter().map(str::to_owned));
            output_pins.push("out".to_owned());
            DigitalPrimitive::Mux2
        }
        "demux2" => {
            input_pins.extend(["in", "sel"].into_iter().map(str::to_owned));
            output_pins.extend(["a", "b"].into_iter().map(str::to_owned));
            DigitalPrimitive::Demux2
        }
        "decoder2_to_4" => {
            input_pins.extend(["a", "b", "enable"].into_iter().map(str::to_owned));
            output_pins.extend((0..4).map(|index| format!("y{index}")));
            DigitalPrimitive::Decoder2To4
        }
        "d_latch" => {
            input_pins.extend(["d", "enable"].into_iter().map(str::to_owned));
            append_optional_inputs(component, &mut input_pins, &["set", "reset"]);
            output_pins.push("q".to_owned());
            append_optional_outputs(component, &mut output_pins, &["nq"]);
            DigitalPrimitive::DLatch
        }
        "sr_latch" => {
            input_pins.extend(["s", "r"].into_iter().map(str::to_owned));
            append_optional_inputs(component, &mut input_pins, &["set", "reset"]);
            output_pins.push("q".to_owned());
            append_optional_outputs(component, &mut output_pins, &["nq"]);
            DigitalPrimitive::SRLatch
        }
        "d_flip_flop" => {
            input_pins.extend(["d", "clk"].into_iter().map(str::to_owned));
            append_optional_inputs(component, &mut input_pins, &["set", "reset"]);
            output_pins.push("q".to_owned());
            append_optional_outputs(component, &mut output_pins, &["nq"]);
            DigitalPrimitive::DFlipFlop {
                edge: edge_kind(component)?,
            }
        }
        "jk_flip_flop" => {
            input_pins.extend(["j", "k", "clk"].into_iter().map(str::to_owned));
            append_optional_inputs(component, &mut input_pins, &["set", "reset"]);
            output_pins.push("q".to_owned());
            append_optional_outputs(component, &mut output_pins, &["nq"]);
            DigitalPrimitive::JKFlipFlop {
                edge: edge_kind(component)?,
            }
        }
        "t_flip_flop" => {
            input_pins.extend(["t", "clk"].into_iter().map(str::to_owned));
            append_optional_inputs(component, &mut input_pins, &["set", "reset"]);
            output_pins.push("q".to_owned());
            append_optional_outputs(component, &mut output_pins, &["nq"]);
            DigitalPrimitive::TFlipFlop {
                edge: edge_kind(component)?,
            }
        }
        "sr_flip_flop" => {
            input_pins.extend(["s", "r", "clk"].into_iter().map(str::to_owned));
            append_optional_inputs(component, &mut input_pins, &["set", "reset"]);
            output_pins.push("q".to_owned());
            append_optional_outputs(component, &mut output_pins, &["nq"]);
            DigitalPrimitive::SRFlipFlop {
                edge: edge_kind(component)?,
            }
        }
        "register" => {
            let width = component_width(component)?;
            input_pins.extend((0..width).map(|bit| format!("d{bit}")));
            input_pins.push("clk".to_owned());
            append_optional_inputs(component, &mut input_pins, &["enable", "reset"]);
            output_pins.extend((0..width).map(|bit| format!("q{bit}")));
            DigitalPrimitive::Register {
                width,
                edge: edge_kind(component)?,
            }
        }
        "counter" => {
            let width = component_width(component)?;
            input_pins.push("clk".to_owned());
            append_optional_inputs(component, &mut input_pins, &["enable", "reset"]);
            output_pins.extend((0..width).map(|bit| format!("q{bit}")));
            DigitalPrimitive::Counter {
                width,
                edge: edge_kind(component)?,
            }
        }
        _ => {
            return Err(EngineError::new(
                "digital_unsupported_component",
                format!(
                    "component `{}` has unsupported digital kind `{kind}`",
                    component.id.as_str()
                ),
            ));
        }
    };

    let mut input_nets = BTreeMap::new();
    for pin in input_pins {
        require_pin(component, &pin, PinDirection::Input)?;
        input_nets.insert(
            pin.clone(),
            require_connected_net(component, &pin, endpoint_net)?,
        );
    }
    let mut output_nets = Vec::new();
    for pin in output_pins {
        require_pin(component, &pin, PinDirection::Output)?;
        output_nets.push((
            pin.clone(),
            require_connected_net(component, &pin, endpoint_net)?,
        ));
    }

    let delay = match primitive {
        DigitalPrimitive::Constant(_) | DigitalPrimitive::Clock { .. } | DigitalPrimitive::Sink => {
            0.0
        }
        _ => optional_non_negative_seconds(component, "delay")?.unwrap_or(0.0),
    };

    let initial_scalar = match primitive {
        DigitalPrimitive::DLatch
        | DigitalPrimitive::SRLatch
        | DigitalPrimitive::DFlipFlop { .. }
        | DigitalPrimitive::JKFlipFlop { .. }
        | DigitalPrimitive::TFlipFlop { .. }
        | DigitalPrimitive::SRFlipFlop { .. } => {
            optional_logic_value(component, "initial")?.unwrap_or(LogicValue::X)
        }
        _ => LogicValue::X,
    };
    let initial_vector = match primitive {
        DigitalPrimitive::Register { width, .. } | DigitalPrimitive::Counter { width, .. } => {
            initial_vector(component, width)?
        }
        _ => Vec::new(),
    };

    Ok(CompiledDigitalComponent {
        primitive,
        input_nets,
        output_nets,
        delay,
        initial_scalar,
        initial_vector,
    })
}

fn gate_primitive(
    input_pins: &mut Vec<String>,
    output_pins: &mut Vec<String>,
    kind: GateKind,
    inputs: &[&str],
) -> DigitalPrimitive {
    input_pins.extend(inputs.iter().map(|pin| (*pin).to_owned()));
    output_pins.push("out".to_owned());
    DigitalPrimitive::Gate(kind)
}

fn append_optional_inputs(component: &Component, pins: &mut Vec<String>, names: &[&str]) {
    for name in names {
        if component.pins.iter().any(|pin| pin.id.as_str() == *name) {
            pins.push((*name).to_owned());
        }
    }
}

fn append_optional_outputs(component: &Component, pins: &mut Vec<String>, names: &[&str]) {
    for name in names {
        if component.pins.iter().any(|pin| pin.id.as_str() == *name) {
            pins.push((*name).to_owned());
        }
    }
}

fn component_width(component: &Component) -> Result<usize, EngineError> {
    let width = match component.parameters.get("width") {
        Some(ParameterValue::Integer(value)) if *value > 0 => *value as usize,
        Some(ParameterValue::Number(value)) if value.is_finite() && *value > 0.0 => *value as usize,
        Some(_) => {
            return Err(component_error(
                component,
                "parameter `width` must be a positive integer",
            ));
        }
        None => 4,
    };
    validate_width(width)?;
    Ok(width)
}

fn validate_width(width: usize) -> Result<(), EngineError> {
    if width == 0 || width > MAX_DIGITAL_BUS_WIDTH {
        return Err(EngineError::new(
            "digital_invalid_bus_width",
            format!("digital bus width must be in 1..={MAX_DIGITAL_BUS_WIDTH}, got {width}"),
        ));
    }
    Ok(())
}

fn initial_vector(component: &Component, width: usize) -> Result<Vec<LogicValue>, EngineError> {
    match component.parameters.get("initial") {
        None => Ok(vec![LogicValue::Zero; width]),
        Some(ParameterValue::Integer(value)) if *value >= 0 => {
            Ok(LogicVector::from_u64(width, *value as u64)?.0)
        }
        Some(ParameterValue::Number(value))
            if value.is_finite() && *value >= 0.0 && value.fract() == 0.0 =>
        {
            Ok(LogicVector::from_u64(width, *value as u64)?.0)
        }
        Some(_) => Err(component_error(
            component,
            "vector parameter `initial` must be a non-negative integer",
        )),
    }
}

fn edge_kind(component: &Component) -> Result<EdgeKind, EngineError> {
    match component.parameters.get("edge") {
        None => Ok(EdgeKind::Rising),
        Some(ParameterValue::Text(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "rising" | "positive" | "pos" => Ok(EdgeKind::Rising),
            "falling" | "negative" | "neg" => Ok(EdgeKind::Falling),
            _ => Err(component_error(
                component,
                "parameter `edge` must be `rising` or `falling`",
            )),
        },
        Some(_) => Err(component_error(component, "parameter `edge` must be text")),
    }
}

fn require_pin(
    component: &Component,
    pin_id: &str,
    direction: PinDirection,
) -> Result<(), EngineError> {
    let Some(pin) = component.pins.iter().find(|pin| pin.id.as_str() == pin_id) else {
        return Err(component_error(
            component,
            format!("required digital pin `{pin_id}` is missing"),
        ));
    };
    if pin.domain != SignalDomain::Digital {
        return Err(component_error(
            component,
            format!("pin `{pin_id}` must use the digital signal domain"),
        ));
    }
    if pin.direction != direction {
        return Err(component_error(
            component,
            format!("pin `{pin_id}` must have direction {direction:?}"),
        ));
    }
    Ok(())
}

fn require_connected_net(
    component: &Component,
    pin: &str,
    endpoint_net: &BTreeMap<(String, String), String>,
) -> Result<String, EngineError> {
    endpoint_net
        .get(&(component.id.as_str().to_owned(), pin.to_owned()))
        .cloned()
        .ok_or_else(|| {
            component_error(
                component,
                format!("required digital pin `{pin}` is not connected to a net"),
            )
        })
}

fn compile_probes(
    request: &SimulationRequest,
    endpoint_net: &BTreeMap<(String, String), String>,
) -> Result<Vec<CompiledProbe>, EngineError> {
    if request.probes.is_empty() {
        return Ok(request
            .circuit
            .nets
            .iter()
            .map(|net| CompiledProbe {
                signal: format!("net:{}", net.id.as_str()),
                net: net.id.as_str().to_owned(),
            })
            .collect());
    }

    request
        .probes
        .iter()
        .map(|probe| compile_probe(probe, endpoint_net))
        .collect()
}

fn compile_probe(
    probe: &Probe,
    endpoint_net: &BTreeMap<(String, String), String>,
) -> Result<CompiledProbe, EngineError> {
    let key = (
        probe.endpoint.component.as_str().to_owned(),
        probe.endpoint.pin.as_str().to_owned(),
    );
    let net = endpoint_net.get(&key).cloned().ok_or_else(|| {
        EngineError::new(
            "digital_unknown_probe",
            format!("probe endpoint `{}.{}` is not connected", key.0, key.1),
        )
    })?;
    Ok(CompiledProbe {
        signal: probe.alias.clone().unwrap_or_else(|| format!("net:{net}")),
        net,
    })
}

fn required_logic_value(component: &Component, key: &str) -> Result<LogicValue, EngineError> {
    optional_logic_value(component, key)?.ok_or_else(|| {
        component_error(
            component,
            format!("required logic parameter `{key}` is missing"),
        )
    })
}

fn optional_logic_value(
    component: &Component,
    key: &str,
) -> Result<Option<LogicValue>, EngineError> {
    let Some(value) = component.parameters.get(key) else {
        return Ok(None);
    };
    let logic = match value {
        ParameterValue::Boolean(value) => {
            if *value {
                LogicValue::One
            } else {
                LogicValue::Zero
            }
        }
        ParameterValue::Integer(0) => LogicValue::Zero,
        ParameterValue::Integer(1) => LogicValue::One,
        ParameterValue::Text(value) => match value.trim().to_ascii_lowercase().as_str() {
            "0" | "zero" | "low" => LogicValue::Zero,
            "1" | "one" | "high" => LogicValue::One,
            "x" | "unknown" => LogicValue::X,
            "z" | "high_impedance" | "high-impedance" => LogicValue::Z,
            _ => {
                return Err(component_error(
                    component,
                    format!("parameter `{key}` is not a supported four-state logic value"),
                ));
            }
        },
        ParameterValue::Integer(_) | ParameterValue::Number(_) | ParameterValue::Quantity(_) => {
            return Err(component_error(
                component,
                format!("parameter `{key}` must be 0, 1, x, z, or boolean"),
            ));
        }
    };
    Ok(Some(logic))
}

fn required_positive_seconds(component: &Component, key: &str) -> Result<f64, EngineError> {
    let value = optional_seconds(component, key)?.ok_or_else(|| {
        component_error(component, format!("required parameter `{key}` is missing"))
    })?;
    if value <= 0.0 {
        return Err(component_error(
            component,
            format!("parameter `{key}` must be greater than zero"),
        ));
    }
    Ok(value)
}

fn optional_non_negative_seconds(
    component: &Component,
    key: &str,
) -> Result<Option<f64>, EngineError> {
    let value = optional_seconds(component, key)?;
    if value.is_some_and(|value| value < 0.0) {
        return Err(component_error(
            component,
            format!("parameter `{key}` must not be negative"),
        ));
    }
    Ok(value)
}

fn optional_seconds(component: &Component, key: &str) -> Result<Option<f64>, EngineError> {
    let Some(value) = component.parameters.get(key) else {
        return Ok(None);
    };
    let seconds = match value {
        ParameterValue::Integer(value) => *value as f64,
        ParameterValue::Number(value) => *value,
        ParameterValue::Quantity(Quantity {
            value,
            unit: Unit::Second,
        }) => *value,
        ParameterValue::Quantity(quantity) => {
            return Err(component_error(
                component,
                format!("parameter `{key}` expects seconds, got {:?}", quantity.unit),
            ));
        }
        ParameterValue::Boolean(_) | ParameterValue::Text(_) => {
            return Err(component_error(
                component,
                format!("parameter `{key}` must be numeric seconds"),
            ));
        }
    };
    if !seconds.is_finite() {
        return Err(component_error(
            component,
            format!("parameter `{key}` must be finite"),
        ));
    }
    Ok(Some(seconds))
}

fn duty_cycle(component: &Component) -> Result<f64, EngineError> {
    let Some(value) = component.parameters.get("duty_cycle") else {
        return Ok(0.5);
    };
    let duty = match value {
        ParameterValue::Integer(value) => *value as f64,
        ParameterValue::Number(value) => *value,
        ParameterValue::Quantity(Quantity {
            value,
            unit: Unit::Dimensionless,
        }) => *value,
        _ => {
            return Err(component_error(
                component,
                "parameter `duty_cycle` must be a dimensionless number",
            ));
        }
    };
    if !duty.is_finite() || duty <= 0.0 || duty >= 1.0 {
        return Err(component_error(
            component,
            "parameter `duty_cycle` must be greater than zero and less than one",
        ));
    }
    Ok(duty)
}

#[allow(clippy::too_many_arguments)]
fn schedule_clock(
    queue: &mut BinaryHeap<ScheduledEvent>,
    sequence: &mut u64,
    scheduled_events: &mut usize,
    component_index: usize,
    stop: f64,
    period: f64,
    duty_cycle: f64,
    initial: LogicValue,
    control: &ExecutionControl,
    started: Instant,
) -> Result<(), EngineError> {
    if !initial.is_known() {
        return Err(EngineError::new(
            "digital_invalid_clock",
            "digital clock initial value must be zero or one",
        ));
    }
    schedule_output(
        queue,
        sequence,
        scheduled_events,
        0.0,
        component_index,
        0,
        initial,
    )?;

    let high_duration = period * duty_cycle;
    let low_duration = period * (1.0 - duty_cycle);
    let mut time = 0.0;
    let mut value = initial;
    while time < stop {
        check_execution(control, started)?;
        time += if value == LogicValue::One {
            high_duration
        } else {
            low_duration
        };
        if time > stop {
            break;
        }
        value = if value == LogicValue::One {
            LogicValue::Zero
        } else {
            LogicValue::One
        };
        schedule_output(
            queue,
            sequence,
            scheduled_events,
            time,
            component_index,
            0,
            value,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn schedule_output(
    queue: &mut BinaryHeap<ScheduledEvent>,
    sequence: &mut u64,
    scheduled_events: &mut usize,
    time: f64,
    component: usize,
    output: usize,
    value: LogicValue,
) -> Result<(), EngineError> {
    if !time.is_finite() || time < 0.0 {
        return Err(EngineError::new(
            "digital_invalid_event_time",
            "digital event time must be finite and non-negative",
        ));
    }
    *scheduled_events += 1;
    if *scheduled_events > MAX_DIGITAL_EVENTS {
        return Err(event_limit_error());
    }
    queue.push(ScheduledEvent {
        time,
        sequence: *sequence,
        driver: DriverKey { component, output },
        value,
    });
    *sequence = (*sequence).wrapping_add(1);
    Ok(())
}

fn push_transition(transitions: &mut Vec<DigitalTransition>, time: f64, value: LogicValue) {
    if let Some(last) = transitions.last_mut() {
        if last.time.to_bits() == time.to_bits() {
            last.value = value;
            return;
        }
        if last.value == value {
            return;
        }
    }
    transitions.push(DigitalTransition { time, value });
}

fn enforce_transition_budget(
    histories: &BTreeMap<String, Vec<DigitalTransition>>,
    control: &ExecutionControl,
) -> Result<(), EngineError> {
    if control.policy.max_output_bytes == 0 {
        return Ok(());
    }
    const APPROX_TRANSITION_BYTES: u64 = 24;
    let transitions = histories
        .values()
        .map(|history| history.len() as u64)
        .sum::<u64>();
    let estimated = transitions.saturating_mul(APPROX_TRANSITION_BYTES);
    if estimated > control.policy.max_output_bytes {
        return Err(EngineError::new(
            "execution_resource_limit",
            format!(
                "digital transition output estimate {estimated} bytes exceeds configured limit {} bytes",
                control.policy.max_output_bytes
            ),
        ));
    }
    Ok(())
}

fn check_execution(control: &ExecutionControl, started: Instant) -> Result<(), EngineError> {
    if control.cancellation.is_cancelled() {
        return Err(execution_cancelled());
    }
    if control.policy.timeout_ms > 0
        && started.elapsed().as_millis() >= u128::from(control.policy.timeout_ms)
    {
        return Err(EngineError::new(
            "execution_timeout",
            format!(
                "digital simulation exceeded configured timeout of {} ms",
                control.policy.timeout_ms
            ),
        ));
    }
    Ok(())
}

fn execution_cancelled() -> EngineError {
    EngineError::new("execution_cancelled", "digital simulation was cancelled")
}

fn event_limit_error() -> EngineError {
    EngineError::new(
        "execution_resource_limit",
        format!("digital simulation exceeded {MAX_DIGITAL_EVENTS} scheduled events"),
    )
}

fn component_error(component: &Component, message: impl Into<String>) -> EngineError {
    EngineError::new(
        "digital_invalid_component",
        format!("component `{}`: {}", component.id.as_str(), message.into()),
    )
}
