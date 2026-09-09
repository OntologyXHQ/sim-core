use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    AcScale, AnalogAxis, AnalogWaveform, Analysis, AnalysisKind, AxisKind, Circuit, Component,
    ComponentKind, Diagnostic, DiagnosticLevel, EngineCapabilities, EngineError, EngineId,
    ExecutionControl, ModelDefinition, ModelKind, ModelLanguage, ParameterValue, SignalId,
    SimulationEngine, SimulationRequest, SimulationResult, Unit, Waveform,
};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const ENGINE_ID: &str = "ngspice";
const RESULT_FILE: &str = "result.txt";
const LOG_FILE: &str = "ngspice.log";
const NETLIST_FILE: &str = "circuit.cir";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NgSpiceInfo {
    pub available: bool,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NgSpiceEngine {
    executable: PathBuf,
}

impl Default for NgSpiceEngine {
    fn default() -> Self {
        Self::new("ngspice")
    }
}

impl NgSpiceEngine {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn info(&self) -> NgSpiceInfo {
        let executable = self.executable.to_string_lossy().into_owned();
        match Command::new(&self.executable).arg("-v").output() {
            Ok(output) => {
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let version = combined
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty() && line.to_ascii_lowercase().contains("ngspice"))
                    .map(str::to_owned)
                    .or_else(|| {
                        combined
                            .lines()
                            .map(str::trim)
                            .find(|line| !line.is_empty())
                            .map(str::to_owned)
                    });
                NgSpiceInfo {
                    available: output.status.success() || version.is_some(),
                    executable,
                    version,
                }
            }
            Err(_) => NgSpiceInfo {
                available: false,
                executable,
                version: None,
            },
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
                "simulation was cancelled before ngspice execution",
            ));
        }

        let compiled = compile_request(request)?;
        enforce_buffer_limit(
            "netlist",
            compiled.netlist.len() as u64,
            control.policy.max_input_bytes,
        )?;

        let run_dir = TempRunDir::create().map_err(|error| {
            EngineError::new(
                "ngspice_tempdir_failed",
                format!("could not create ngspice run directory: {error}"),
            )
        })?;
        fs::write(
            run_dir.path().join(NETLIST_FILE),
            compiled.netlist.as_bytes(),
        )
        .map_err(|error| {
            EngineError::new(
                "ngspice_netlist_write_failed",
                format!("could not write generated netlist: {error}"),
            )
        })?;

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
                    "ngspice_spawn_failed"
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
            RESULT_FILE,
        )?;
        child.disarm();
        let log = match read_text_limited(
            &run_dir.path().join(LOG_FILE),
            control.policy.max_log_bytes,
            "log",
            "ngspice_log_read_failed",
        ) {
            Ok(log) => log,
            Err(error) if error.code() == "ngspice_log_read_failed" => String::new(),
            Err(error) => return Err(error),
        };
        if !status.success() {
            return Err(EngineError::new(
                "ngspice_failed",
                format!("ngspice exited with {status}: {}", bounded_log(&log)),
            ));
        }

        let table = read_text_limited(
            &run_dir.path().join(RESULT_FILE),
            control.policy.max_output_bytes,
            "result",
            "ngspice_result_read_failed",
        )
        .map_err(|error| {
            if error.code() == "execution_resource_limit" {
                error
            } else {
                EngineError::new(
                    "ngspice_result_missing",
                    format!(
                        "ngspice completed without a readable result table: {}; log: {}",
                        error.message(),
                        bounded_log(&log)
                    ),
                )
            }
        })?;
        let rows = parse_wrdata(
            &table,
            compiled.probes.len(),
            compiled.complex,
            compiled.axis_kind == AxisKind::Scalar,
        )?;
        if rows.is_empty() {
            return Err(EngineError::new(
                "ngspice_empty_result",
                "ngspice returned no result rows",
            ));
        }

        let axis_values = if matches!(request.analysis, Analysis::OperatingPoint) {
            vec![0.0; rows.len()]
        } else {
            rows.iter().map(|row| row.axis).collect()
        };
        let axis = AnalogAxis {
            kind: compiled.axis_kind,
            unit: compiled.axis_unit,
            values: axis_values,
        };

        let waveforms = compiled
            .probes
            .iter()
            .enumerate()
            .map(|(probe_index, probe)| {
                let values = rows.iter().map(|row| row.values[probe_index].0).collect();
                let imaginary = if compiled.complex {
                    Some(rows.iter().map(|row| row.values[probe_index].1).collect())
                } else {
                    None
                };
                Waveform::Analog(AnalogWaveform {
                    signal: SignalId::new(probe.signal.clone()),
                    unit: Unit::Volt,
                    axis: axis.clone(),
                    values,
                    imaginary,
                })
            })
            .collect();

        let info = self.info();
        let engine_version = info.version.clone();
        let mut diagnostics = Vec::new();
        if let Some(version) = info.version {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Info,
                code: "ngspice_version".to_owned(),
                message: version,
            });
        }
        for warning in extract_warnings(&log) {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                code: "ngspice_warning".to_owned(),
                message: warning,
            });
        }

        Ok(SimulationResult::new(
            self.id(),
            engine_version,
            request.analysis.kind(),
            request.circuit.schema_version,
            waveforms,
            diagnostics,
        ))
    }
}

