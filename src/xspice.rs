use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::ngspice::{
    ChildGuard, NgSpiceEngine, TempRunDir, bounded_log, enforce_buffer_limit, extract_warnings,
    read_text_limited, wait_for_child_with_files,
};
use crate::{
    Analysis, AnalysisKind, Circuit, Component, Diagnostic, DiagnosticLevel, DigitalTransition,
    DigitalWaveform, EngineCapabilities, EngineError, EngineId, ExecutionControl, LogicValue,
    MAX_DIGITAL_EVENTS, ParameterValue, PinDirection, Probe, Quantity, SignalDomain, SignalId,
    SimulationEngine, SimulationRequest, SimulationResult, Unit, Waveform,
};

pub const XSPICE_ENGINE_ID: &str = "xspice";
pub const XSPICE_MIN_DELAY_SECONDS: f64 = 1e-12;
const NETLIST_FILE: &str = "xspice.cir";
const STIMULUS_FILE: &str = "source.txt";
const LOG_FILE: &str = "xspice.log";
const VCD_FILE: &str = "result.vcd";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct XSpiceInfo {
    pub available: bool,
    pub xspice_available: bool,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct XSpiceEngine {
    executable: PathBuf,
}

impl Default for XSpiceEngine {
    fn default() -> Self {
        Self::new("ngspice")
    }
}

impl XSpiceEngine {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn info(&self) -> XSpiceInfo {
        let ngspice = NgSpiceEngine::new(self.executable.clone()).info();
        let xspice_available = ngspice.available && probe_xspice(&self.executable);
        XSpiceInfo {
            available: ngspice.available,
            xspice_available,
            executable: ngspice.executable,
            version: ngspice.version,
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
                "simulation was cancelled before XSPICE execution",
            ));
        }

        let compiled = compile_request(request)?;
        let input_bytes = compiled
            .netlist
            .len()
            .saturating_add(compiled.stimulus.len()) as u64;
        enforce_buffer_limit("xspice input", input_bytes, control.policy.max_input_bytes)?;

        let run_dir = TempRunDir::create().map_err(|error| {
            EngineError::new(
                "xspice_tempdir_failed",
                format!("could not create XSPICE run directory: {error}"),
            )
        })?;
        fs::write(
            run_dir.path().join(NETLIST_FILE),
            compiled.netlist.as_bytes(),
        )
        .map_err(|error| {
            EngineError::new(
                "xspice_netlist_write_failed",
                format!("could not write generated XSPICE netlist: {error}"),
            )
        })?;
        if !compiled.stimulus.is_empty() {
            fs::write(
                run_dir.path().join(STIMULUS_FILE),
                compiled.stimulus.as_bytes(),
            )
            .map_err(|error| {
                EngineError::new(
                    "xspice_stimulus_write_failed",
                    format!("could not write generated XSPICE stimulus: {error}"),
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
                    "xspice_spawn_failed"
                };
                EngineError::new(
                    code,
                    format!("could not execute `{}`: {error}", self.executable.display()),
                )
            })?;
        let mut child = ChildGuard::new(child);
        let status = wait_for_child_with_files(
            child.child_mut(),
            run_dir.path(),
            control,
            LOG_FILE,
            VCD_FILE,
        )?;
        child.disarm();

        let log = match read_text_limited(
            &run_dir.path().join(LOG_FILE),
            control.policy.max_log_bytes,
            "log",
            "xspice_log_read_failed",
        ) {
            Ok(log) => log,
            Err(error) if error.code() == "xspice_log_read_failed" => String::new(),
            Err(error) => return Err(error),
        };
        if !status.success() {
            return Err(EngineError::new(
                "xspice_failed",
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

        let vcd = read_text_limited(
            &run_dir.path().join(VCD_FILE),
            control.policy.max_output_bytes,
            "result",
            "xspice_vcd_read_failed",
        )
        .map_err(|error| {
            if error.code() == "execution_resource_limit" {
                error
            } else {
                EngineError::new(
                    "xspice_result_missing",
                    format!(
                        "XSPICE completed without a readable VCD result: {}; log: {}",
                        error.message(),
                        bounded_log(&log)
                    ),
                )
            }
        })?;
        let waveforms = parse_vcd(&vcd, &compiled.probes)?;

        let info = self.info();
        let mut diagnostics = vec![
            Diagnostic {
                level: DiagnosticLevel::Info,
                code: "xspice_backend".to_owned(),
                message: "ngspice XSPICE event engine".to_owned(),
            },
            Diagnostic {
                level: DiagnosticLevel::Info,
                code: "xspice_initialization_semantics".to_owned(),
                message: "XSPICE initializes digital event nodes to zero at simulation start; startup transitions can differ from the built-in X-initialized reference engine before the first causal transition".to_owned(),
            },
        ];
        if compiled.delay_floor_applied {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                code: "xspice_delay_floor".to_owned(),
                message: format!(
                    "XSPICE gate delay values below {XSPICE_MIN_DELAY_SECONDS:.3e} s were clamped to the solver minimum"
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
                code: "xspice_warning".to_owned(),
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

impl SimulationEngine for XSpiceEngine {
    fn id(&self) -> EngineId {
        EngineId::new(XSPICE_ENGINE_ID)
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
        self.info().version
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

#[derive(Clone, Debug)]
struct CompiledProbe {
    signal: String,
    node: String,
}

#[derive(Clone, Debug)]
struct CompiledXSpiceRequest {
    netlist: String,
    stimulus: String,
    probes: Vec<CompiledProbe>,
    delay_floor_applied: bool,
}

#[derive(Clone, Debug)]
struct SourceSpec {
    node: String,
    events: Vec<(f64, LogicValue)>,
}

fn compile_request(request: &SimulationRequest) -> Result<CompiledXSpiceRequest, EngineError> {
    let Analysis::DigitalTransient { stop } = &request.analysis else {
        return Err(EngineError::new(
            "xspice_unsupported_analysis",
            "the XSPICE engine only supports digital_transient analysis",
        ));
    };
    let stop = *stop;
    if !stop.is_finite() || stop <= 0.0 {
        return Err(EngineError::new(
            "xspice_invalid_analysis",
            "digital transient stop time must be finite and greater than zero",
        ));
    }

    let mut net_nodes = BTreeMap::new();
    for (index, net) in request.circuit.nets.iter().enumerate() {
        net_nodes.insert(net.id.as_str().to_owned(), format!("d{index}"));
    }
    if net_nodes.is_empty() {
        return Err(EngineError::new(
            "xspice_no_nets",
            "XSPICE digital simulation requires at least one net",
        ));
    }

    let endpoint_net = endpoint_net_map(&request.circuit)?;
    let mut sources = Vec::new();
    let mut gate_lines = Vec::new();
    let mut model_lines = Vec::new();
    let mut delay_floor_applied = false;

    for (index, component) in request.circuit.components.iter().enumerate() {
        match component.kind.as_str() {
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
                let model_name = format!("xg{index}");
                let instance_name = format!("ag{index}");
                let input_expr = if input_nodes.len() == 1 {
                    input_nodes[0].clone()
                } else {
                    format!("[{}]", input_nodes.join(" "))
                };
                gate_lines.push(format!(
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
                let model_name = format!("xt{index}");
                gate_lines.push(format!("at{index} {input} {enable} {output} {model_name}"));
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
                    index,
                    &endpoint_net,
                    &net_nodes,
                    &mut gate_lines,
                    &mut model_lines,
                    &mut delay_floor_applied,
                )?;
            }
            other => {
                return Err(EngineError::new(
                    "xspice_unsupported_component",
                    format!(
                        "component `{}` has unsupported XSPICE digital kind `{other}`",
                        component.id.as_str()
                    ),
                ));
            }
        }
    }

    let probes = compile_probes(request, &endpoint_net, &net_nodes)?;
    let stimulus = compile_stimulus(&sources)?;
    let mut lines = vec![
        "* OntologyX Sim Core generated XSPICE digital netlist".to_owned(),
        "Vdummy dummy 0 DC=0".to_owned(),
    ];
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
    lines.extend(gate_lines);
    lines.extend(model_lines);
    lines.push(".control".to_owned());
    lines.push("set noaskquit".to_owned());
    lines.push("set noacct".to_owned());
    lines.push("set digital_delay_type=2".to_owned());
    lines.push("esave all".to_owned());
    let step = (stop / 1000.0).max(1e-15).min(stop);
    lines.push(format!(
        "tran {} {}",
        spice_number(step),
        spice_number(stop)
    ));
    let vcd_nodes = probes
        .iter()
        .map(|probe| probe.node.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(format!("eprvcd -t 1e-15 {vcd_nodes} > {VCD_FILE}"));
    lines.push("quit".to_owned());
    lines.push(".endc".to_owned());
    lines.push(".end".to_owned());
    lines.push(String::new());

    Ok(CompiledXSpiceRequest {
        netlist: lines.join("\n"),
        stimulus,
        probes,
        delay_floor_applied,
    })
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
                    "xspice_endpoint_on_multiple_nets",
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

fn gate_contract(
    kind: &str,
) -> Result<(Vec<&'static str>, &'static str, &'static str), EngineError> {
    match kind {
        "buffer" => Ok((vec!["in"], "out", "d_buffer")),
        "not_gate" => Ok((vec!["in"], "out", "d_inverter")),
        "and_gate" => Ok((vec!["a", "b"], "out", "d_and")),
        "or_gate" => Ok((vec!["a", "b"], "out", "d_or")),
        "xor_gate" => Ok((vec!["a", "b"], "out", "d_xor")),
        "nand_gate" => Ok((vec!["a", "b"], "out", "d_nand")),
        "nor_gate" => Ok((vec!["a", "b"], "out", "d_nor")),
        "xnor_gate" => Ok((vec!["a", "b"], "out", "d_xnor")),
        _ => Err(EngineError::new(
            "xspice_unsupported_gate",
            format!("unsupported XSPICE gate kind `{kind}`"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_sequential_xspice(
    component: &Component,
    index: usize,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
    instance_lines: &mut Vec<String>,
    model_lines: &mut Vec<String>,
    delay_floor_applied: &mut bool,
) -> Result<(), EngineError> {
    if let Some(ParameterValue::Text(edge)) = component.parameters.get("edge") {
        let normalized = edge.trim().to_ascii_lowercase();
        if !matches!(normalized.as_str(), "rising" | "positive" | "pos") {
            return Err(EngineError::new(
                "xspice_unsupported_clock_edge",
                format!(
                    "component `{}` requests `{edge}` clocking but the canonical XSPICE sequential models are rising-edge triggered",
                    component.id.as_str()
                ),
            ));
        }
    }

    let requested_delay = optional_non_negative_seconds(component, "delay")?.unwrap_or(0.0);
    let delay = requested_delay.max(XSPICE_MIN_DELAY_SECONDS);
    *delay_floor_applied |= requested_delay < XSPICE_MIN_DELAY_SECONDS;
    let model_name = format!("xs{index}");

    let require_node = |pin: &str, direction: PinDirection| -> Result<String, EngineError> {
        require_pin(component, pin, direction)?;
        connected_node(component, pin, endpoint_net, net_nodes)
    };
    let require_controls = || -> Result<(String, String, String), EngineError> {
        let set = require_node("set", PinDirection::Input)?;
        let reset = require_node("reset", PinDirection::Input)?;
        let nq = require_node("nq", PinDirection::Output)?;
        Ok((set, reset, nq))
    };

    let ic = match optional_logic_value(component, "initial")?.unwrap_or(LogicValue::X) {
        LogicValue::Zero => 0,
        LogicValue::One => 1,
        LogicValue::X | LogicValue::Z => 2,
    };

    match component.kind.as_str() {
        "d_flip_flop" => {
            let d = require_node("d", PinDirection::Input)?;
            let clk = require_node("clk", PinDirection::Input)?;
            let q = require_node("q", PinDirection::Output)?;
            let (set, reset, nq) = require_controls()?;
            instance_lines.push(format!(
                "as{index} {d} {clk} {set} {reset} {q} {nq} {model_name}"
            ));
            model_lines.push(format!(
                ".model {model_name} d_dff(clk_delay={} set_delay={} reset_delay={} ic={ic} rise_delay={} fall_delay={})",
                spice_number(delay),
                spice_number(delay),
                spice_number(delay),
                spice_number(XSPICE_MIN_DELAY_SECONDS),
                spice_number(XSPICE_MIN_DELAY_SECONDS)
            ));
        }
        "jk_flip_flop" | "t_flip_flop" => {
            let (j, k) = if component.kind.as_str() == "t_flip_flop" {
                let t = require_node("t", PinDirection::Input)?;
                (t.clone(), t)
            } else {
                (
                    require_node("j", PinDirection::Input)?,
                    require_node("k", PinDirection::Input)?,
                )
            };
            let clk = require_node("clk", PinDirection::Input)?;
            let q = require_node("q", PinDirection::Output)?;
            let (set, reset, nq) = require_controls()?;
            instance_lines.push(format!(
                "as{index} {j} {k} {clk} {set} {reset} {q} {nq} {model_name}"
            ));
            model_lines.push(format!(
                ".model {model_name} d_jkff(clk_delay={} set_delay={} reset_delay={} ic={ic} rise_delay={} fall_delay={})",
                spice_number(delay),
                spice_number(delay),
                spice_number(delay),
                spice_number(XSPICE_MIN_DELAY_SECONDS),
                spice_number(XSPICE_MIN_DELAY_SECONDS)
            ));
        }
        "sr_flip_flop" => {
            let s = require_node("s", PinDirection::Input)?;
            let r = require_node("r", PinDirection::Input)?;
            let clk = require_node("clk", PinDirection::Input)?;
            let q = require_node("q", PinDirection::Output)?;
            let (set, reset, nq) = require_controls()?;
            instance_lines.push(format!(
                "as{index} {s} {r} {clk} {set} {reset} {q} {nq} {model_name}"
            ));
            model_lines.push(format!(
                ".model {model_name} d_srff(clk_delay={} set_delay={} reset_delay={} ic={ic} rise_delay={} fall_delay={})",
                spice_number(delay),
                spice_number(delay),
                spice_number(delay),
                spice_number(XSPICE_MIN_DELAY_SECONDS),
                spice_number(XSPICE_MIN_DELAY_SECONDS)
            ));
        }
        "d_latch" => {
            let d = require_node("d", PinDirection::Input)?;
            let enable = require_node("enable", PinDirection::Input)?;
            let q = require_node("q", PinDirection::Output)?;
            let (set, reset, nq) = require_controls()?;
            instance_lines.push(format!(
                "as{index} {d} {enable} {set} {reset} {q} {nq} {model_name}"
            ));
            model_lines.push(format!(
                ".model {model_name} d_dlatch(data_delay={} enable_delay={} set_delay={} reset_delay={} ic={ic} rise_delay={} fall_delay={})",
                spice_number(delay),
                spice_number(delay),
                spice_number(delay),
                spice_number(delay),
                spice_number(XSPICE_MIN_DELAY_SECONDS),
                spice_number(XSPICE_MIN_DELAY_SECONDS)
            ));
        }
        _ => {
            return Err(EngineError::new(
                "xspice_unsupported_sequential",
                format!(
                    "component `{}` is not a native XSPICE sequential primitive",
                    component.id.as_str()
                ),
            ));
        }
    }
    Ok(())
}

fn compile_probes(
    request: &SimulationRequest,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
) -> Result<Vec<CompiledProbe>, EngineError> {
    if request.probes.is_empty() {
        return Ok(request
            .circuit
            .nets
            .iter()
            .map(|net| CompiledProbe {
                signal: format!("net:{}", net.id.as_str()),
                node: net_nodes
                    .get(net.id.as_str())
                    .cloned()
                    .expect("compiled XSPICE net owns node alias"),
            })
            .collect());
    }

    request
        .probes
        .iter()
        .map(|probe| compile_probe(probe, endpoint_net, net_nodes))
        .collect()
}

fn compile_probe(
    probe: &Probe,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
) -> Result<CompiledProbe, EngineError> {
    let key = (
        probe.endpoint.component.as_str().to_owned(),
        probe.endpoint.pin.as_str().to_owned(),
    );
    let net = endpoint_net.get(&key).ok_or_else(|| {
        EngineError::new(
            "xspice_unknown_probe",
            format!("probe endpoint `{}.{}` is not connected", key.0, key.1),
        )
    })?;
    let node = net_nodes.get(net).cloned().ok_or_else(|| {
        EngineError::new(
            "xspice_unknown_probe_net",
            format!("probe references unknown net `{net}`"),
        )
    })?;
    Ok(CompiledProbe {
        signal: probe.alias.clone().unwrap_or_else(|| format!("net:{net}")),
        node,
    })
}

fn compile_stimulus(sources: &[SourceSpec]) -> Result<String, EngineError> {
    if sources.is_empty() {
        return Ok(String::new());
    }
    let mut times = sources
        .iter()
        .flat_map(|source| source.events.iter().map(|event| event.0))
        .collect::<Vec<_>>();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|left, right| left.to_bits() == right.to_bits());
    if times.len() > MAX_DIGITAL_EVENTS {
        return Err(EngineError::new(
            "execution_resource_limit",
            format!(
                "generated XSPICE stimulus contains {} event rows, exceeding limit {MAX_DIGITAL_EVENTS}",
                times.len()
            ),
        ));
    }

    let mut indices = vec![0_usize; sources.len()];
    let mut current = sources
        .iter()
        .map(|source| source.events[0].1)
        .collect::<Vec<_>>();
    let mut lines = vec!["* time and XSPICE digital source states".to_owned()];
    for time in times {
        for (source_index, source) in sources.iter().enumerate() {
            while indices[source_index] + 1 < source.events.len()
                && source.events[indices[source_index] + 1].0 <= time
            {
                indices[source_index] += 1;
                current[source_index] = source.events[indices[source_index]].1;
            }
        }
        lines.push(format!(
            "{} {}",
            spice_number(time),
            current
                .iter()
                .map(|value| xspice_state(*value))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn clock_events(
    stop: f64,
    period: f64,
    duty_cycle: f64,
    initial: LogicValue,
) -> Result<Vec<(f64, LogicValue)>, EngineError> {
    if !initial.is_known() {
        return Err(EngineError::new(
            "xspice_invalid_clock",
            "XSPICE digital clock initial value must be zero or one",
        ));
    }
    let high_duration = period * duty_cycle;
    let low_duration = period * (1.0 - duty_cycle);
    let mut events = vec![(0.0, initial)];
    let mut time = 0.0;
    let mut value = initial;
    while time < stop {
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
        events.push((time, value));
        if events.len() > MAX_DIGITAL_EVENTS {
            return Err(EngineError::new(
                "execution_resource_limit",
                format!("generated XSPICE clock exceeds event limit {MAX_DIGITAL_EVENTS}"),
            ));
        }
    }
    Ok(events)
}

fn xspice_state(value: LogicValue) -> &'static str {
    match value {
        LogicValue::Zero => "0s",
        LogicValue::One => "1s",
        LogicValue::X => "Us",
        LogicValue::Z => "Uz",
    }
}

fn parse_vcd(source: &str, probes: &[CompiledProbe]) -> Result<Vec<Waveform>, EngineError> {
    let timescale = parse_vcd_timescale(source)?;
    let mut id_to_reference = BTreeMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("$var ") {
            continue;
        }
        let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
        if tokens.len() < 6 {
            return Err(EngineError::new(
                "xspice_vcd_declaration",
                format!("malformed VCD variable declaration `{trimmed}`"),
            ));
        }
        id_to_reference.insert(
            tokens[3].to_owned(),
            tokens[4].trim_start_matches('\\').to_owned(),
        );
    }

    let declared_nodes = id_to_reference.values().cloned().collect::<BTreeSet<_>>();
    let requested = probes
        .iter()
        .map(|probe| probe.node.as_str())
        .collect::<BTreeSet<_>>();
    let mut histories = requested
        .iter()
        .map(|node| {
            (
                (*node).to_owned(),
                vec![DigitalTransition {
                    time: 0.0,
                    value: LogicValue::X,
                }],
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut current_time = 0.0;
    let mut in_values = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("$enddefinitions") {
            in_values = true;
            continue;
        }
        if !in_values || trimmed.is_empty() || trimmed.starts_with('$') {
            continue;
        }
        if let Some(raw_time) = trimmed.strip_prefix('#') {
            let tick = raw_time.parse::<f64>().map_err(|error| {
                EngineError::new(
                    "xspice_vcd_time",
                    format!("could not parse VCD timestamp `{raw_time}`: {error}"),
                )
            })?;
            current_time = tick * timescale;
            continue;
        }

        let mut chars = trimmed.chars();
        let Some(value_char) = chars.next() else {
            continue;
        };
        let value = match value_char {
            '0' => LogicValue::Zero,
            '1' => LogicValue::One,
            'x' | 'X' | 'u' | 'U' => LogicValue::X,
            'z' | 'Z' => LogicValue::Z,
            _ => continue,
        };
        let identifier = chars.as_str().trim();
        let Some(reference) = id_to_reference.get(identifier) else {
            continue;
        };
        if let Some(history) = histories.get_mut(reference) {
            push_transition(history, current_time, value);
        }
    }

    for node in &requested {
        if !declared_nodes.contains(*node) {
            return Err(EngineError::new(
                "xspice_vcd_missing_signal",
                format!("VCD result did not contain requested event node `{node}`"),
            ));
        }
    }

    probes
        .iter()
        .map(|probe| {
            Ok(Waveform::Digital(DigitalWaveform {
                signal: SignalId::new(probe.signal.clone()),
                transitions: histories
                    .get(&probe.node)
                    .cloned()
                    .expect("requested XSPICE probe owns history"),
            }))
        })
        .collect()
}

fn parse_vcd_timescale(source: &str) -> Result<f64, EngineError> {
    let Some(start) = source.find("$timescale") else {
        return Err(EngineError::new(
            "xspice_vcd_timescale",
            "VCD result is missing a timescale declaration",
        ));
    };
    let remainder = &source[start + "$timescale".len()..];
    let Some(end) = remainder.find("$end") else {
        return Err(EngineError::new(
            "xspice_vcd_timescale",
            "VCD timescale declaration is not terminated",
        ));
    };
    let text = remainder[..end].trim();
    let compact = text
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let digit_count = compact.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return Err(EngineError::new(
            "xspice_vcd_timescale",
            format!("unsupported VCD timescale `{text}`"),
        ));
    }
    let magnitude = compact[..digit_count].parse::<f64>().map_err(|error| {
        EngineError::new(
            "xspice_vcd_timescale",
            format!("could not parse VCD timescale `{text}`: {error}"),
        )
    })?;
    let unit = &compact[digit_count..];
    let multiplier = match unit {
        "s" => 1.0,
        "ms" => 1e-3,
        "us" => 1e-6,
        "ns" => 1e-9,
        "ps" => 1e-12,
        "fs" => 1e-15,
        _ => {
            return Err(EngineError::new(
                "xspice_vcd_timescale",
                format!("unsupported VCD timescale unit `{unit}`"),
            ));
        }
    };
    Ok(magnitude * multiplier)
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

fn connected_node(
    component: &Component,
    pin: &str,
    endpoint_net: &BTreeMap<(String, String), String>,
    net_nodes: &BTreeMap<String, String>,
) -> Result<String, EngineError> {
    let net = endpoint_net
        .get(&(component.id.as_str().to_owned(), pin.to_owned()))
        .ok_or_else(|| {
            component_error(
                component,
                format!("required digital pin `{pin}` is not connected to a net"),
            )
        })?;
    net_nodes.get(net).cloned().ok_or_else(|| {
        component_error(
            component,
            format!("digital pin `{pin}` references unknown net `{net}`"),
        )
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
                "parameter `duty_cycle` must be dimensionless",
            ));
        }
    };
    if !duty.is_finite() || duty <= 0.0 || duty >= 1.0 {
        return Err(component_error(
            component,
            "parameter `duty_cycle` must be finite and strictly between zero and one",
        ));
    }
    Ok(duty)
}

fn component_error(component: &Component, message: impl Into<String>) -> EngineError {
    EngineError::new(
        "xspice_component_invalid",
        format!("component `{}`: {}", component.id.as_str(), message.into()),
    )
}

fn spice_number(value: f64) -> String {
    format!("{value:.17e}")
}

fn log_has_xspice_failure(log: &str) -> bool {
    let lowered = log.to_ascii_lowercase();
    lowered.contains("unknown model type d_")
        || lowered.contains("unknown device type")
        || lowered.contains("d_source: source.txt file was not read successfully")
}

fn probe_xspice(executable: &Path) -> bool {
    let Ok(run_dir) = TempRunDir::create() else {
        return false;
    };
    let netlist = "* OntologyX XSPICE capability probe\nVdummy dummy 0 DC=0\na_source [din] source_model\n.model source_model d_source(input_file=\"source.txt\")\na_gate din dout gate_model\n.model gate_model d_buffer(rise_delay=1e-12 fall_delay=1e-12)\n.control\nset noaskquit\ntran 1e-12 5e-12\neprvcd -t 1e-15 dout > result.vcd\nquit\n.endc\n.end\n";
    if fs::write(run_dir.path().join(NETLIST_FILE), netlist).is_err()
        || fs::write(run_dir.path().join(STIMULUS_FILE), "0 0s\n1e-12 1s\n").is_err()
    {
        return false;
    }
    let Ok(status) = Command::new(executable)
        .arg("-n")
        .arg("-b")
        .arg("-o")
        .arg(LOG_FILE)
        .arg(NETLIST_FILE)
        .current_dir(run_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    else {
        return false;
    };
    if !status.success() {
        return false;
    }
    let log = fs::read_to_string(run_dir.path().join(LOG_FILE)).unwrap_or_default();
    if log_has_xspice_failure(&log) {
        return false;
    }
    fs::metadata(run_dir.path().join(VCD_FILE))
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentKind, Net, NetEndpoint, Pin};

    fn digital_pin(id: &str, direction: PinDirection) -> Pin {
        Pin::new(id, id, SignalDomain::Digital, direction)
    }

    #[test]
    fn compiler_emits_d_source_and_xspice_gate_models() {
        let circuit = Circuit::new()
            .with_component(
                Component::new("a", ComponentKind::logic_input())
                    .with_pin(digital_pin("out", PinDirection::Output))
                    .with_parameter("value", ParameterValue::Integer(1)),
            )
            .with_component(
                Component::new("inv", ComponentKind::not_gate())
                    .with_pin(digital_pin("in", PinDirection::Input))
                    .with_pin(digital_pin("out", PinDirection::Output))
                    .with_parameter(
                        "delay",
                        ParameterValue::Quantity(Quantity::new(5e-9, Unit::Second)),
                    ),
            )
            .with_component(
                Component::new("out", ComponentKind::logic_output())
                    .with_pin(digital_pin("in", PinDirection::Input)),
            )
            .with_net(
                Net::new("in")
                    .connect(NetEndpoint::new("a", "out"))
                    .connect(NetEndpoint::new("inv", "in")),
            )
            .with_net(
                Net::new("out")
                    .connect(NetEndpoint::new("inv", "out"))
                    .connect(NetEndpoint::new("out", "in")),
            );
        let request = SimulationRequest {
            circuit,
            analysis: Analysis::DigitalTransient { stop: 20e-9 },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("out", "in"),
                alias: Some("out".to_owned()),
            }],
        };
        let compiled = compile_request(&request).unwrap();
        assert!(compiled.netlist.contains("d_source"));
        assert!(compiled.netlist.contains("d_inverter"));
        assert!(compiled.netlist.contains("set digital_delay_type=2"));
        assert!(compiled.netlist.contains("rise_delay=5."));
        assert!(compiled.netlist.contains("eprvcd -t 1e-15"));
        assert!(compiled.stimulus.contains("1s"));
    }

    #[test]
    fn vcd_parser_normalizes_event_transitions() {
        let vcd = "$timescale 1 ns $end\n$scope module logic $end\n$var wire 1 ! d0 $end\n$upscope $end\n$enddefinitions $end\n#0\nx!\n#5\n1!\n#10\n0!\n";
        let waveforms = parse_vcd(
            vcd,
            &[CompiledProbe {
                signal: "out".to_owned(),
                node: "d0".to_owned(),
            }],
        )
        .unwrap();
        let Waveform::Digital(waveform) = &waveforms[0] else {
            panic!("expected digital waveform");
        };
        assert_eq!(waveform.transitions.len(), 3);
        assert_eq!(waveform.transitions[0].value, LogicValue::X);
        assert!((waveform.transitions[1].time - 5e-9).abs() < 1e-18);
        assert_eq!(waveform.transitions[1].value, LogicValue::One);
        assert_eq!(waveform.transitions[2].value, LogicValue::Zero);
    }
}
