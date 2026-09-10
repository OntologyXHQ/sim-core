use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::ngspice::{
    ChildGuard, TempRunDir, Topology, bounded_log, compile_component, compile_models,
    enforce_buffer_limit, extract_warnings, parse_wrdata, read_text_limited, spice_number,
    wait_for_child_with_files,
};
use crate::xspice::{
    CompiledProbe as XSpiceProbe, SourceSpec, clock_events, compile_sequential_xspice,
    compile_stimulus, connected_node, duty_cycle, gate_contract, log_has_xspice_failure,
    optional_logic_value, optional_non_negative_seconds, parse_vcd, require_pin,
    required_logic_value,
};
use crate::{
    AnalogAxis, AnalogWaveform, Analysis, AnalysisKind, AxisKind, Circuit, Component,
    ComponentKind, Diagnostic, DiagnosticLevel, EngineCapabilities, EngineError, EngineId,
    ExecutionControl, LogicValue, ParameterValue, PinDirection, Quantity, SignalDomain, SignalId,
    SimulationEngine, SimulationRequest, SimulationResult, Unit, Waveform,
    XSPICE_MIN_DELAY_SECONDS, XSpiceEngine,
};

pub const MIXED_SIGNAL_ENGINE_ID: &str = "mixed-signal-xspice";
const NETLIST_FILE: &str = "mixed.cir";
const STIMULUS_FILE: &str = "source.txt";
const LOG_FILE: &str = "mixed.log";
const ANALOG_RESULT_FILE: &str = "analog.txt";
const DIGITAL_RESULT_FILE: &str = "digital.vcd";