impl SimulationEngine for NgSpiceEngine {
    fn id(&self) -> EngineId {
        EngineId::new(ENGINE_ID)
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: true,
            digital: false,
            mixed_signal: false,
            analyses: BTreeSet::from([
                AnalysisKind::OperatingPoint,
                AnalysisKind::DcSweep,
                AnalysisKind::Transient,
                AnalysisKind::AcSweep,
            ]),
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
    expression: String,
}

#[derive(Clone, Debug)]
struct CompiledRequest {
    netlist: String,
    probes: Vec<CompiledProbe>,
    axis_kind: AxisKind,
    axis_unit: Unit,
    complex: bool,
}

#[derive(Clone, Debug)]
struct OwnedTableRow {
    axis: f64,
    values: Vec<(f64, f64)>,
}

fn compile_request(request: &SimulationRequest) -> Result<CompiledRequest, EngineError> {
    let topology = Topology::build(&request.circuit)?;
    let model_registry = compile_models(&request.circuit.models)?;
    let probes = compile_probes(request, &topology)?;
    let (analysis_command, axis_kind, axis_unit, complex) =
        compile_analysis(&request.analysis, &request.circuit, &topology)?;

    let mut lines = vec!["* OntologyX Sim generated ngspice netlist".to_owned()];
    if !model_registry.netlist.is_empty() {
        lines.push("* OntologyX Sim inline model registry".to_owned());
        lines.extend(model_registry.netlist.iter().cloned());
    }
    for (index, component) in request.circuit.components.iter().enumerate() {
        if component.kind == ComponentKind::ground() {
            continue;
        }
        lines.push(compile_component(
            index,
            component,
            &topology,
            &model_registry,
        )?);
    }

    lines.push(".control".to_owned());
    lines.push("set noaskquit".to_owned());
    lines.push("set wr_singlescale".to_owned());
    lines.push("set wr_vecnames".to_owned());
    lines.push("option numdgt=17".to_owned());
    lines.push(analysis_command);
    lines.push(format!(
        "wrdata {RESULT_FILE} {}",
        probes
            .iter()
            .map(|probe| probe.expression.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    ));
    lines.push("quit".to_owned());
    lines.push(".endc".to_owned());
    lines.push(".end".to_owned());
    lines.push(String::new());

    Ok(CompiledRequest {
        netlist: lines.join("\n"),
        probes,
        axis_kind,
        axis_unit,
        complex,
    })
}

#[derive(Clone, Debug)]
struct Topology {
    endpoint_node: BTreeMap<(String, String), String>,
    endpoint_net: BTreeMap<(String, String), String>,
    source_names: BTreeMap<String, (String, Unit)>,
    net_nodes: BTreeMap<String, String>,
}

impl Topology {
    fn build(circuit: &Circuit) -> Result<Self, EngineError> {
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
            let endpoint = (
                component.id.as_str().to_owned(),
                component.pins[0].id.as_str().to_owned(),
            );
            for net in &circuit.nets {
                if net.endpoints.iter().any(|candidate| {
                    candidate.component.as_str() == endpoint.0.as_str()
                        && candidate.pin.as_str() == endpoint.1.as_str()
                }) {
                    ground_nets.insert(net.id.as_str().to_owned());
                }
            }
        }
        if ground_nets.is_empty() {
            return Err(EngineError::new(
                "ngspice_ground_required",
                "analog ngspice simulation requires at least one ground component connected to a net",
            ));
        }

        let mut net_nodes = BTreeMap::new();
        for (index, net) in circuit.nets.iter().enumerate() {
            let node = if ground_nets.contains(net.id.as_str()) {
                "0".to_owned()
            } else {
                format!("n{}", index + 1)
            };
            net_nodes.insert(net.id.as_str().to_owned(), node);
        }

        let mut endpoint_node = BTreeMap::new();
        let mut endpoint_net = BTreeMap::new();
        for net in &circuit.nets {
            let node = net_nodes
                .get(net.id.as_str())
                .expect("net node map is created from the same circuit");
            for endpoint in &net.endpoints {
                let key = (
                    endpoint.component.as_str().to_owned(),
                    endpoint.pin.as_str().to_owned(),
                );
                if endpoint_node.insert(key.clone(), node.clone()).is_some() {
                    return Err(EngineError::new(
                        "ngspice_endpoint_multiple_nets",
                        format!(
                            "endpoint `{}.{}` belongs to more than one net",
                            key.0, key.1
                        ),
                    ));
                }
                endpoint_net.insert(key, net.id.as_str().to_owned());
            }
        }

        let mut source_names = BTreeMap::new();
        let mut voltage_index = 0usize;
        let mut current_index = 0usize;
        for component in &circuit.components {
            if component.kind == ComponentKind::voltage_source() {
                voltage_index += 1;
                source_names.insert(
                    component.id.as_str().to_owned(),
                    (format!("V{voltage_index}"), Unit::Volt),
                );
            } else if component.kind == ComponentKind::current_source() {
                current_index += 1;
                source_names.insert(
                    component.id.as_str().to_owned(),
                    (format!("I{current_index}"), Unit::Ampere),
                );
            }
        }

        Ok(Self {
            endpoint_node,
            endpoint_net,
            source_names,
            net_nodes,
        })
    }

    fn node_for(&self, component: &Component, pin: &str) -> Result<&str, EngineError> {
        self.endpoint_node
            .get(&(component.id.as_str().to_owned(), pin.to_owned()))
            .map(String::as_str)
            .ok_or_else(|| {
                component_error(component, format!("pin `{pin}` is not connected to a net"))
            })
    }
}

const MAX_INLINE_MODEL_BYTES: usize = 1024 * 1024;
const MAX_INLINE_MODEL_REGISTRY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
struct CompiledModel {
    entry: String,
    kind: ModelKind,
    device_type: Option<String>,
    subcircuit_port_count: Option<usize>,
}

#[derive(Clone, Debug, Default)]
struct CompiledModelRegistry {
    entries: BTreeMap<String, CompiledModel>,
    netlist: Vec<String>,
}

fn compile_models(models: &[ModelDefinition]) -> Result<CompiledModelRegistry, EngineError> {
    let total_bytes = models.iter().map(|model| model.source.len()).sum::<usize>();
    if total_bytes > MAX_INLINE_MODEL_REGISTRY_BYTES {
        return Err(EngineError::new(
            "ngspice_model_registry_too_large",
            format!(
                "inline model registry is {total_bytes} bytes; maximum is {MAX_INLINE_MODEL_REGISTRY_BYTES} bytes"
            ),
        ));
    }

    let mut compiled = CompiledModelRegistry::default();
    for model in models {
        if model.language != ModelLanguage::Spice {
            return Err(EngineError::new(
                "ngspice_model_language_unsupported",
                format!("model `{}` is not SPICE source", model.id.as_str()),
            ));
        }
        if model.source.len() > MAX_INLINE_MODEL_BYTES {
            return Err(EngineError::new(
                "ngspice_model_too_large",
                format!(
                    "model `{}` is {} bytes; maximum is {MAX_INLINE_MODEL_BYTES} bytes",
                    model.id.as_str(),
                    model.source.len()
                ),
            ));
        }
        let metadata = validate_spice_model_source(model)?;
        if compiled.entries.contains_key(model.id.as_str()) {
            return Err(EngineError::new(
                "ngspice_duplicate_model",
                format!("duplicate model id `{}`", model.id.as_str()),
            ));
        }
        compiled.entries.insert(
            model.id.as_str().to_owned(),
            CompiledModel {
                entry: model.entry.trim().to_owned(),
                kind: model.kind,
                device_type: metadata.device_type,
                subcircuit_port_count: metadata.subcircuit_port_count,
            },
        );
        compiled.netlist.extend(
            model
                .source
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .lines()
                .map(str::to_owned),
        );
    }
    Ok(compiled)
}

#[derive(Clone, Debug, Default)]
struct SpiceModelMetadata {
    device_type: Option<String>,
    subcircuit_port_count: Option<usize>,
}

fn validate_spice_model_source(model: &ModelDefinition) -> Result<SpiceModelMetadata, EngineError> {
    let entry = model.entry.trim();
    if entry.is_empty()
        || !entry.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '$')
        })
    {
        return Err(EngineError::new(
            "ngspice_model_entry_invalid",
            format!(
                "model `{}` has an unsafe or empty entry symbol",
                model.id.as_str()
            ),
        ));
    }

    let expected_directive = match model.kind {
        ModelKind::Device => ".model",
        ModelKind::Subcircuit => ".subckt",
    };
    let mut declaration_found = false;
    let mut subcircuit_end_found = false;
    let mut metadata = SpiceModelMetadata::default();

    for (line_index, line) in model.source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            continue;
        }
        let mut tokens = trimmed.split_whitespace();
        let first = tokens.next().unwrap_or_default();
        let directive = first.to_ascii_lowercase();
        if matches!(
            directive.as_str(),
            ".control" | ".endc" | ".end" | ".include" | ".inc" | ".lib" | ".source" | ".shell"
        ) {
            return Err(EngineError::new(
                "ngspice_model_directive_forbidden",
                format!(
                    "model `{}` line {} contains forbidden directive `{first}`; inline models cannot control the simulator or read external files",
                    model.id.as_str(),
                    line_index + 1
                ),
            ));
        }
        if directive == expected_directive {
            if let Some(symbol) = tokens.next() {
                if symbol.eq_ignore_ascii_case(entry) {
                    declaration_found = true;
                    match model.kind {
                        ModelKind::Device => {
                            let device_type = tokens.next().ok_or_else(|| {
                                EngineError::new(
                                    "ngspice_model_device_type_missing",
                                    format!(
                                        "device model `{}` does not declare a SPICE device type",
                                        model.id.as_str()
                                    ),
                                )
                            })?;
                            metadata.device_type = Some(
                                device_type
                                    .trim_matches(|character| matches!(character, '(' | ')'))
                                    .to_ascii_lowercase(),
                            );
                        }
                        ModelKind::Subcircuit => {
                            let port_count = tokens
                                .take_while(|token| {
                                    let lowered = token.to_ascii_lowercase();
                                    lowered != "params:"
                                        && !lowered.starts_with("params:")
                                        && !token.contains('=')
                                })
                                .count();
                            metadata.subcircuit_port_count = Some(port_count);
                        }
                    }
                }
            }
        }
        if directive == ".ends" {
            subcircuit_end_found = true;
        }
    }

    if !declaration_found {
        return Err(EngineError::new(
            "ngspice_model_entry_missing",
            format!(
                "model `{}` does not declare `{entry}` with `{expected_directive}`",
                model.id.as_str()
            ),
        ));
    }
    if model.kind == ModelKind::Subcircuit && !subcircuit_end_found {
        return Err(EngineError::new(
            "ngspice_model_subcircuit_unterminated",
            format!(
                "subcircuit model `{}` has no `.ends` directive",
                model.id.as_str()
            ),
        ));
    }
    Ok(metadata)
}

