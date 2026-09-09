use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    time::Instant,
};

use crate::{
    Analysis, AnalysisKind, Circuit, Component, Diagnostic, DigitalTransition, DigitalWaveform,
    EngineCapabilities, EngineError, EngineId, ExecutionControl, LogicValue, ParameterValue,
    PinDirection, Probe, Quantity, SignalDomain, SignalId, SimulationEngine, SimulationRequest,
    SimulationResult, Unit, VERSION, Waveform,
};

pub const DIGITAL_ENGINE_ID: &str = "digital";
pub const MAX_DIGITAL_EVENTS: usize = 1_000_000;

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

#[derive(Clone, Copy, Debug)]
enum DigitalPrimitive {
    Constant(LogicValue),
    Clock {
        period: f64,
        duty_cycle: f64,
        initial: LogicValue,
    },
    Gate(GateKind),
    Sink,
}

#[derive(Clone, Debug)]
struct CompiledDigitalComponent {
    primitive: DigitalPrimitive,
    input_nets: Vec<String>,
    output_net: Option<String>,
    delay: f64,
}

impl CompiledDigitalComponent {
    fn evaluate(&self, net_values: &BTreeMap<String, LogicValue>) -> Option<LogicValue> {
        let DigitalPrimitive::Gate(kind) = self.primitive else {
            return None;
        };
        let inputs = self
            .input_nets
            .iter()
            .map(|net| net_values.get(net).copied().unwrap_or(LogicValue::X))
            .collect::<Vec<_>>();
        let value = match kind {
            GateKind::Buffer => inputs[0].logic_buffer(),
            GateKind::Not => inputs[0].logic_not(),
            GateKind::And => inputs[0].logic_and(inputs[1]),
            GateKind::Or => inputs[0].logic_or(inputs[1]),
            GateKind::Xor => inputs[0].logic_xor(inputs[1]),
            GateKind::Nand => inputs[0].logic_and(inputs[1]).logic_not(),
            GateKind::Nor => inputs[0].logic_or(inputs[1]).logic_not(),
            GateKind::Xnor => inputs[0].logic_xor(inputs[1]).logic_not(),
        };
        Some(value)
    }
}

#[derive(Clone, Debug)]
struct CompiledProbe {
    signal: String,
    net: String,
}

#[derive(Clone, Debug)]
struct DigitalProgram {
    components: Vec<CompiledDigitalComponent>,
    dependents: BTreeMap<String, Vec<usize>>,
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
        let stop = *stop;
        if !stop.is_finite() || stop <= 0.0 {
            return Err(EngineError::new(
                "digital_invalid_analysis",
                "digital transient stop time must be finite and greater than zero",
            ));
        }

        let endpoint_net = endpoint_net_map(&request.circuit)?;
        let mut components = Vec::with_capacity(request.circuit.components.len());
        let mut dependents: BTreeMap<String, Vec<usize>> = BTreeMap::new();

        for component in &request.circuit.components {
            let compiled = compile_component(component, &endpoint_net)?;
            let component_index = components.len();
            for net in &compiled.input_nets {
                dependents
                    .entry(net.clone())
                    .or_default()
                    .push(component_index);
            }
            components.push(compiled);
        }

        let mut driven_nets = BTreeSet::new();
        for component in &components {
            if let Some(net) = component.output_net.as_ref() {
                if !driven_nets.insert(net.clone()) {
                    return Err(EngineError::new(
                        "digital_multiple_drivers",
                        format!("digital net `{net}` has more than one driver"),
                    ));
                }
            }
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
            nets,
            probes,
        })
    }
}

#[derive(Clone, Debug)]
struct ScheduledEvent {
    time: f64,
    sequence: u64,
    net: String,
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

        let mut queue = BinaryHeap::new();
        let mut sequence = 0_u64;
        let mut scheduled_events = 0_usize;

        for component in &program.components {
            match component.primitive {
                DigitalPrimitive::Constant(value) => {
                    let net = component
                        .output_net
                        .as_ref()
                        .expect("source output is compiled");
                    schedule_event(
                        &mut queue,
                        &mut sequence,
                        &mut scheduled_events,
                        0.0,
                        net,
                        value,
                    )?;
                }
                DigitalPrimitive::Clock {
                    period,
                    duty_cycle,
                    initial,
                } => {
                    let net = component
                        .output_net
                        .as_ref()
                        .expect("clock output is compiled");
                    schedule_clock(
                        &mut queue,
                        &mut sequence,
                        &mut scheduled_events,
                        net,
                        stop,
                        period,
                        duty_cycle,
                        initial,
                        control,
                        started,
                    )?;
                }
                DigitalPrimitive::Gate(_) | DigitalPrimitive::Sink => {}
            }
        }