type NetNodeMap = BTreeMap<String, String>;
type EndpointNetMap = BTreeMap<(String, String), String>;
type MixedNodeMaps = (NetNodeMap, EndpointNetMap);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MixedSignalInfo {
    pub available: bool,
    pub xspice_available: bool,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MixedSignalEngine {
    executable: PathBuf,
}

impl Default for MixedSignalEngine {
    fn default() -> Self {
        Self::new("ngspice")
    }
}

impl MixedSignalEngine {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn info(&self) -> MixedSignalInfo {
        let info = XSpiceEngine::new(self.executable.clone()).info();
        MixedSignalInfo {
            available: info.available,
            xspice_available: info.xspice_available,
            executable: info.executable,
            version: info.version,
        }
    }

    fn run(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, EngineError> {
        control.policy.validate()?;
        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                "simulation was cancelled before mixed-signal execution",
            ));
        }

        let compiled = compile_request(request)?;
        let input_bytes = compiled
            .netlist
            .len()
            .saturating_add(compiled.stimulus.len()) as u64;
        enforce_buffer_limit(
            "mixed-signal input",
            input_bytes,
            control.policy.max_input_bytes,
        )?;

        let run_dir = TempRunDir::create().map_err(|error| {
            EngineError::new(
                "mixed_signal_tempdir_failed",
                format!("could not create mixed-signal run directory: {error}"),
            )
        })?;
        fs::write(
            run_dir.path().join(NETLIST_FILE),
            compiled.netlist.as_bytes(),
        )
        .map_err(|error| {
            EngineError::new(
                "mixed_signal_netlist_write_failed",
                format!("could not write generated mixed-signal netlist: {error}"),
            )
        })?;
        if !compiled.stimulus.is_empty() {
            fs::write(
                run_dir.path().join(STIMULUS_FILE),
                compiled.stimulus.as_bytes(),
            )
            .map_err(|error| {
                EngineError::new(
                    "mixed_signal_stimulus_write_failed",
                    format!("could not write generated mixed-signal stimulus: {error}"),
                )
            })?;
        }

        let child = Command::new(&self.executable)
            .arg("-n")
            .arg("-b")
            .arg("-o")
            .arg(LOG_FILE)
            .arg(NETLIST_FILE)
            .current_dir(run_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                let code = if error.kind() == io::ErrorKind::NotFound {
                    "ngspice_not_found"
                } else {
                    "mixed_signal_spawn_failed"
                };
                EngineError::new(
                    code,
                    format!("could not execute `{}`: {error}", self.executable.display()),
                )
            })?;
        let mut child = ChildGuard::new(child);
        let primary_result = if compiled.analog_probes.is_empty() {
            DIGITAL_RESULT_FILE
        } else {
            ANALOG_RESULT_FILE
        };
        let status = wait_for_child_with_files(
            child.child_mut(),
            run_dir.path(),
            control,
            LOG_FILE,
            primary_result,
        )?;
        child.disarm();

        let log = match read_text_limited(
            &run_dir.path().join(LOG_FILE),
            control.policy.max_log_bytes,
            "log",
            "mixed_signal_log_read_failed",
        ) {
            Ok(log) => log,
            Err(error) if error.code() == "mixed_signal_log_read_failed" => String::new(),
            Err(error) => return Err(error),
        };
        if !status.success() {
            return Err(EngineError::new(
                "mixed_signal_failed",
                format!("ngspice/XSPICE exited with {status}: {}", bounded_log(&log)),
            ));
        }
        if log_has_xspice_failure(&log) {
            return Err(EngineError::new(
                "xspice_unavailable",
                format!(
                    "ngspice does not appear to provide usable XSPICE code models: {}",
                    bounded_log(&log)
                ),
            ));
        }

        let analog_text = if compiled.analog_probes.is_empty() {
            None
        } else {
            Some(read_text_limited(
                &run_dir.path().join(ANALOG_RESULT_FILE),
                control.policy.max_output_bytes,
                "analog result",
                "mixed_signal_analog_result_read_failed",
            )?)
        };
        let digital_text = if compiled.digital_probes.is_empty() {
            None
        } else {
            Some(read_text_limited(
                &run_dir.path().join(DIGITAL_RESULT_FILE),
                control.policy.max_output_bytes,
                "digital result",
                "mixed_signal_digital_result_read_failed",
            )?)
        };
        let total_output_bytes = analog_text
            .as_ref()
            .map_or(0_u64, |value| value.len() as u64)
            .saturating_add(
                digital_text
                    .as_ref()
                    .map_or(0_u64, |value| value.len() as u64),
            );
        enforce_buffer_limit(
            "mixed-signal result",
            total_output_bytes,
            control.policy.max_output_bytes,
        )?;

        let mut waveforms = Vec::new();
        if let Some(table) = analog_text {
            let rows = parse_wrdata(&table, compiled.analog_probes.len(), false, false)?;
            if rows.is_empty() {
                return Err(EngineError::new(
                    "mixed_signal_empty_analog_result",
                    "mixed-signal transient returned no analog rows",
                ));
            }
            let axis = AnalogAxis {
                kind: AxisKind::Time,
                unit: Unit::Second,
                values: rows.iter().map(|row| row.axis).collect(),
            };
            for (index, probe) in compiled.analog_probes.iter().enumerate() {
                waveforms.push(Waveform::Analog(AnalogWaveform {
                    signal: SignalId::new(probe.signal.clone()),
                    unit: Unit::Volt,
                    axis: axis.clone(),
                    values: rows.iter().map(|row| row.values[index].0).collect(),
                    imaginary: None,
                }));
            }
        }
        if let Some(vcd) = digital_text {
            waveforms.extend(parse_vcd(&vcd, &compiled.digital_probes)?);
        }

        let info = self.info();
        let mut diagnostics = vec![Diagnostic {
            level: DiagnosticLevel::Info,
            code: "mixed_signal_backend".to_owned(),
            message: "single-process ngspice/XSPICE coordinated analog + event transient"
                .to_owned(),
        }];
        if compiled.delay_floor_applied {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                code: "xspice_delay_floor".to_owned(),
                message: format!(
                    "mixed-signal XSPICE delays below {XSPICE_MIN_DELAY_SECONDS:.3e} s were clamped to the solver minimum"
                ),
            });
        }
        if let Some(version) = info.version.clone() {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Info,
                code: "ngspice_version".to_owned(),
                message: version,
            });
        }
        for warning in extract_warnings(&log) {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                code: "mixed_signal_warning".to_owned(),
                message: warning,
            });
        }

        Ok(SimulationResult::new(
            self.id(),
            info.version,
            request.analysis.kind(),
            request.circuit.schema_version,
            waveforms,
            diagnostics,
        ))
    }
}