fn named_node<'a>(
    component: &'a Component,
    topology: &'a Topology,
    pin: &str,
) -> Result<&'a str, EngineError> {
    if !component
        .pins
        .iter()
        .any(|candidate| candidate.id.as_str() == pin)
    {
        return Err(component_error(
            component,
            format!("required pin `{pin}` is missing"),
        ));
    }
    topology.node_for(component, pin)
}

fn model_reference<'models, 'component>(
    component: &'component Component,
    models: &'models CompiledModelRegistry,
) -> Result<(&'models CompiledModel, &'component str), EngineError> {
    let model_id = match component.parameters.get("model") {
        Some(ParameterValue::Text(value)) if !value.trim().is_empty() => value.trim(),
        Some(_) => {
            return Err(component_error(
                component,
                "parameter `model` must be a non-empty string",
            ));
        }
        None => {
            return Err(component_error(
                component,
                "required parameter `model` is missing",
            ));
        }
    };
    let model = models.entries.get(model_id).ok_or_else(|| {
        component_error(
            component,
            format!("model `{model_id}` is not present in the circuit model registry"),
        )
    })?;
    Ok((model, model_id))
}

fn required_device_model<'a>(
    component: &Component,
    models: &'a CompiledModelRegistry,
    accepted_device_types: &[&str],
) -> Result<&'a CompiledModel, EngineError> {
    let (model, model_id) = model_reference(component, models)?;
    if model.kind != ModelKind::Device {
        return Err(component_error(
            component,
            format!("model `{model_id}` is not a device model"),
        ));
    }
    let device_type = model.device_type.as_deref().unwrap_or_default();
    if !accepted_device_types
        .iter()
        .any(|accepted| device_type.eq_ignore_ascii_case(accepted))
    {
        return Err(component_error(
            component,
            format!(
                "model `{model_id}` declares SPICE type `{device_type}`, expected one of {}",
                accepted_device_types.join(", ")
            ),
        ));
    }
    Ok(model)
}