        let mut processed_events = 0_usize;
        while let Some(event) = queue.pop() {
            check_execution(control, started)?;
            if event.time > stop {
                break;
            }
            processed_events += 1;
            if processed_events > MAX_DIGITAL_EVENTS {
                return Err(event_limit_error());
            }

            let current = net_values.get(&event.net).copied().unwrap_or(LogicValue::X);
            if current == event.value {
                continue;
            }
            net_values.insert(event.net.clone(), event.value);
            push_transition(
                histories
                    .get_mut(&event.net)
                    .expect("compiled digital net owns history"),
                event.time,
                event.value,
            );
            enforce_transition_budget(&histories, control)?;

            if let Some(dependent_indices) = program.dependents.get(&event.net) {
                for component_index in dependent_indices {
                    let component = &program.components[*component_index];
                    let Some(value) = component.evaluate(&net_values) else {
                        continue;
                    };
                    let Some(output_net) = component.output_net.as_ref() else {
                        continue;
                    };
                    let output_time = event.time + component.delay;
                    if output_time <= stop {
                        schedule_event(
                            &mut queue,
                            &mut sequence,
                            &mut scheduled_events,
                            output_time,
                            output_net,
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

        Ok(SimulationResult::new(
            self.id(),
            self.version(),
            request.analysis.kind(),
            request.circuit.schema_version,
            waveforms,
            Vec::<Diagnostic>::new(),
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
    let (primitive, input_pins, output_pin) = match kind {
        "logic_input" => (
            DigitalPrimitive::Constant(required_logic_value(component, "value")?),
            Vec::new(),
            Some("out"),
        ),
        "digital_clock" => (
            DigitalPrimitive::Clock {
                period: required_positive_seconds(component, "period")?,
                duty_cycle: duty_cycle(component)?,
                initial: optional_logic_value(component, "initial")?.unwrap_or(LogicValue::Zero),
            },
            Vec::new(),
            Some("out"),
        ),
        "logic_output" => (DigitalPrimitive::Sink, vec!["in"], None),
        "buffer" => (
            DigitalPrimitive::Gate(GateKind::Buffer),
            vec!["in"],
            Some("out"),
        ),
        "not_gate" => (
            DigitalPrimitive::Gate(GateKind::Not),
            vec!["in"],
            Some("out"),
        ),
        "and_gate" => (
            DigitalPrimitive::Gate(GateKind::And),
            vec!["a", "b"],
            Some("out"),
        ),
        "or_gate" => (
            DigitalPrimitive::Gate(GateKind::Or),
            vec!["a", "b"],
            Some("out"),
        ),
        "xor_gate" => (
            DigitalPrimitive::Gate(GateKind::Xor),
            vec!["a", "b"],
            Some("out"),
        ),
        "nand_gate" => (
            DigitalPrimitive::Gate(GateKind::Nand),
            vec!["a", "b"],
            Some("out"),
        ),
        "nor_gate" => (
            DigitalPrimitive::Gate(GateKind::Nor),
            vec!["a", "b"],
            Some("out"),
        ),
        "xnor_gate" => (
            DigitalPrimitive::Gate(GateKind::Xnor),
            vec!["a", "b"],
            Some("out"),
        ),
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

    let mut input_nets = Vec::with_capacity(input_pins.len());
    for pin in input_pins {
        require_pin(component, pin, PinDirection::Input)?;
        input_nets.push(require_connected_net(component, pin, endpoint_net)?);
    }
    let output_net = if let Some(pin) = output_pin {
        require_pin(component, pin, PinDirection::Output)?;
        Some(require_connected_net(component, pin, endpoint_net)?)
    } else {
        None
    };

    let delay = match primitive {
        DigitalPrimitive::Gate(_) => {
            optional_non_negative_seconds(component, "delay")?.unwrap_or(0.0)
        }
        DigitalPrimitive::Constant(_) | DigitalPrimitive::Clock { .. } | DigitalPrimitive::Sink => {
            0.0
        }
    };

    Ok(CompiledDigitalComponent {
        primitive,
        input_nets,
        output_net,
        delay,
    })
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
    net: &str,
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
    schedule_event(queue, sequence, scheduled_events, 0.0, net, initial)?;

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
        schedule_event(queue, sequence, scheduled_events, time, net, value)?;
    }
    Ok(())
}

fn schedule_event(
    queue: &mut BinaryHeap<ScheduledEvent>,
    sequence: &mut u64,
    scheduled_events: &mut usize,
    time: f64,
    net: &str,
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
        net: net.to_owned(),
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