impl SimulationEngine for MixedSignalEngine {
    fn id(&self) -> EngineId {
        EngineId::new(MIXED_SIGNAL_ENGINE_ID)
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: false,
            digital: false,
            mixed_signal: true,
            analyses: BTreeSet::from([AnalysisKind::MixedSignalTransient]),
        }
    }

    fn version(&self) -> Option<String> {
        self.info().version
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NetDomain {
    Analog,
    Digital,
}

#[derive(Clone, Debug)]
struct AnalogProbe {
    signal: String,
    expression: String,
}

#[derive(Clone, Debug)]
struct CompiledMixedRequest {
    netlist: String,
    stimulus: String,
    analog_probes: Vec<AnalogProbe>,
    digital_probes: Vec<XSpiceProbe>,
    delay_floor_applied: bool,
}

fn compile_request(request: &SimulationRequest) -> Result<CompiledMixedRequest, EngineError> {
    let Analysis::MixedSignalTransient { step, stop } = &request.analysis else {
        return Err(EngineError::new(
            "mixed_signal_unsupported_analysis",
            "mixed-signal engine only supports mixed_signal_transient analysis",
        ));
    };
    let step = *step;
    let stop = *stop;
    positive_finite("mixed transient step", step)?;
    positive_finite("mixed transient stop", stop)?;
    if step > stop {
        return Err(EngineError::new(
            "mixed_signal_invalid_analysis",
            "mixed transient step must not exceed stop time",
        ));
    }

    let domains = classify_nets(&request.circuit)?;
    let (net_nodes, endpoint_net) = build_nodes(&request.circuit, &domains)?;
    let topology = Topology::build_with_net_nodes(&request.circuit, net_nodes.clone())?;
    let models = compile_models(&request.circuit.models)?;

    let mut analog_lines = Vec::new();
    let mut digital_lines = Vec::new();
    let mut bridge_lines = Vec::new();
    let mut model_lines = Vec::new();
    let mut sources = Vec::new();
    let mut bridge_count = 0_usize;
    let mut delay_floor_applied = false;

    for (index, component) in request.circuit.components.iter().enumerate() {
        match component.kind.as_str() {
            "ground" => {}
            "resistor" | "capacitor" | "inductor" | "voltage_source" | "current_source"
            | "diode" | "bjt" | "mosfet" | "op_amp" | "subcircuit" => {
                analog_lines.push(compile_component(index, component, &topology, &models)?);
            }
            "logic_input" => {
                require_pin(component, "out", PinDirection::Output)?;
                let node = connected_node(component, "out", &endpoint_net, &net_nodes)?;
                sources.push(SourceSpec {
                    node,
                    events: vec![(0.0, required_logic_value(component, "value")?)],
                });
            }
            "digital_clock" => {
                require_pin(component, "out", PinDirection::Output)?;
                let node = connected_node(component, "out", &endpoint_net, &net_nodes)?;
                let period = required_positive_seconds(component, "period")?;
                let duty = duty_cycle(component)?;
                let initial =
                    optional_logic_value(component, "initial")?.unwrap_or(LogicValue::Zero);
                sources.push(SourceSpec {
                    node,
                    events: clock_events(stop, period, duty, initial)?,
                });
            }
            "logic_output" => {
                require_pin(component, "in", PinDirection::Input)?;
                let _ = connected_node(component, "in", &endpoint_net, &net_nodes)?;
            }
            "buffer" | "not_gate" | "and_gate" | "or_gate" | "xor_gate" | "nand_gate"
            | "nor_gate" | "xnor_gate" => {
                let (input_pins, output_pin, model_kind) = gate_contract(component.kind.as_str())?;
                let input_nodes = input_pins
                    .iter()
                    .map(|pin| {
                        require_pin(component, pin, PinDirection::Input)?;
                        connected_node(component, pin, &endpoint_net, &net_nodes)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                require_pin(component, output_pin, PinDirection::Output)?;
                let output_node = connected_node(component, output_pin, &endpoint_net, &net_nodes)?;
                let model_name = format!("xmg{index}");
                let instance_name = format!("amg{index}");
                let input_expr = if input_nodes.len() == 1 {
                    input_nodes[0].clone()
                } else {
                    format!("[{}]", input_nodes.join(" "))
                };
                digital_lines.push(format!(
                    "{instance_name} {input_expr} {output_node} {model_name}"
                ));
                let requested_delay =
                    optional_non_negative_seconds(component, "delay")?.unwrap_or(0.0);
                let delay = requested_delay.max(XSPICE_MIN_DELAY_SECONDS);
                delay_floor_applied |= requested_delay < XSPICE_MIN_DELAY_SECONDS;
                model_lines.push(format!(
                    ".model {model_name} {model_kind}(rise_delay={} fall_delay={})",
                    spice_number(delay),
                    spice_number(delay)
                ));
            }
            "tri_state_buffer" => {
                for pin in ["in", "enable"] {
                    require_pin(component, pin, PinDirection::Input)?;
                }
                require_pin(component, "out", PinDirection::Output)?;
                let input = connected_node(component, "in", &endpoint_net, &net_nodes)?;
                let enable = connected_node(component, "enable", &endpoint_net, &net_nodes)?;
                let output = connected_node(component, "out", &endpoint_net, &net_nodes)?;
                let model_name = format!("xmt{index}");
                digital_lines.push(format!("amt{index} {input} {enable} {output} {model_name}"));
                let requested_delay =
                    optional_non_negative_seconds(component, "delay")?.unwrap_or(0.0);
                let delay = requested_delay.max(XSPICE_MIN_DELAY_SECONDS);
                delay_floor_applied |= requested_delay < XSPICE_MIN_DELAY_SECONDS;
                model_lines.push(format!(
                    ".model {model_name} d_tristate(delay={})",
                    spice_number(delay)
                ));
            }
            "d_flip_flop" | "jk_flip_flop" | "t_flip_flop" | "sr_flip_flop" | "d_latch" => {
                compile_sequential_xspice(
                    component,
                    index + 10_000,
                    &endpoint_net,
                    &net_nodes,
                    &mut digital_lines,
                    &mut model_lines,
                    &mut delay_floor_applied,
                )?;
            }
            "adc_bridge" => {
                bridge_count += 1;
                compile_adc_bridge(
                    component,
                    index,
                    &endpoint_net,
                    &net_nodes,
                    &mut bridge_lines,
                    &mut model_lines,
                    &mut delay_floor_applied,
                )?;
            }
            "dac_bridge" => {
                bridge_count += 1;
                compile_dac_bridge(
                    component,
                    index,
                    &endpoint_net,
                    &net_nodes,
                    &mut bridge_lines,
                    &mut model_lines,
                    &mut delay_floor_applied,
                )?;
            }
            other => {
                return Err(EngineError::new(
                    "mixed_signal_unsupported_component",
                    format!(
                        "component `{}` has unsupported mixed-signal kind `{other}`",
                        component.id.as_str()
                    ),
                ));
            }
        }
    }

    if bridge_count == 0 {
        return Err(EngineError::new(
            "mixed_signal_bridge_required",
            "mixed-signal transient requires at least one explicit adc_bridge or dac_bridge",
        ));
    }

    let (analog_probes, digital_probes) =
        compile_probes(request, &domains, &endpoint_net, &net_nodes)?;
    if analog_probes.is_empty() && digital_probes.is_empty() {
        return Err(EngineError::new(
            "mixed_signal_no_probes",
            "mixed-signal transient has no observable analog or digital net",
        ));
    }

    let stimulus = compile_stimulus(&sources)?;
    let mut lines = vec!["* OntologyX Sim Core generated mixed-signal netlist".to_owned()];
    if !models.netlist.is_empty() {
        lines.push("* OntologyX Sim inline model registry".to_owned());
        lines.extend(models.netlist.iter().cloned());
    }
    lines.extend(analog_lines);
    if !sources.is_empty() {
        lines.push(format!(
            "a_source [{}] xsource",
            sources
                .iter()
                .map(|source| source.node.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        lines.push(format!(
            ".model xsource d_source(input_file=\"{STIMULUS_FILE}\")"
        ));
    }
    lines.extend(digital_lines);
    lines.extend(bridge_lines);
    lines.extend(model_lines);
    lines.push(".control".to_owned());
    lines.push("set noaskquit".to_owned());
    lines.push("set noacct".to_owned());
    lines.push("set digital_delay_type=2".to_owned());
    lines.push("set wr_singlescale".to_owned());
    lines.push("set wr_vecnames".to_owned());
    lines.push("option numdgt=17".to_owned());
    lines.push("esave all".to_owned());
    lines.push(format!(
        "tran {} {}",
        spice_number(step),
        spice_number(stop)
    ));
    if !analog_probes.is_empty() {
        lines.push(format!(
            "wrdata {ANALOG_RESULT_FILE} {}",
            analog_probes
                .iter()
                .map(|probe| probe.expression.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !digital_probes.is_empty() {
        let nodes = digital_probes
            .iter()
            .map(|probe| probe.node.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!("eprvcd -t 1e-15 {nodes} > {DIGITAL_RESULT_FILE}"));
    }
    lines.push("quit".to_owned());
    lines.push(".endc".to_owned());
    lines.push(".end".to_owned());
    lines.push(String::new());

    Ok(CompiledMixedRequest {
        netlist: lines.join("\n"),
        stimulus,
        analog_probes,
        digital_probes,
        delay_floor_applied,
    })
}

fn classify_nets(circuit: &Circuit) -> Result<BTreeMap<String, NetDomain>, EngineError> {
    let mut endpoint_domains = BTreeMap::new();
    for component in &circuit.components {
        for pin in &component.pins {
            endpoint_domains.insert(
                (component.id.as_str().to_owned(), pin.id.as_str().to_owned()),
                pin.domain,
            );
        }
    }

    let mut domains = BTreeMap::new();
    for net in &circuit.nets {
        let mut has_analog = false;
        let mut has_digital = false;
        for endpoint in &net.endpoints {
            let key = (
                endpoint.component.as_str().to_owned(),
                endpoint.pin.as_str().to_owned(),
            );
            let domain = endpoint_domains.get(&key).ok_or_else(|| {
                EngineError::new(
                    "mixed_signal_unknown_endpoint",
                    format!(
                        "net `{}` contains unknown endpoint `{}.{}`",
                        net.id.as_str(),
                        key.0,
                        key.1
                    ),
                )
            })?;
            match domain {
                SignalDomain::Analog | SignalDomain::Reference => has_analog = true,
                SignalDomain::Digital => has_digital = true,
                SignalDomain::Mixed => {
                    return Err(EngineError::new(
                        "mixed_signal_legacy_mixed_pin_unsupported",
                        format!(
                            "net `{}` uses SignalDomain::Mixed; R4 requires explicit adc_bridge/dac_bridge pins with separate analog and digital nets",
                            net.id.as_str()
                        ),
                    ));
                }
            }
        }
        if has_analog && has_digital {
            return Err(EngineError::new(
                "mixed_signal_direct_domain_join",
                format!(
                    "net `{}` directly joins analog and digital pins; use an explicit bridge",
                    net.id.as_str()
                ),
            ));
        }
        let domain = if has_digital {
            NetDomain::Digital
        } else {
            NetDomain::Analog
        };
        domains.insert(net.id.as_str().to_owned(), domain);
    }
    Ok(domains)
}

fn build_nodes(
    circuit: &Circuit,
    domains: &BTreeMap<String, NetDomain>,
) -> Result<MixedNodeMaps, EngineError> {
    let mut ground_nets = BTreeSet::new();
    for component in &circuit.components {
        if component.kind != ComponentKind::ground() {
            continue;
        }
        if component.pins.len() != 1 {
            return Err(component_error(
                component,
                "ground must expose exactly one pin",
            ));
        }
        for net in &circuit.nets {
            if net.endpoints.iter().any(|endpoint| {
                endpoint.component.as_str() == component.id.as_str()
                    && endpoint.pin.as_str() == component.pins[0].id.as_str()
            }) {
                ground_nets.insert(net.id.as_str().to_owned());
            }
        }
    }
    if ground_nets.is_empty() {
        return Err(EngineError::new(
            "mixed_signal_ground_required",
            "mixed-signal simulation requires an analog ground component connected to a net",
        ));
    }
    for net in &ground_nets {
        if domains.get(net) == Some(&NetDomain::Digital) {
            return Err(EngineError::new(
                "mixed_signal_digital_ground_invalid",
                format!("ground component is connected to digital net `{net}`"),
            ));
        }
    }

    let mut net_nodes = BTreeMap::new();
    for (index, net) in circuit.nets.iter().enumerate() {
        let node = if ground_nets.contains(net.id.as_str()) {
            "0".to_owned()
        } else {
            match domains
                .get(net.id.as_str())
                .copied()
                .unwrap_or(NetDomain::Analog)
            {
                NetDomain::Analog => format!("n{}", index + 1),
                NetDomain::Digital => format!("d{}", index + 1),
            }
        };
        net_nodes.insert(net.id.as_str().to_owned(), node);
    }

    let mut endpoint_net = BTreeMap::new();
    for net in &circuit.nets {
        for endpoint in &net.endpoints {
            let key = (
                endpoint.component.as_str().to_owned(),
                endpoint.pin.as_str().to_owned(),
            );
            if endpoint_net
                .insert(key.clone(), net.id.as_str().to_owned())
                .is_some()
            {
                return Err(EngineError::new(
                    "mixed_signal_endpoint_multiple_nets",
                    format!(
                        "endpoint `{}.{}` belongs to more than one net",
                        key.0, key.1
                    ),
                ));
            }
        }
    }
    Ok((net_nodes, endpoint_net))
}

fn compile_adc_bridge(
    component: &Component,
    index: usize,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
    bridge_lines: &mut Vec<String>,
    model_lines: &mut Vec<String>,
    delay_floor_applied: &mut bool,
) -> Result<(), EngineError> {
    require_bridge_pin(
        component,
        "analog_in",
        SignalDomain::Analog,
        PinDirection::Input,
    )?;
    require_bridge_pin(
        component,
        "digital_out",
        SignalDomain::Digital,
        PinDirection::Output,
    )?;
    let analog = connected_bridge_node(component, "analog_in", endpoint_net, net_nodes)?;
    let digital = connected_bridge_node(component, "digital_out", endpoint_net, net_nodes)?;
    let low = required_quantity(component, "low_threshold", Unit::Volt)?;
    let high = required_quantity(component, "high_threshold", Unit::Volt)?;
    if low >= high {
        return Err(component_error(
            component,
            "low_threshold must be strictly below high_threshold",
        ));
    }
    let requested_rise = optional_quantity(component, "rise_delay", Unit::Second)?.unwrap_or(0.0);
    let requested_fall = optional_quantity(component, "fall_delay", Unit::Second)?.unwrap_or(0.0);
    non_negative_finite(component, "rise_delay", requested_rise)?;
    non_negative_finite(component, "fall_delay", requested_fall)?;
    let rise = requested_rise.max(XSPICE_MIN_DELAY_SECONDS);
    let fall = requested_fall.max(XSPICE_MIN_DELAY_SECONDS);
    *delay_floor_applied |=
        requested_rise < XSPICE_MIN_DELAY_SECONDS || requested_fall < XSPICE_MIN_DELAY_SECONDS;
    let model = format!("madc{index}");
    bridge_lines.push(format!("aadc{index} [{analog}] [{digital}] {model}"));
    model_lines.push(format!(
        ".model {model} adc_bridge(in_low={} in_high={} rise_delay={} fall_delay={})",
        spice_number(low),
        spice_number(high),
        spice_number(rise),
        spice_number(fall)
    ));
    Ok(())
}

fn compile_dac_bridge(
    component: &Component,
    index: usize,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
    bridge_lines: &mut Vec<String>,
    model_lines: &mut Vec<String>,
    delay_floor_applied: &mut bool,
) -> Result<(), EngineError> {
    require_bridge_pin(
        component,
        "digital_in",
        SignalDomain::Digital,
        PinDirection::Input,
    )?;
    require_bridge_pin(
        component,
        "analog_out",
        SignalDomain::Analog,
        PinDirection::Output,
    )?;
    let digital = connected_bridge_node(component, "digital_in", endpoint_net, net_nodes)?;
    let analog = connected_bridge_node(component, "analog_out", endpoint_net, net_nodes)?;
    let low = required_quantity(component, "low_voltage", Unit::Volt)?;
    let high = required_quantity(component, "high_voltage", Unit::Volt)?;
    if low >= high {
        return Err(component_error(
            component,
            "low_voltage must be strictly below high_voltage",
        ));
    }
    let unknown =
        optional_quantity(component, "unknown_voltage", Unit::Volt)?.unwrap_or((low + high) / 2.0);
    let requested_rise = optional_quantity(component, "rise_time", Unit::Second)?.unwrap_or(0.0);
    let requested_fall = optional_quantity(component, "fall_time", Unit::Second)?.unwrap_or(0.0);
    let input_load = optional_quantity(component, "input_load", Unit::Farad)?.unwrap_or(0.0);
    non_negative_finite(component, "rise_time", requested_rise)?;
    non_negative_finite(component, "fall_time", requested_fall)?;
    non_negative_finite(component, "input_load", input_load)?;
    let rise = requested_rise.max(XSPICE_MIN_DELAY_SECONDS);
    let fall = requested_fall.max(XSPICE_MIN_DELAY_SECONDS);
    *delay_floor_applied |=
        requested_rise < XSPICE_MIN_DELAY_SECONDS || requested_fall < XSPICE_MIN_DELAY_SECONDS;
    let model = format!("mdac{index}");
    bridge_lines.push(format!("adac{index} [{digital}] [{analog}] {model}"));
    model_lines.push(format!(
        ".model {model} dac_bridge(out_low={} out_high={} out_undef={} input_load={} t_rise={} t_fall={})",
        spice_number(low),
        spice_number(high),
        spice_number(unknown),
        spice_number(input_load),
        spice_number(rise),
        spice_number(fall)
    ));
    Ok(())
}

fn compile_probes(
    request: &SimulationRequest,
    domains: &BTreeMap<String, NetDomain>,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
) -> Result<(Vec<AnalogProbe>, Vec<XSpiceProbe>), EngineError> {
    let mut analog = Vec::new();
    let mut digital = Vec::new();

    if request.probes.is_empty() {
        for net in &request.circuit.nets {
            let node = net_nodes
                .get(net.id.as_str())
                .expect("compiled net owns node");
            let signal = format!("net:{}", net.id.as_str());
            match domains
                .get(net.id.as_str())
                .copied()
                .unwrap_or(NetDomain::Analog)
            {
                NetDomain::Analog if node != "0" => analog.push(AnalogProbe {
                    signal,
                    expression: format!("v({node})"),
                }),
                NetDomain::Digital => digital.push(XSpiceProbe {
                    signal,
                    node: node.clone(),
                }),
                NetDomain::Analog => {}
            }
        }
        return Ok((analog, digital));
    }

    for probe in &request.probes {
        let key = (
            probe.endpoint.component.as_str().to_owned(),
            probe.endpoint.pin.as_str().to_owned(),
        );
        let net = endpoint_net.get(&key).ok_or_else(|| {
            EngineError::new(
                "mixed_signal_unknown_probe",
                format!("probe endpoint `{}.{}` is not connected", key.0, key.1),
            )
        })?;
        let node = net_nodes.get(net).ok_or_else(|| {
            EngineError::new(
                "mixed_signal_unknown_probe_net",
                format!("probe references unknown net `{net}`"),
            )
        })?;
        let signal = probe.alias.clone().unwrap_or_else(|| format!("net:{net}"));
        match domains.get(net).copied().unwrap_or(NetDomain::Analog) {
            NetDomain::Analog => analog.push(AnalogProbe {
                signal,
                expression: format!("v({node})"),
            }),
            NetDomain::Digital => digital.push(XSpiceProbe {
                signal,
                node: node.clone(),
            }),
        }
    }
    Ok((analog, digital))
}

fn require_bridge_pin(
    component: &Component,
    pin_id: &str,
    domain: SignalDomain,
    direction: PinDirection,
) -> Result<(), EngineError> {
    let Some(pin) = component.pins.iter().find(|pin| pin.id.as_str() == pin_id) else {
        return Err(component_error(
            component,
            format!("required bridge pin `{pin_id}` is missing"),
        ));
    };
    if pin.domain != domain {
        return Err(component_error(
            component,
            format!("bridge pin `{pin_id}` must use the {domain:?} signal domain"),
        ));
    }
    if pin.direction != direction {
        return Err(component_error(
            component,
            format!("bridge pin `{pin_id}` must have direction {direction:?}"),
        ));
    }
    Ok(())
}

fn connected_bridge_node(
    component: &Component,
    pin_id: &str,
    endpoint_net: &EndpointNetMap,
    net_nodes: &NetNodeMap,
) -> Result<String, EngineError> {
    let net = endpoint_net
        .get(&(component.id.as_str().to_owned(), pin_id.to_owned()))
        .ok_or_else(|| {
            component_error(
                component,
                format!("required bridge pin `{pin_id}` is not connected to a net"),
            )
        })?;
    net_nodes.get(net).cloned().ok_or_else(|| {
        component_error(
            component,
            format!("bridge pin `{pin_id}` references unknown net `{net}`"),
        )
    })
}

fn required_positive_seconds(component: &Component, key: &str) -> Result<f64, EngineError> {
    let value = required_quantity(component, key, Unit::Second)?;
    if value <= 0.0 {
        return Err(component_error(
            component,
            format!("parameter `{key}` must be greater than zero"),
        ));
    }
    Ok(value)
}

fn required_quantity(component: &Component, key: &str, unit: Unit) -> Result<f64, EngineError> {
    optional_quantity(component, key, unit)?
        .ok_or_else(|| component_error(component, format!("required parameter `{key}` is missing")))
}

fn optional_quantity(
    component: &Component,
    key: &str,
    unit: Unit,
) -> Result<Option<f64>, EngineError> {
    let Some(value) = component.parameters.get(key) else {
        return Ok(None);
    };
    let value = match value {
        ParameterValue::Number(value) => *value,
        ParameterValue::Quantity(Quantity {
            value,
            unit: actual_unit,
        }) if *actual_unit == unit => *value,
        ParameterValue::Quantity(quantity) => {
            return Err(component_error(
                component,
                format!(
                    "parameter `{key}` has unit {:?}, expected {unit:?}",
                    quantity.unit
                ),
            ));
        }
        _ => {
            return Err(component_error(
                component,
                format!("parameter `{key}` must be a number or {unit:?} quantity"),
            ));
        }
    };
    if !value.is_finite() {
        return Err(component_error(
            component,
            format!("parameter `{key}` must be finite"),
        ));
    }
    Ok(Some(value))
}

fn non_negative_finite(component: &Component, key: &str, value: f64) -> Result<(), EngineError> {
    if !value.is_finite() || value < 0.0 {
        return Err(component_error(
            component,
            format!("parameter `{key}` must be finite and non-negative"),
        ));
    }
    Ok(())
}

fn positive_finite(label: &str, value: f64) -> Result<(), EngineError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(EngineError::new(
            "mixed_signal_invalid_analysis",
            format!("{label} must be finite and greater than zero"),
        ));
    }
    Ok(())
}

fn component_error(component: &Component, message: impl Into<String>) -> EngineError {
    EngineError::new(
        "mixed_signal_component_invalid",
        format!("component `{}`: {}", component.id.as_str(), message.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Net, NetEndpoint, Pin, Probe};

    fn analog_pin(id: &str, direction: PinDirection) -> Pin {
        Pin::new(id, id, SignalDomain::Analog, direction)
    }

    fn digital_pin(id: &str, direction: PinDirection) -> Pin {
        Pin::new(id, id, SignalDomain::Digital, direction)
    }

    fn mixed_request() -> SimulationRequest {
        let source = Component::new("vin", ComponentKind::voltage_source())
            .with_pin(analog_pin("pos", PinDirection::Passive))
            .with_pin(analog_pin("neg", PinDirection::Passive))
            .with_parameter(
                "pulse_high",
                ParameterValue::Quantity(Quantity::new(3.3, Unit::Volt)),
            )
            .with_parameter(
                "pulse_width",
                ParameterValue::Quantity(Quantity::new(5e-6, Unit::Second)),
            )
            .with_parameter(
                "pulse_period",
                ParameterValue::Quantity(Quantity::new(10e-6, Unit::Second)),
            );
        let ground = Component::new("gnd", ComponentKind::ground())
            .with_pin(analog_pin("gnd", PinDirection::Passive));
        let adc = Component::new("adc", ComponentKind::adc_bridge())
            .with_pin(analog_pin("analog_in", PinDirection::Input))
            .with_pin(digital_pin("digital_out", PinDirection::Output))
            .with_parameter(
                "low_threshold",
                ParameterValue::Quantity(Quantity::new(0.8, Unit::Volt)),
            )
            .with_parameter(
                "high_threshold",
                ParameterValue::Quantity(Quantity::new(2.0, Unit::Volt)),
            );
        let inverter = Component::new("inv", ComponentKind::not_gate())
            .with_pin(digital_pin("in", PinDirection::Input))
            .with_pin(digital_pin("out", PinDirection::Output));
        let dac = Component::new("dac", ComponentKind::dac_bridge())
            .with_pin(digital_pin("digital_in", PinDirection::Input))
            .with_pin(analog_pin("analog_out", PinDirection::Output))
            .with_parameter(
                "low_voltage",
                ParameterValue::Quantity(Quantity::new(0.0, Unit::Volt)),
            )
            .with_parameter(
                "high_voltage",
                ParameterValue::Quantity(Quantity::new(3.3, Unit::Volt)),
            );
        let load = Component::new("load", ComponentKind::resistor())
            .with_pin(analog_pin("a", PinDirection::Passive))
            .with_pin(analog_pin("b", PinDirection::Passive))
            .with_parameter(
                "resistance",
                ParameterValue::Quantity(Quantity::new(10_000.0, Unit::Ohm)),
            );

        let circuit = Circuit::new()
            .with_component(source)
            .with_component(ground)
            .with_component(adc)
            .with_component(inverter)
            .with_component(dac)
            .with_component(load)
            .with_net(
                Net::new("vin")
                    .connect(NetEndpoint::new("vin", "pos"))
                    .connect(NetEndpoint::new("adc", "analog_in")),
            )
            .with_net(
                Net::new("gnd")
                    .connect(NetEndpoint::new("vin", "neg"))
                    .connect(NetEndpoint::new("gnd", "gnd"))
                    .connect(NetEndpoint::new("load", "b")),
            )
            .with_net(
                Net::new("d_adc")
                    .connect(NetEndpoint::new("adc", "digital_out"))
                    .connect(NetEndpoint::new("inv", "in")),
            )
            .with_net(
                Net::new("d_inv")
                    .connect(NetEndpoint::new("inv", "out"))
                    .connect(NetEndpoint::new("dac", "digital_in")),
            )
            .with_net(
                Net::new("vout")
                    .connect(NetEndpoint::new("dac", "analog_out"))
                    .connect(NetEndpoint::new("load", "a")),
            );

        SimulationRequest {
            circuit,
            analysis: Analysis::MixedSignalTransient {
                step: 10e-9,
                stop: 20e-6,
            },
            probes: vec![
                Probe {
                    endpoint: crate::NetEndpoint::new("adc", "digital_out"),
                    alias: Some("logic".to_owned()),
                },
                Probe {
                    endpoint: crate::NetEndpoint::new("dac", "analog_out"),
                    alias: Some("vout".to_owned()),
                },
            ],
        }
    }

    #[test]
    fn compiler_emits_explicit_adc_dac_and_hybrid_outputs() {
        let compiled = compile_request(&mixed_request()).unwrap();
        assert!(compiled.netlist.contains("adc_bridge("));
        assert!(compiled.netlist.contains("dac_bridge("));
        assert!(compiled.netlist.contains("wrdata analog.txt"));
        assert!(compiled.netlist.contains("eprvcd -t 1e-15"));
        assert_eq!(compiled.analog_probes.len(), 1);
        assert_eq!(compiled.digital_probes.len(), 1);
    }

    #[test]
    fn bridge_pin_validation_is_owned_by_mixed_engine() {
        let adc = Component::new("adc", ComponentKind::adc_bridge())
            .with_pin(analog_pin("analog_in", PinDirection::Input));
        let error = require_bridge_pin(
            &adc,
            "analog_in",
            SignalDomain::Digital,
            PinDirection::Input,
        )
        .unwrap_err();
        assert_eq!(error.code(), "mixed_signal_component_invalid");
    }

    #[test]
    fn bridge_thresholds_fail_closed() {
        let mut request = mixed_request();
        let adc = request
            .circuit
            .components
            .iter_mut()
            .find(|component| component.id.as_str() == "adc")
            .unwrap();
        adc.parameters.insert(
            "low_threshold".to_owned(),
            ParameterValue::Quantity(Quantity::new(2.5, Unit::Volt)),
        );
        let error = compile_request(&request).unwrap_err();
        assert_eq!(error.code(), "mixed_signal_component_invalid");
    }
}