fn required_subcircuit_model<'a>(
    component: &Component,
    models: &'a CompiledModelRegistry,
    expected_ports: Option<usize>,
) -> Result<&'a CompiledModel, EngineError> {
    let (model, model_id) = model_reference(component, models)?;
    if model.kind != ModelKind::Subcircuit {
        return Err(component_error(
            component,
            format!("model `{model_id}` is not a subcircuit model"),
        ));
    }
    if let Some(expected_ports) = expected_ports {
        if model.subcircuit_port_count != Some(expected_ports) {
            return Err(component_error(
                component,
                format!(
                    "subcircuit model `{model_id}` exposes {} port(s); component requires {expected_ports}",
                    model.subcircuit_port_count.unwrap_or(0)
                ),
            ));
        }
    }
    Ok(model)
}

fn compile_component(
    index: usize,
    component: &Component,
    topology: &Topology,
    models: &CompiledModelRegistry,
) -> Result<String, EngineError> {
    let two_pin = || -> Result<(&str, &str), EngineError> {
        if component.pins.len() != 2 {
            return Err(component_error(
                component,
                "component must expose exactly two pins",
            ));
        }
        Ok((
            topology.node_for(component, component.pins[0].id.as_str())?,
            topology.node_for(component, component.pins[1].id.as_str())?,
        ))
    };

    if component.kind == ComponentKind::resistor() {
        let (a, b) = two_pin()?;
        let value = required_positive(component, "resistance", Unit::Ohm)?;
        return Ok(format!("R{} {a} {b} {}", index + 1, spice_number(value)));
    }
    if component.kind == ComponentKind::capacitor() {
        let (a, b) = two_pin()?;
        let value = required_positive(component, "capacitance", Unit::Farad)?;
        return Ok(format!("C{} {a} {b} {}", index + 1, spice_number(value)));
    }
    if component.kind == ComponentKind::inductor() {
        let (a, b) = two_pin()?;
        let value = required_positive(component, "inductance", Unit::Henry)?;
        return Ok(format!("L{} {a} {b} {}", index + 1, spice_number(value)));
    }
    if component.kind == ComponentKind::diode() {
        let anode = named_node(component, topology, "anode")?;
        let cathode = named_node(component, topology, "cathode")?;
        let model = required_device_model(component, models, &["d"])?;
        return Ok(format!("D{} {anode} {cathode} {}", index + 1, model.entry));
    }
    if component.kind == ComponentKind::bjt() {
        let collector = named_node(component, topology, "collector")?;
        let base = named_node(component, topology, "base")?;
        let emitter = named_node(component, topology, "emitter")?;
        let model = required_device_model(component, models, &["npn", "pnp"])?;
        return Ok(format!(
            "Q{} {collector} {base} {emitter} {}",
            index + 1,
            model.entry
        ));
    }
    if component.kind == ComponentKind::mosfet() {
        let drain = named_node(component, topology, "drain")?;
        let gate = named_node(component, topology, "gate")?;
        let source = named_node(component, topology, "source")?;
        let bulk = named_node(component, topology, "bulk")?;
        let model = required_device_model(component, models, &["nmos", "pmos"])?;
        return Ok(format!(
            "M{} {drain} {gate} {source} {bulk} {}",
            index + 1,
            model.entry
        ));
    }
    if component.kind == ComponentKind::op_amp() {
        let non_inverting = named_node(component, topology, "non_inverting")?;
        let inverting = named_node(component, topology, "inverting")?;
        let positive_supply = named_node(component, topology, "positive_supply")?;
        let negative_supply = named_node(component, topology, "negative_supply")?;
        let output = named_node(component, topology, "output")?;
        let model = required_subcircuit_model(component, models, Some(5))?;
        return Ok(format!(
            "X{} {non_inverting} {inverting} {positive_supply} {negative_supply} {output} {}",
            index + 1,
            model.entry
        ));
    }
    if component.kind == ComponentKind::subcircuit() {
        if component.pins.is_empty() {
            return Err(component_error(
                component,
                "subcircuit component must expose at least one pin",
            ));
        }
        let model = required_subcircuit_model(component, models, None)?;
        let nodes = component
            .pins
            .iter()
            .map(|pin| topology.node_for(component, pin.id.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(format!(
            "X{} {} {}",
            index + 1,
            nodes.join(" "),
            model.entry
        ));
    }
    if component.kind == ComponentKind::voltage_source()
        || component.kind == ComponentKind::current_source()
    {
        let (positive, negative) = two_pin()?;
        let (name, unit) = topology
            .source_names
            .get(component.id.as_str())
            .ok_or_else(|| component_error(component, "source name was not allocated"))?;
        let dc = optional_number(component, "dc", *unit)?.unwrap_or(0.0);
        let mut source = format!("{name} {positive} {negative} DC {}", spice_number(dc));
        if let Some(high) = optional_number(component, "pulse_high", *unit)? {
            let low = optional_number(component, "pulse_low", *unit)?.unwrap_or(dc);
            let delay = optional_number(component, "pulse_delay", Unit::Second)?.unwrap_or(0.0);
            let rise = optional_number(component, "pulse_rise", Unit::Second)?.unwrap_or(1e-12);
            let fall = optional_number(component, "pulse_fall", Unit::Second)?.unwrap_or(1e-12);
            let width = required_positive(component, "pulse_width", Unit::Second)?;
            let period = required_positive(component, "pulse_period", Unit::Second)?;
            source.push_str(&format!(
                " PULSE({} {} {} {} {} {} {})",
                spice_number(low),
                spice_number(high),
                spice_number(delay),
                spice_number(rise),
                spice_number(fall),
                spice_number(width),
                spice_number(period),
            ));
        }
        if let Some(magnitude) = optional_number(component, "ac_magnitude", *unit)? {
            if magnitude < 0.0 {
                return Err(component_error(
                    component,
                    "parameter `ac_magnitude` must be non-negative",
                ));
            }
            let phase =
                optional_number(component, "ac_phase_degrees", Unit::Dimensionless)?.unwrap_or(0.0);
            source.push_str(&format!(
                " AC {} {}",
                spice_number(magnitude),
                spice_number(phase)
            ));
        }
        return Ok(source);
    }

    Err(component_error(
        component,
        format!(
            "component kind `{}` is not supported by the R2 ngspice engine",
            component.kind.as_str()
        ),
    ))
}

fn compile_analysis(
    analysis: &Analysis,
    circuit: &Circuit,
    topology: &Topology,
) -> Result<(String, AxisKind, Unit, bool), EngineError> {
    match analysis {
        Analysis::OperatingPoint => Ok((
            "op".to_owned(),
            AxisKind::Scalar,
            Unit::Dimensionless,
            false,
        )),
        Analysis::Transient { step, stop } => {
            positive_finite("transient step", *step)?;
            positive_finite("transient stop", *stop)?;
            if step > stop {
                return Err(EngineError::new(
                    "ngspice_invalid_analysis",
                    "transient step must not exceed stop time",
                ));
            }
            Ok((
                format!("tran {} {}", spice_number(*step), spice_number(*stop)),
                AxisKind::Time,
                Unit::Second,
                false,
            ))
        }
        Analysis::DcSweep {
            source,
            start,
            stop,
            step,
        } => {
            finite("DC sweep start", *start)?;
            finite("DC sweep stop", *stop)?;
            finite("DC sweep step", *step)?;
            if *step == 0.0 || (*stop - *start).signum() != step.signum() {
                return Err(EngineError::new(
                    "ngspice_invalid_analysis",
                    "DC sweep step must be non-zero and move from start toward stop",
                ));
            }
            let (spice_source, unit) = topology.source_names.get(source).ok_or_else(|| {
                EngineError::new(
                    "ngspice_unknown_sweep_source",
                    format!(
                        "DC sweep source `{source}` is not a voltage/current source in the circuit"
                    ),
                )
            })?;
            if !circuit
                .components
                .iter()
                .any(|component| component.id.as_str() == source)
            {
                return Err(EngineError::new(
                    "ngspice_unknown_sweep_source",
                    format!("unknown source `{source}`"),
                ));
            }
            Ok((
                format!(
                    "dc {spice_source} {} {} {}",
                    spice_number(*start),
                    spice_number(*stop),
                    spice_number(*step)
                ),
                AxisKind::DcSweep,
                *unit,
                false,
            ))
        }
        Analysis::AcSweep {
            scale,
            points,
            start_hz,
            stop_hz,
        } => {
            if *points == 0 {
                return Err(EngineError::new(
                    "ngspice_invalid_analysis",
                    "AC sweep points must be greater than zero",
                ));
            }
            positive_finite("AC start frequency", *start_hz)?;
            positive_finite("AC stop frequency", *stop_hz)?;
            if start_hz >= stop_hz {
                return Err(EngineError::new(
                    "ngspice_invalid_analysis",
                    "AC start frequency must be below stop frequency",
                ));
            }
            let scale = match scale {
                AcScale::Linear => "lin",
                AcScale::Decade => "dec",
                AcScale::Octave => "oct",
            };
            Ok((
                format!(
                    "ac {scale} {points} {} {}",
                    spice_number(*start_hz),
                    spice_number(*stop_hz)
                ),
                AxisKind::Frequency,
                Unit::Hertz,
                true,
            ))
        }
        Analysis::DigitalTransient { .. } | Analysis::MixedSignalTransient { .. } => {
            Err(EngineError::new(
                "ngspice_analysis_unsupported",
                "R2 ngspice engine supports analog analyses only; digital/mixed-signal lands in the next engine slice",
            ))
        }
    }
}

fn compile_probes(
    request: &SimulationRequest,
    topology: &Topology,
) -> Result<Vec<CompiledProbe>, EngineError> {
    let mut probes = Vec::new();
    let mut seen_nodes = BTreeSet::new();

    if request.probes.is_empty() {
        for net in &request.circuit.nets {
            let Some(node) = topology.net_nodes.get(net.id.as_str()) else {
                continue;
            };
            if node == "0" || !seen_nodes.insert(node.clone()) {
                continue;
            }
            probes.push(CompiledProbe {
                signal: format!("net:{}", net.id.as_str()),
                expression: format!("v({node})"),
            });
        }
    } else {
        for probe in &request.probes {
            let key = (
                probe.endpoint.component.as_str().to_owned(),
                probe.endpoint.pin.as_str().to_owned(),
            );
            let node = topology.endpoint_node.get(&key).ok_or_else(|| {
                EngineError::new(
                    "ngspice_unknown_probe",
                    format!("probe endpoint `{}.{}` is not connected", key.0, key.1),
                )
            })?;
            let net = topology
                .endpoint_net
                .get(&key)
                .expect("endpoint net and node are populated together");
            probes.push(CompiledProbe {
                signal: probe.alias.clone().unwrap_or_else(|| format!("net:{net}")),
                expression: format!("v({node})"),
            });
        }
    }

    if probes.is_empty() {
        return Err(EngineError::new(
            "ngspice_no_probes",
            "simulation has no non-ground analog node to observe; add a probe or a non-ground net",
        ));
    }
    Ok(probes)
}

fn parameter_value(
    component: &Component,
    key: &str,
    expected_unit: Unit,
) -> Result<Option<f64>, EngineError> {
    let Some(value) = component.parameters.get(key) else {
        return Ok(None);
    };
    let number = match value {
        ParameterValue::Integer(value) => *value as f64,
        ParameterValue::Number(value) => *value,
        ParameterValue::Quantity(quantity) if quantity.unit == expected_unit => quantity.value,
        ParameterValue::Quantity(quantity) => {
            return Err(component_error(
                component,
                format!(
                    "parameter `{key}` expects {expected_unit:?}, got {:?}",
                    quantity.unit
                ),
            ));
        }
        ParameterValue::Boolean(_) | ParameterValue::Text(_) => {
            return Err(component_error(
                component,
                format!("parameter `{key}` must be numeric"),
            ));
        }
    };
    if !number.is_finite() {
        return Err(component_error(
            component,
            format!("parameter `{key}` must be finite"),
        ));
    }
    Ok(Some(number))
}

fn optional_number(
    component: &Component,
    key: &str,
    unit: Unit,
) -> Result<Option<f64>, EngineError> {
    parameter_value(component, key, unit)
}

fn required_positive(component: &Component, key: &str, unit: Unit) -> Result<f64, EngineError> {
    let value = parameter_value(component, key, unit)?.ok_or_else(|| {
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

fn finite(label: &str, value: f64) -> Result<(), EngineError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(EngineError::new(
            "ngspice_invalid_analysis",
            format!("{label} must be finite"),
        ))
    }
}

fn positive_finite(label: &str, value: f64) -> Result<(), EngineError> {
    finite(label, value)?;
    if value <= 0.0 {
        return Err(EngineError::new(
            "ngspice_invalid_analysis",
            format!("{label} must be greater than zero"),
        ));
    }
    Ok(())
}

fn component_error(component: &Component, message: impl Into<String>) -> EngineError {
    EngineError::new(
        "ngspice_component_invalid",
        format!("component `{}`: {}", component.id.as_str(), message.into()),
    )
}

fn spice_number(value: f64) -> String {
    format!("{value:.17e}")
}

fn parse_wrdata(
    source: &str,
    signal_count: usize,
    complex: bool,
    scalar_without_scale: bool,
) -> Result<Vec<OwnedTableRow>, EngineError> {
    let value_columns = signal_count * if complex { 2 } else { 1 };
    let expected_columns = 1 + value_columns;
    let mut rows = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let Ok(first) = tokens[0].parse::<f64>() else {
            // wr_vecnames emits one header row; any other non-numeric line is ignored.
            continue;
        };
        let scalar_row_without_scale = scalar_without_scale && tokens.len() == value_columns;
        if tokens.len() != expected_columns && !scalar_row_without_scale {
            return Err(EngineError::new(
                "ngspice_result_shape",
                format!(
                    "expected {expected_columns} numeric columns, got {} in `{trimmed}`",
                    tokens.len()
                ),
            ));
        }
        let mut numeric = Vec::with_capacity(tokens.len());
        numeric.push(first);
        for token in &tokens[1..] {
            let value = token.parse::<f64>().map_err(|error| {
                EngineError::new(
                    "ngspice_result_parse",
                    format!("could not parse `{token}`: {error}"),
                )
            })?;
            if !value.is_finite() {
                return Err(EngineError::new(
                    "ngspice_non_finite_result",
                    format!("ngspice returned non-finite value `{token}`"),
                ));
            }
            numeric.push(value);
        }
        let offset = if scalar_row_without_scale { 0 } else { 1 };
        let mut values = Vec::with_capacity(signal_count);
        if complex {
            for index in 0..signal_count {
                values.push((numeric[offset + index * 2], numeric[offset + index * 2 + 1]));
            }
        } else {
            for index in 0..signal_count {
                values.push((numeric[offset + index], 0.0));
            }
        }
        let axis = if scalar_row_without_scale {
            0.0
        } else {
            numeric[0]
        };
        rows.push(OwnedTableRow { axis, values });
    }
    Ok(rows)
}

pub(crate) fn wait_for_child_with_files(
    child: &mut Child,
    run_dir: &Path,
    control: &ExecutionControl,
    log_file: &str,
    result_file: &str,
) -> Result<ExitStatus, EngineError> {
    let started = Instant::now();
    let poll_interval = Duration::from_millis(control.policy.poll_interval_ms);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                return Err(EngineError::new(
                    "ngspice_wait_failed",
                    format!("could not query ngspice process status: {error}"),
                ));
            }
        }

        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                "ngspice execution was cancelled",
            ));
        }

        if control.policy.timeout_ms != 0
            && started.elapsed() >= Duration::from_millis(control.policy.timeout_ms)
        {
            return Err(EngineError::new(
                "execution_timeout",
                format!(
                    "ngspice exceeded execution timeout of {} ms",
                    control.policy.timeout_ms
                ),
            )
            .retryable(true));
        }

        enforce_file_limit(&run_dir.join(log_file), "log", control.policy.max_log_bytes)?;
        enforce_file_limit(
            &run_dir.join(result_file),
            "result",
            control.policy.max_output_bytes,
        )?;

        thread::sleep(poll_interval);
    }
}

fn enforce_file_limit(path: &Path, resource: &str, max_bytes: u64) -> Result<(), EngineError> {
    if max_bytes == 0 {
        return Ok(());
    }
    match fs::metadata(path) {
        Ok(metadata) => enforce_buffer_limit(resource, metadata.len(), max_bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EngineError::new(
            "ngspice_resource_inspection_failed",
            format!("could not inspect ngspice {resource} file: {error}"),
        )),
    }
}

pub(crate) fn enforce_buffer_limit(
    resource: &str,
    actual: u64,
    max_bytes: u64,
) -> Result<(), EngineError> {
    if max_bytes != 0 && actual > max_bytes {
        return Err(EngineError::new(
            "execution_resource_limit",
            format!("{resource} size {actual} bytes exceeds configured limit of {max_bytes} bytes"),
        ));
    }
    Ok(())
}

pub(crate) fn read_text_limited(
    path: &Path,
    max_bytes: u64,
    resource: &str,
    read_error_code: &str,
) -> Result<String, EngineError> {
    let metadata = fs::metadata(path).map_err(|error| {
        EngineError::new(
            read_error_code,
            format!("could not inspect ngspice {resource} file: {error}"),
        )
    })?;
    enforce_buffer_limit(resource, metadata.len(), max_bytes)?;
    fs::read_to_string(path).map_err(|error| {
        EngineError::new(
            read_error_code,
            format!("could not read ngspice {resource} file: {error}"),
        )
    })
}

pub(crate) fn bounded_log(log: &str) -> String {
    const LIMIT: usize = 4000;
    let trimmed = log.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_owned();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

pub(crate) fn extract_warnings(log: &str) -> Vec<String> {
    log.lines()
        .map(str::trim)
        .filter(|line| {
            let lowered = line.to_ascii_lowercase();
            lowered.starts_with("warning") || lowered.contains("warning:")
        })
        .take(16)
        .map(str::to_owned)
        .collect()
}

pub(crate) struct ChildGuard {
    child: Child,
    armed: bool,
}

impl ChildGuard {
    pub(crate) fn new(child: Child) -> Self {
        Self { child, armed: true }
    }

    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(crate) struct TempRunDir {
    path: PathBuf,
}

impl TempRunDir {
    pub(crate) fn create() -> io::Result<Self> {
        for _ in 0..128 {
            let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ontologyx-sim-core-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique ontologyx-sim-core run directory",
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRunDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, NetEndpoint, Pin, PinDirection, Probe, Quantity, SignalDomain};

    fn analog_pin(id: &str) -> Pin {
        Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
    }

    fn divider_request(analysis: Analysis) -> SimulationRequest {
        let source = Component::new("v1", ComponentKind::voltage_source())
            .with_pin(analog_pin("pos"))
            .with_pin(analog_pin("neg"))
            .with_parameter(
                "dc",
                ParameterValue::Quantity(Quantity::new(5.0, Unit::Volt)),
            );
        let r1 = Component::new("r1", ComponentKind::resistor())
            .with_pin(analog_pin("a"))
            .with_pin(analog_pin("b"))
            .with_parameter(
                "resistance",
                ParameterValue::Quantity(Quantity::new(1000.0, Unit::Ohm)),
            );
        let r2 = Component::new("r2", ComponentKind::resistor())
            .with_pin(analog_pin("a"))
            .with_pin(analog_pin("b"))
            .with_parameter(
                "resistance",
                ParameterValue::Quantity(Quantity::new(1000.0, Unit::Ohm)),
            );
        let ground = Component::new("gnd", ComponentKind::ground()).with_pin(Pin::new(
            "gnd",
            "gnd",
            SignalDomain::Reference,
            PinDirection::Passive,
        ));
        let circuit = Circuit::new()
            .with_component(source)
            .with_component(r1)
            .with_component(r2)
            .with_component(ground)
            .with_net(
                crate::Net::new("vin")
                    .connect(NetEndpoint::new("v1", "pos"))
                    .connect(NetEndpoint::new("r1", "a")),
            )
            .with_net(
                crate::Net::new("out")
                    .connect(NetEndpoint::new("r1", "b"))
                    .connect(NetEndpoint::new("r2", "a")),
            )
            .with_net(
                crate::Net::new("gnd")
                    .connect(NetEndpoint::new("v1", "neg"))
                    .connect(NetEndpoint::new("r2", "b"))
                    .connect(NetEndpoint::new("gnd", "gnd")),
            );
        SimulationRequest {
            circuit,
            analysis,
            probes: vec![Probe {
                endpoint: NetEndpoint::new("r1", "b"),
                alias: Some("vout".to_owned()),
            }],
        }
    }

    #[test]
    fn compiler_emits_deterministic_voltage_divider_netlist() {
        let compiled = compile_request(&divider_request(Analysis::OperatingPoint)).unwrap();
        assert!(
            compiled
                .netlist
                .contains("V1 n1 0 DC 5.00000000000000000e0")
                || compiled
                    .netlist
                    .contains("V1 n1 0 DC 5.00000000000000000e+00")
        );
        assert!(compiled.netlist.contains("R2 n1 n2"));
        assert!(compiled.netlist.contains("R3 n2 0"));
        assert!(compiled.netlist.contains("wrdata result.txt v(n2)"));
        assert!(compiled.netlist.contains("\nop\n"));
    }

    #[test]
    fn wrdata_real_table_parses_single_scale() {
        let source = "time v(n1) v(n2)\n0.0 1.0 2.0\n1e-3 1.5 2.5\n";
        let rows = parse_wrdata(source, 2, false, false).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].axis, 1e-3);
        assert_eq!(rows[1].values[0], (1.5, 0.0));
        assert_eq!(rows[1].values[1], (2.5, 0.0));
    }

    #[test]
    fn wrdata_complex_table_preserves_real_and_imaginary_parts() {
        let source = "frequency v(n1) v(n1)\n1.0 0.8 -0.2\n10.0 0.1 -0.3\n";
        let rows = parse_wrdata(source, 1, true, false).unwrap();
        assert_eq!(rows[0].values[0], (0.8, -0.2));
        assert_eq!(rows[1].values[0], (0.1, -0.3));
    }

    #[test]
    fn model_registry_emits_declared_device_model() {
        let models = compile_models(&[ModelDefinition::spice_device(
            "diode-model",
            "DTEST",
            ".model DTEST D (IS=1e-14 N=1)",
        )])
        .unwrap();
        assert_eq!(models.entries.get("diode-model").unwrap().entry, "DTEST");
        assert!(
            models
                .netlist
                .iter()
                .any(|line| line.contains(".model DTEST D"))
        );
    }

    #[test]
    fn inline_model_cannot_include_external_files_or_control_flow() {
        for source in [
            ".include /tmp/untrusted-model.lib",
            ".control\nshell touch /tmp/nope\n.endc",
            ".end",
        ] {
            let error = compile_models(&[ModelDefinition::spice_device("bad", "DTEST", source)])
                .expect_err("unsafe inline model must fail closed");
            assert_eq!(error.code, "ngspice_model_directive_forbidden");
        }
    }

    #[test]
    fn subcircuit_model_requires_declared_entry_and_ends() {
        let missing_entry = compile_models(&[ModelDefinition::spice_subcircuit(
            "opamp",
            "OX_OPAMP",
            ".subckt OTHER INP INN OUT\nE1 OUT 0 INP INN 1e5\n.ends OTHER",
        )])
        .expect_err("entry mismatch must fail");
        assert_eq!(missing_entry.code, "ngspice_model_entry_missing");

        let missing_ends = compile_models(&[ModelDefinition::spice_subcircuit(
            "opamp",
            "OX_OPAMP",
            ".subckt OX_OPAMP INP INN OUT\nE1 OUT 0 INP INN 1e5",
        )])
        .expect_err("unterminated subcircuit must fail");
        assert_eq!(missing_ends.code, "ngspice_model_subcircuit_unterminated");
    }
}
