use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::firmware::{FirmwareArtifact, FirmwareFormat};
use crate::ngspice::{
    ChildGuard, TempRunDir, bounded_log, enforce_buffer_limit, read_text_limited,
};
use crate::{
    Analysis, AnalysisKind, Component, ComponentId, ComponentKind, Diagnostic, DiagnosticLevel,
    DigitalTransition, DigitalWaveform, EngineCapabilities, EngineError, EngineId,
    ExecutionControl, LogicValue, PinDirection, Probe, SignalDomain, SignalId, SimulationEngine,
    SimulationRequest, SimulationResult, Waveform,
};

pub const RENODE_ENGINE_ID: &str = "renode";
pub const MAX_RENODE_SAMPLE_POINTS: usize = 100_000;
const SCRIPT_FILE: &str = "run.resc";
const LOG_FILE: &str = "renode.log";
const SAMPLE_MARKER: &str = "OXSIM_GPIO|";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenodeInfo {
    pub available: bool,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Backend configuration for one MCU component and one immutable firmware artifact.
///
/// Platform paths are Renode search-path relative (for example
/// `platforms/boards/stm32f4_discovery-kit.repl`). Keeping the artifact outside
/// `SimulationRequest` avoids inflating the solver-independent circuit schema with
/// executable bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenodeTarget {
    pub component: ComponentId,
    pub platform: String,
    pub firmware: FirmwareArtifact,
    #[serde(default = "default_cpu_path")]
    pub cpu: String,
    #[serde(default)]
    pub gpio_observers: BTreeMap<String, String>,
}

fn default_cpu_path() -> String {
    "sysbus.cpu".to_owned()
}

impl RenodeTarget {
    pub fn new(
        component: impl Into<ComponentId>,
        platform: impl Into<String>,
        firmware: FirmwareArtifact,
    ) -> Self {
        Self {
            component: component.into(),
            platform: platform.into(),
            firmware,
            cpu: default_cpu_path(),
            gpio_observers: BTreeMap::new(),
        }
    }

    pub fn with_cpu(mut self, cpu: impl Into<String>) -> Self {
        self.cpu = cpu.into();
        self
    }

    /// Reuse an observer already exposed by the Renode platform for a GPIO mapping.
    pub fn with_gpio_observer(
        mut self,
        mapping: impl Into<String>,
        object: impl Into<String>,
    ) -> Self {
        self.gpio_observers.insert(mapping.into(), object.into());
        self
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        self.firmware.validate()?;
        if !is_safe_platform_path(&self.platform) {
            return Err(EngineError::new(
                "renode_target_invalid",
                "Renode platform must be a safe search-path-relative .repl path without `..` segments",
            ));
        }
        if !is_safe_object_path(&self.cpu) {
            return Err(EngineError::new(
                "renode_target_invalid",
                "Renode CPU path must contain only identifier segments separated by dots",
            ));
        }
        for (mapping, object) in &self.gpio_observers {
            parse_gpio_mapping(mapping).map_err(|message| {
                EngineError::new(
                    "renode_target_invalid",
                    format!("Renode GPIO observer mapping `{mapping}` is invalid: {message}"),
                )
            })?;
            if !is_safe_object_path(object) {
                return Err(EngineError::new(
                    "renode_target_invalid",
                    format!(
                        "Renode GPIO observer `{object}` must contain only identifier segments separated by dots"
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct RenodeEngine {
    executable: PathBuf,
    target: RenodeTarget,
}

impl RenodeEngine {
    pub fn new(target: RenodeTarget) -> Self {
        Self::with_executable("renode", target)
    }

    pub fn with_executable(executable: impl Into<PathBuf>, target: RenodeTarget) -> Self {
        Self {
            executable: executable.into(),
            target,
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn target(&self) -> &RenodeTarget {
        &self.target
    }

    pub fn info(&self) -> RenodeInfo {
        match Command::new(&self.executable).arg("-v").output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                let version = if stdout.is_empty() {
                    (!stderr.is_empty()).then_some(stderr)
                } else {
                    Some(stdout)
                };
                RenodeInfo {
                    available: true,
                    executable: self.executable.display().to_string(),
                    version,
                }
            }
            _ => RenodeInfo {
                available: false,
                executable: self.executable.display().to_string(),
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
                "simulation was cancelled before Renode execution",
            ));
        }
        self.target.validate()?;

        let compiled = compile_request(request, &self.target)?;
        let input_bytes = self.target.firmware.bytes.len() as u64 + compiled.script.len() as u64;
        enforce_buffer_limit(
            "Renode firmware and generated script input",
            input_bytes,
            control.policy.max_input_bytes,
        )?;

        let run_dir = TempRunDir::create().map_err(|error| {
            EngineError::new(
                "renode_tempdir_failed",
                format!("could not create Renode run directory: {error}"),
            )
        })?;
        let firmware_file = format!("firmware.{}", self.target.firmware.extension());
        fs::write(
            run_dir.path().join(&firmware_file),
            &self.target.firmware.bytes,
        )
        .map_err(|error| {
            EngineError::new(
                "renode_firmware_write_failed",
                format!("could not write firmware artifact: {error}"),
            )
        })?;
        fs::write(run_dir.path().join(SCRIPT_FILE), compiled.script.as_bytes()).map_err(
            |error| {
                EngineError::new(
                    "renode_script_write_failed",
                    format!("could not write generated Renode script: {error}"),
                )
            },
        )?;

        let info = self.info();
        let log = fs::File::create(run_dir.path().join(LOG_FILE)).map_err(|error| {
            EngineError::new(
                "renode_log_create_failed",
                format!("could not create Renode log: {error}"),
            )
        })?;
        let stderr = log.try_clone().map_err(|error| {
            EngineError::new(
                "renode_log_create_failed",
                format!("could not clone Renode log handle: {error}"),
            )
        })?;
        let started = Instant::now();
        let child = Command::new(&self.executable)
            .arg("--disable-gui")
            .arg("--console")
            .arg("-p")
            .arg(SCRIPT_FILE)
            .current_dir(run_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                let code = if error.kind() == io::ErrorKind::NotFound {
                    "renode_not_found"
                } else {
                    "renode_spawn_failed"
                };
                EngineError::new(
                    code,
                    format!("could not execute `{}`: {error}", self.executable.display()),
                )
            })?;
        let mut child = ChildGuard::new(child);
        let status = wait_for_renode_child(child.child_mut(), run_dir.path(), control, &started)?;
        child.disarm();

        let output = read_text_limited(
            &run_dir.path().join(LOG_FILE),
            control.policy.max_log_bytes,
            "Renode log",
            "renode_log_read_failed",
        )?;
        if !status.success() {
            return Err(EngineError::new(
                "renode_execution_failed",
                format!("Renode exited with {status}: {}", bounded_log(&output)),
            ));
        }

        let samples = parse_samples(&output, compiled.sources.len())?;
        let waveforms = compiled
            .probes
            .iter()
            .map(|probe| {
                Waveform::Digital(DigitalWaveform {
                    signal: probe.signal.clone(),
                    transitions: transitions_from_samples(&samples[probe.source_index]),
                })
            })
            .collect::<Vec<_>>();
        let diagnostics = vec![Diagnostic {
            level: DiagnosticLevel::Info,
            code: "renode_gpio_sampled".to_owned(),
            message: format!(
                "Renode MCU firmware executed on virtual time with {:.17}s GPIO sampling; peripherals and instruction execution remain owned by Renode",
                compiled.step
            ),
        }];

        Ok(SimulationResult::new(
            EngineId::new(RENODE_ENGINE_ID),
            info.version,
            AnalysisKind::FirmwareTransient,
            request.circuit.schema_version,
            waveforms,
            diagnostics,
        ))
    }
}

impl SimulationEngine for RenodeEngine {
    fn id(&self) -> EngineId {
        EngineId::new(RENODE_ENGINE_ID)
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            analog: false,
            digital: true,
            mixed_signal: false,
            analyses: BTreeSet::from([AnalysisKind::FirmwareTransient]),
        }
    }

    fn version(&self) -> Option<String> {
        self.info().version
    }

    fn supports_request(&self, request: &SimulationRequest) -> bool {
        request.analysis.kind() == AnalysisKind::FirmwareTransient
            && request.circuit.components.iter().any(|component| {
                component.id == self.target.component && component.kind == ComponentKind::mcu()
            })
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
struct GpioSource {
    port: String,
    pin: u32,
    object: String,
    synthetic_observer: bool,
}

#[derive(Clone, Debug)]
struct RenodeProbe {
    signal: SignalId,
    source_index: usize,
}

#[derive(Clone, Debug)]
struct CompiledRenodeRequest {
    script: String,
    step: f64,
    sources: Vec<GpioSource>,
    probes: Vec<RenodeProbe>,
}

fn compile_request(
    request: &SimulationRequest,
    target: &RenodeTarget,
) -> Result<CompiledRenodeRequest, EngineError> {
    let Analysis::FirmwareTransient { step, stop } = &request.analysis else {
        return Err(EngineError::new(
            "renode_analysis_unsupported",
            "RenodeEngine requires Analysis::FirmwareTransient",
        ));
    };
    validate_time(*step, *stop)?;

    let component = request
        .circuit
        .components
        .iter()
        .find(|component| component.id == target.component)
        .ok_or_else(|| {
            EngineError::new(
                "renode_target_missing",
                format!(
                    "target MCU component `{}` is not present in the circuit",
                    target.component.as_str()
                ),
            )
        })?;
    if component.kind != ComponentKind::mcu() {
        return Err(component_error(
            component,
            "target component must use kind `mcu`",
        ));
    }

    for candidate in &request.circuit.components {
        if candidate.id == target.component {
            continue;
        }
        if candidate.kind != ComponentKind::logic_output() {
            return Err(component_error(
                candidate,
                "R6 Renode execution accepts the target mcu plus passive logic_output observation components; cross-engine circuit scheduling is intentionally deferred to the shared co-simulation scheduler",
            ));
        }
    }

    let sources = compile_gpio_sources(component, target)?;
    let endpoint_source = build_endpoint_source_map(request, component, &sources)?;
    let probes = compile_probes(request, &endpoint_source)?;
    let sample_times = sample_times(*step, *stop, sources.len())?;
    let script = build_script(target, &sources, &sample_times)?;

    Ok(CompiledRenodeRequest {
        script,
        step: *step,
        sources,
        probes,
    })
}

fn compile_gpio_sources(
    component: &Component,
    target: &RenodeTarget,
) -> Result<Vec<GpioSource>, EngineError> {
    if component.pins.is_empty() {
        return Err(component_error(
            component,
            "mcu must expose at least one GPIO output pin in R6",
        ));
    }
    let mut mappings = BTreeSet::new();
    component
        .pins
        .iter()
        .enumerate()
        .map(|(index, pin)| {
            if pin.domain != SignalDomain::Digital || pin.direction != PinDirection::Output {
                return Err(component_error(
                    component,
                    format!(
                        "MCU pin `{}` must be a digital Output in R6; bidirectional/input synchronization belongs to the shared co-simulation scheduler",
                        pin.id.as_str()
                    ),
                ));
            }
            let (port, gpio_pin) = parse_gpio_mapping(&pin.name).map_err(|message| {
                component_error(
                    component,
                    format!("pin `{}` has invalid GPIO mapping: {message}", pin.id.as_str()),
                )
            })?;
            if !mappings.insert((port.clone(), gpio_pin)) {
                return Err(component_error(
                    component,
                    format!("Renode GPIO mapping `{port}@{gpio_pin}` appears more than once"),
                ));
            }
            let mapping = format!("{port}@{gpio_pin}");
            let observer = target.gpio_observers.get(&mapping);
            Ok(GpioSource {
                object: observer
                    .cloned()
                    .unwrap_or_else(|| format!("sysbus.{port}.oxsim_gpio_{index}")),
                port,
                pin: gpio_pin,
                synthetic_observer: observer.is_none(),
            })
        })
        .collect()
}

fn build_endpoint_source_map(
    request: &SimulationRequest,
    component: &Component,
    sources: &[GpioSource],
) -> Result<BTreeMap<(String, String), usize>, EngineError> {
    let pin_index = component
        .pins
        .iter()
        .enumerate()
        .map(|(index, pin)| (pin.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut map = component
        .pins
        .iter()
        .enumerate()
        .map(|(index, pin)| {
            (
                (component.id.as_str().to_owned(), pin.id.as_str().to_owned()),
                index,
            )
        })
        .collect::<BTreeMap<_, _>>();

    for net in &request.circuit.nets {
        let source_indices = net
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.component == component.id)
            .filter_map(|endpoint| pin_index.get(endpoint.pin.as_str()).copied())
            .collect::<BTreeSet<_>>();
        if source_indices.len() > 1 {
            return Err(EngineError::new(
                "renode_net_invalid",
                format!(
                    "net `{}` connects more than one MCU GPIO output and is ambiguous for R6 observation",
                    net.id.as_str()
                ),
            ));
        }
        let Some(source_index) = source_indices.iter().next().copied() else {
            continue;
        };
        if source_index >= sources.len() {
            return Err(EngineError::new(
                "renode_internal_error",
                "compiled GPIO source index is out of range",
            ));
        }
        for endpoint in &net.endpoints {
            map.insert(
                (
                    endpoint.component.as_str().to_owned(),
                    endpoint.pin.as_str().to_owned(),
                ),
                source_index,
            );
        }
    }
    Ok(map)
}

fn compile_probes(
    request: &SimulationRequest,
    endpoint_source: &BTreeMap<(String, String), usize>,
) -> Result<Vec<RenodeProbe>, EngineError> {
    request
        .probes
        .iter()
        .map(|probe| compile_probe(probe, endpoint_source))
        .collect()
}

fn compile_probe(
    probe: &Probe,
    endpoint_source: &BTreeMap<(String, String), usize>,
) -> Result<RenodeProbe, EngineError> {
    let key = (
        probe.endpoint.component.as_str().to_owned(),
        probe.endpoint.pin.as_str().to_owned(),
    );
    let source_index = endpoint_source.get(&key).copied().ok_or_else(|| {
        EngineError::new(
            "renode_probe_invalid",
            format!(
                "probe endpoint `{}.{}` is not driven by the configured MCU GPIO outputs",
                probe.endpoint.component.as_str(),
                probe.endpoint.pin.as_str()
            ),
        )
    })?;
    Ok(RenodeProbe {
        signal: SignalId::new(probe.alias.clone().unwrap_or_else(|| {
            format!(
                "{}.{}",
                probe.endpoint.component.as_str(),
                probe.endpoint.pin.as_str()
            )
        })),
        source_index,
    })
}

fn sample_times(step: f64, stop: f64, source_count: usize) -> Result<Vec<f64>, EngineError> {
    let mut times = vec![0.0];
    let mut current = 0.0;
    while current < stop {
        let next = (current + step).min(stop);
        if next <= current {
            return Err(EngineError::new(
                "renode_time_invalid",
                "firmware sampling step does not advance virtual time",
            ));
        }
        times.push(next);
        current = next;
        if times.len().saturating_mul(source_count) > MAX_RENODE_SAMPLE_POINTS {
            return Err(EngineError::new(
                "execution_resource_limit",
                format!(
                    "Renode firmware run would create more than {MAX_RENODE_SAMPLE_POINTS} GPIO sample points"
                ),
            ));
        }
    }
    Ok(times)
}

fn build_script(
    target: &RenodeTarget,
    sources: &[GpioSource],
    times: &[f64],
) -> Result<String, EngineError> {
    let firmware_file = format!("firmware.{}", target.firmware.extension());
    let mut lines = vec![
        "using sysbus".to_owned(),
        "mach create \"oxsim\"".to_owned(),
        format!("machine LoadPlatformDescription @{}", target.platform),
    ];
    for (index, source) in sources.iter().enumerate() {
        if !source.synthetic_observer {
            continue;
        }
        lines.push(format!(
            "machine LoadPlatformDescriptionFromString \"{}: {{ {} -> oxsim_gpio_{}@0 }}; oxsim_gpio_{}: Miscellaneous.LED @ {} {}\"",
            source.port, source.pin, index, index, source.port, source.pin
        ));
    }
    lines.push(match target.firmware.format {
        FirmwareFormat::Elf => format!("sysbus LoadELF @{firmware_file}"),
        FirmwareFormat::IntelHex => format!("sysbus LoadHEX @{firmware_file}"),
        FirmwareFormat::Binary => {
            let address = target.firmware.load_address.ok_or_else(|| {
                EngineError::new(
                    "firmware_artifact_invalid",
                    "raw binary firmware requires an explicit load_address",
                )
            })?;
            format!("sysbus LoadBinary @{firmware_file} 0x{address:x}")
        }
    });
    if let Some(entry_point) = target.firmware.entry_point {
        lines.push(format!("{} PC 0x{entry_point:x}", target.cpu));
    }

    emit_samples(&mut lines, sources, times[0]);
    for pair in times.windows(2) {
        let delta = pair[1] - pair[0];
        lines.push(format!("emulation RunFor \"{delta:.17}\""));
        emit_samples(&mut lines, sources, pair[1]);
    }
    lines.push("quit".to_owned());
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn emit_samples(lines: &mut Vec<String>, sources: &[GpioSource], time: f64) {
    for (index, source) in sources.iter().enumerate() {
        lines.push(format!("log \"{SAMPLE_MARKER}{index}|{time:.17}\""));
        lines.push(format!("{} State", source.object));
    }
}

fn parse_samples(log: &str, source_count: usize) -> Result<Vec<Vec<(f64, bool)>>, EngineError> {
    let mut samples = vec![Vec::new(); source_count];
    let mut pending = None::<(usize, f64)>;
    for line in log.lines() {
        if let Some(offset) = line.find(SAMPLE_MARKER) {
            let marker = &line[offset + SAMPLE_MARKER.len()..];
            let marker = marker.trim().trim_matches('"');
            let mut fields = marker.split('|');
            let source_index = fields
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| {
                    EngineError::new("renode_output_invalid", "invalid GPIO sample source marker")
                })?;
            let time = fields
                .next()
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.trim_matches('"').parse::<f64>().ok())
                .ok_or_else(|| {
                    EngineError::new("renode_output_invalid", "invalid GPIO sample time marker")
                })?;
            if source_index >= source_count || !time.is_finite() || time < 0.0 {
                return Err(EngineError::new(
                    "renode_output_invalid",
                    "Renode GPIO sample marker is outside the compiled source/time range",
                ));
            }
            pending = Some((source_index, time));
            continue;
        }
        let Some((source_index, time)) = pending else {
            continue;
        };
        if let Some(state) = parse_state_line(line) {
            samples[source_index].push((time, state));
            pending = None;
        }
    }
    if pending.is_some() || samples.iter().any(Vec::is_empty) {
        return Err(EngineError::new(
            "renode_output_invalid",
            format!(
                "Renode output did not contain a complete GPIO sample stream: {}",
                bounded_log(log)
            ),
        ));
    }
    Ok(samples)
}

fn parse_state_line(line: &str) -> Option<bool> {
    let token = line
        .split_whitespace()
        .last()?
        .trim_matches(|character: char| !character.is_ascii_alphabetic());
    match token {
        "True" | "true" => Some(true),
        "False" | "false" => Some(false),
        _ => None,
    }
}

fn transitions_from_samples(samples: &[(f64, bool)]) -> Vec<DigitalTransition> {
    let mut transitions = Vec::new();
    let mut previous = None;
    for &(time, state) in samples {
        if previous == Some(state) {
            continue;
        }
        transitions.push(DigitalTransition {
            time,
            value: if state {
                LogicValue::One
            } else {
                LogicValue::Zero
            },
        });
        previous = Some(state);
    }
    transitions
}

fn parse_gpio_mapping(value: &str) -> Result<(String, u32), &'static str> {
    let Some((port, pin)) = value.trim().split_once('@') else {
        return Err("expected `renodeGpioPeripheral@pin`, for example `gpioPortD@12`");
    };
    if !is_safe_identifier(port) {
        return Err("GPIO peripheral must be a safe Renode identifier");
    }
    let pin = pin
        .parse::<u32>()
        .map_err(|_| "GPIO pin must be an unsigned integer")?;
    Ok((port.to_owned(), pin))
}

fn validate_time(step: f64, stop: f64) -> Result<(), EngineError> {
    if !step.is_finite() || step <= 0.0 {
        return Err(EngineError::new(
            "renode_time_invalid",
            "FirmwareTransient step must be finite and greater than zero",
        ));
    }
    if !stop.is_finite() || stop <= 0.0 {
        return Err(EngineError::new(
            "renode_time_invalid",
            "FirmwareTransient stop must be finite and greater than zero",
        ));
    }
    Ok(())
}

fn is_safe_platform_path(value: &str) -> bool {
    let path = value.trim();
    !path.is_empty()
        && !path.starts_with('/')
        && path.ends_with(".repl")
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        && path.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/')
        })
}

fn is_safe_object_path(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(is_safe_identifier)
}

fn is_safe_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn wait_for_renode_child(
    child: &mut std::process::Child,
    run_dir: &Path,
    control: &ExecutionControl,
    started: &Instant,
) -> Result<std::process::ExitStatus, EngineError> {
    let poll_interval = Duration::from_millis(control.policy.poll_interval_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                return Err(EngineError::new(
                    "renode_wait_failed",
                    format!("could not query Renode process status: {error}"),
                ));
            }
        }
        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                "Renode execution was cancelled",
            ));
        }
        if control.policy.timeout_ms != 0
            && started.elapsed() >= Duration::from_millis(control.policy.timeout_ms)
        {
            return Err(EngineError::new(
                "execution_timeout",
                format!(
                    "Renode execution exceeded timeout of {} ms",
                    control.policy.timeout_ms
                ),
            )
            .retryable(true));
        }
        match fs::metadata(run_dir.join(LOG_FILE)) {
            Ok(metadata) => {
                enforce_buffer_limit("Renode log", metadata.len(), control.policy.max_log_bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(EngineError::new(
                    "renode_resource_inspection_failed",
                    format!("could not inspect Renode log: {error}"),
                ));
            }
        }
        thread::sleep(poll_interval);
    }
}

fn component_error(component: &Component, message: impl Into<String>) -> EngineError {
    EngineError::new(
        "renode_component_invalid",
        format!("component `{}`: {}", component.id.as_str(), message.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Circuit, Net, NetEndpoint, Pin};

    fn mcu_component() -> Component {
        Component::new("mcu", ComponentKind::mcu()).with_pin(Pin::new(
            "pd13",
            "gpioPortD@13",
            SignalDomain::Digital,
            PinDirection::Output,
        ))
    }

    fn request() -> SimulationRequest {
        SimulationRequest {
            circuit: Circuit::new()
                .with_component(mcu_component())
                .with_component(
                    Component::new("led", ComponentKind::logic_output()).with_pin(Pin::new(
                        "in",
                        "in",
                        SignalDomain::Digital,
                        PinDirection::Input,
                    )),
                )
                .with_net(
                    Net::new("gpio")
                        .connect(NetEndpoint::new("mcu", "pd13"))
                        .connect(NetEndpoint::new("led", "in")),
                ),
            analysis: Analysis::FirmwareTransient {
                step: 1e-3,
                stop: 3e-3,
            },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("led", "in"),
                alias: Some("gpio".into()),
            }],
        }
    }

    fn target() -> RenodeTarget {
        RenodeTarget::new(
            "mcu",
            "platforms/boards/stm32f4_discovery-kit.repl",
            FirmwareArtifact::elf(vec![0x7f, b'E', b'L', b'F']),
        )
    }

    #[test]
    fn compiler_uses_renode_platform_loader_and_gpio_bridge() {
        let compiled = compile_request(&request(), &target()).unwrap();
        assert!(compiled.script.contains(
            "machine LoadPlatformDescription @platforms/boards/stm32f4_discovery-kit.repl"
        ));
        assert!(compiled.script.contains(
            "gpioPortD: { 13 -> oxsim_gpio_0@0 }; oxsim_gpio_0: Miscellaneous.LED @ gpioPortD 13"
        ));
        assert!(compiled.script.contains("sysbus LoadELF @firmware.elf"));
        assert!(
            compiled
                .script
                .contains("emulation RunFor \"0.00100000000000000\"")
        );
        assert!(compiled.script.contains("gpioPortD.oxsim_gpio_0 State"));
    }

    #[test]
    fn compiler_reuses_platform_gpio_observer_when_configured() {
        let target = target().with_gpio_observer("gpioPortD@13", "sysbus.gpioPortD.Observer");
        let compiled = compile_request(&request(), &target).unwrap();
        assert!(!compiled.script.contains("oxsim_gpio_0: Miscellaneous.LED"));
        assert!(compiled.script.contains("sysbus.gpioPortD.Observer State"));
    }

    #[test]
    fn renode_sample_parser_normalizes_gpio_transitions() {
        let log = r#"
12:00 [INFO] Script: OXSIM_GPIO|0|0.00000000000000000
False
12:00 [INFO] Script: OXSIM_GPIO|0|0.00100000000000000
True
12:00 [INFO] Script: OXSIM_GPIO|0|0.00200000000000000
True
12:00 [INFO] Script: OXSIM_GPIO|0|0.00300000000000000
False
"#;
        let samples = parse_samples(log, 1).unwrap();
        let transitions = transitions_from_samples(&samples[0]);
        assert_eq!(transitions.len(), 3);
        assert_eq!(transitions[0].value, LogicValue::Zero);
        assert_eq!(transitions[1].value, LogicValue::One);
        assert_eq!(transitions[2].value, LogicValue::Zero);
        assert_eq!(transitions[2].time, 3e-3);
    }

    #[test]
    fn target_rejects_script_injection_paths() {
        let bad = RenodeTarget::new(
            "mcu",
            "../escape.repl\nquit",
            FirmwareArtifact::elf(vec![1]),
        );
        assert_eq!(bad.validate().unwrap_err().code(), "renode_target_invalid");
    }
}

// R9.3 -----------------------------------------------------------------------
//
// Renode's External Control client is kept behind a small helper process. The
// upstream client owns a fatal-error path that may terminate its host process;
// isolation here turns that into a bounded participant failure instead of
// allowing a backend transport failure to terminate the simulation service.

pub const RENODE_COSIM_DEFAULT_HELPER: &str = "ontologyx-renode-cosim-helper";
pub const RENODE_COSIM_TIME_QUANTUM_PS: u64 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenodeCoSimulationBinding {
    pub port: String,
    pub controller: String,
    pub pin: i32,
    pub direction: PinDirection,
}

impl RenodeCoSimulationBinding {
    pub fn input(port: impl Into<String>, controller: impl Into<String>, pin: i32) -> Self {
        Self {
            port: port.into(),
            controller: controller.into(),
            pin,
            direction: PinDirection::Input,
        }
    }

    pub fn output(port: impl Into<String>, controller: impl Into<String>, pin: i32) -> Self {
        Self {
            port: port.into(),
            controller: controller.into(),
            pin,
            direction: PinDirection::Output,
        }
    }

    pub fn bidirectional(port: impl Into<String>, controller: impl Into<String>, pin: i32) -> Self {
        Self {
            port: port.into(),
            controller: controller.into(),
            pin,
            direction: PinDirection::Bidirectional,
        }
    }

    fn validate(&self) -> Result<(), EngineError> {
        if !is_safe_identifier(&self.port) {
            return Err(EngineError::new(
                "renode_cosim_binding_invalid",
                format!(
                    "Renode co-simulation port `{}` is not a safe identifier",
                    self.port
                ),
            ));
        }
        if !is_safe_object_path(&self.controller) {
            return Err(EngineError::new(
                "renode_cosim_binding_invalid",
                format!(
                    "Renode GPIO controller `{}` must contain only identifier segments separated by dots",
                    self.controller
                ),
            ));
        }
        if self.pin < 0 {
            return Err(EngineError::new(
                "renode_cosim_binding_invalid",
                format!("Renode GPIO pin for `{}` must be non-negative", self.port),
            ));
        }
        if !matches!(
            self.direction,
            PinDirection::Input | PinDirection::Output | PinDirection::Bidirectional
        ) {
            return Err(EngineError::new(
                "renode_cosim_binding_invalid",
                format!(
                    "Renode GPIO binding `{}` must be input, output, or bidirectional",
                    self.port
                ),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenodeCoSimulationConfig {
    pub helper: PathBuf,
    pub server_port: String,
    pub machine: String,
    pub quantum: crate::CoSimulationTime,
}

impl RenodeCoSimulationConfig {
    pub fn new(server_port: impl Into<String>, machine: impl Into<String>) -> Self {
        Self {
            helper: PathBuf::from(RENODE_COSIM_DEFAULT_HELPER),
            server_port: server_port.into(),
            machine: machine.into(),
            quantum: crate::CoSimulationTime::from_picoseconds(RENODE_COSIM_TIME_QUANTUM_PS),
        }
    }

    pub fn with_helper(mut self, helper: impl Into<PathBuf>) -> Self {
        self.helper = helper.into();
        self
    }

    pub fn with_quantum(mut self, quantum: crate::CoSimulationTime) -> Self {
        self.quantum = quantum;
        self
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        if self.helper.as_os_str().is_empty() {
            return Err(EngineError::new(
                "renode_cosim_config_invalid",
                "Renode co-simulation helper path must not be empty",
            ));
        }
        if self.server_port.is_empty()
            || !self
                .server_port
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            return Err(EngineError::new(
                "renode_cosim_config_invalid",
                "Renode External Control server port must contain only decimal digits",
            ));
        }
        if self
            .server_port
            .parse::<u16>()
            .ok()
            .is_none_or(|port| port == 0)
        {
            return Err(EngineError::new(
                "renode_cosim_config_invalid",
                "Renode External Control server port must be in the range 1..=65535",
            ));
        }
        if !is_safe_identifier(&self.machine) {
            return Err(EngineError::new(
                "renode_cosim_config_invalid",
                "Renode machine name must be a safe identifier",
            ));
        }
        let quantum = self.quantum.as_picoseconds();
        if quantum == 0 || quantum % RENODE_COSIM_TIME_QUANTUM_PS != 0 {
            return Err(EngineError::new(
                "renode_cosim_time_resolution",
                "Renode co-simulation quantum must be a non-zero whole number of nanoseconds",
            ));
        }
        Ok(())
    }
}

pub struct RenodeCoSimulationParticipant {
    id: String,
    config: RenodeCoSimulationConfig,
    bindings: Vec<RenodeCoSimulationBinding>,
    ports: Vec<crate::CoSimulationPort>,
    session: Option<RenodeCoSimulationSession>,
}

impl RenodeCoSimulationParticipant {
    pub fn new(
        id: impl Into<String>,
        config: RenodeCoSimulationConfig,
        bindings: Vec<RenodeCoSimulationBinding>,
    ) -> Result<Self, EngineError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(EngineError::new(
                "renode_cosim_participant_invalid",
                "Renode co-simulation participant id must not be empty",
            ));
        }
        config.validate()?;
        if bindings.is_empty() {
            return Err(EngineError::new(
                "renode_cosim_binding_invalid",
                "Renode co-simulation requires at least one GPIO binding",
            ));
        }
        let mut names = BTreeSet::new();
        for binding in &bindings {
            binding.validate()?;
            if !names.insert(binding.port.clone()) {
                return Err(EngineError::new(
                    "renode_cosim_binding_invalid",
                    format!("duplicate Renode co-simulation port `{}`", binding.port),
                ));
            }
        }
        let ports = bindings
            .iter()
            .map(|binding| crate::CoSimulationPort::digital(&binding.port, binding.direction))
            .collect();
        Ok(Self {
            id,
            config,
            bindings,
            ports,
            session: None,
        })
    }

    fn binding(&self, port: &str) -> Result<(usize, &RenodeCoSimulationBinding), EngineError> {
        self.bindings
            .iter()
            .enumerate()
            .find(|(_, binding)| binding.port == port)
            .ok_or_else(|| {
                EngineError::new(
                    "renode_cosim_port_missing",
                    format!(
                        "Renode co-simulation participant `{}` has no port `{port}`",
                        self.id
                    ),
                )
            })
    }

    fn session_ref(&self) -> Result<&RenodeCoSimulationSession, EngineError> {
        self.session.as_ref().ok_or_else(|| {
            EngineError::new(
                "renode_cosim_not_initialized",
                format!(
                    "Renode co-simulation participant `{}` is not initialized",
                    self.id
                ),
            )
        })
    }

    fn session_mut(&mut self) -> Result<&mut RenodeCoSimulationSession, EngineError> {
        let id = self.id.clone();
        self.session.as_mut().ok_or_else(|| {
            EngineError::new(
                "renode_cosim_not_initialized",
                format!("Renode co-simulation participant `{id}` is not initialized"),
            )
        })
    }
}

impl crate::CoSimulationParticipant for RenodeCoSimulationParticipant {
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
        check_renode_cosim_control(control)?;
        self.config.validate()?;
        self.session = Some(RenodeCoSimulationSession::spawn(
            &self.config,
            &self.bindings,
            control,
        )?);
        if self.current_time() != crate::CoSimulationTime::ZERO {
            self.session = None;
            return Err(EngineError::new(
                "renode_cosim_time_origin",
                "Renode External Control session must be connected before virtual time advances",
            ));
        }
        Ok(())
    }

    fn next_event_time(&self) -> Option<crate::CoSimulationTime> {
        self.session.as_ref().and_then(|session| {
            session
                .current_time
                .as_picoseconds()
                .checked_add(self.config.quantum.as_picoseconds())
                .map(crate::CoSimulationTime::from_picoseconds)
        })
    }

    fn advance_to(
        &mut self,
        target: crate::CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        if target.as_picoseconds() % RENODE_COSIM_TIME_QUANTUM_PS != 0 {
            return Err(EngineError::new(
                "renode_cosim_time_resolution",
                format!(
                    "Renode target {} ps is not representable at the 1 ns External Control boundary",
                    target.as_picoseconds()
                ),
            ));
        }
        self.session_mut()?.advance_to(target, control)
    }

    fn read_output(&self, port: &str) -> Result<crate::CoSimulationValue, EngineError> {
        let (index, binding) = self.binding(port)?;
        if !matches!(
            binding.direction,
            PinDirection::Output | PinDirection::Bidirectional
        ) {
            return Err(EngineError::new(
                "renode_cosim_port_direction",
                format!("Renode co-simulation port `{port}` is not readable"),
            ));
        }
        let value = self
            .session_ref()?
            .values
            .get(index)
            .copied()
            .ok_or_else(|| EngineError::new("renode_cosim_protocol_error", "missing GPIO state"))?;
        Ok(crate::CoSimulationValue::Digital(value))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: crate::CoSimulationTime,
        value: &crate::CoSimulationValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        let (index, binding) = self.binding(port)?;
        if !matches!(
            binding.direction,
            PinDirection::Input | PinDirection::Bidirectional
        ) {
            return Err(EngineError::new(
                "renode_cosim_port_direction",
                format!("Renode co-simulation port `{port}` is not writable"),
            ));
        }
        let crate::CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new(
                "renode_cosim_domain_mismatch",
                format!("Renode co-simulation port `{port}` requires a digital value"),
            ));
        };
        if !value.is_known() {
            return Err(EngineError::new(
                "renode_cosim_two_state_input",
                format!("Renode GPIO input `{port}` requires zero/one"),
            ));
        }
        let value = *value;
        self.session_mut()?.set_input(index, time, value, control)
    }
}

struct RenodeCoSimulationSession {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<Result<String, String>>,
    current_time: crate::CoSimulationTime,
    values: Vec<LogicValue>,
}

impl RenodeCoSimulationSession {
    fn spawn(
        config: &RenodeCoSimulationConfig,
        bindings: &[RenodeCoSimulationBinding],
        control: &ExecutionControl,
    ) -> Result<Self, EngineError> {
        check_renode_cosim_control(control)?;
        let mut command = Command::new(&config.helper);
        command
            .arg("--server-port")
            .arg(&config.server_port)
            .arg("--machine")
            .arg(&config.machine)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for binding in bindings {
            command.arg("--binding").arg(format!(
                "{},{},{}",
                binding.controller,
                binding.pin,
                match binding.direction {
                    PinDirection::Input => "in",
                    PinDirection::Output => "out",
                    PinDirection::Bidirectional => "io",
                    PinDirection::Passive => unreachable!("binding direction was validated"),
                }
            ));
        }
        let mut child = spawn_renode_cosim_helper(&mut command, config, control)?;
        let stdin = child.stdin.take().ok_or_else(|| {
            EngineError::new(
                "renode_cosim_protocol_error",
                "Renode helper has no stdin pipe",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            EngineError::new(
                "renode_cosim_protocol_error",
                "Renode helper has no stdout pipe",
            )
        })?;
        let mut session = Self {
            child,
            stdin,
            stdout: spawn_cosim_protocol_reader(stdout),
            current_time: crate::CoSimulationTime::ZERO,
            values: vec![LogicValue::Zero; bindings.len()],
        };
        session.read_state(bindings.len(), control)?;
        Ok(session)
    }

    fn set_input(
        &mut self,
        index: usize,
        time: crate::CoSimulationTime,
        value: LogicValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        if time != self.current_time {
            return Err(EngineError::new(
                "renode_cosim_time_invalid",
                "Renode co-simulation input writes must occur at the participant current time",
            ));
        }
        if self.values.get(index).copied() == Some(value) {
            return Ok(false);
        }
        let bit = if value == LogicValue::One { 1 } else { 0 };
        self.send_command(&format!("SET {index} {bit}"), control)?;
        Ok(true)
    }

    fn advance_to(
        &mut self,
        target: crate::CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        if target < self.current_time {
            return Err(EngineError::new(
                "renode_cosim_time_invalid",
                "Renode co-simulation participant cannot move backwards in time",
            ));
        }
        if target == self.current_time {
            return Ok(());
        }
        self.send_command(&format!("ADV {}", target.as_picoseconds()), control)?;
        if self.current_time != target {
            return Err(EngineError::new(
                "renode_cosim_protocol_error",
                format!(
                    "Renode helper advanced to {} ps instead of {} ps",
                    self.current_time.as_picoseconds(),
                    target.as_picoseconds()
                ),
            ));
        }
        Ok(())
    }

    fn send_command(
        &mut self,
        command: &str,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        check_renode_cosim_control(control)?;
        writeln!(self.stdin, "{command}")
            .and_then(|_| self.stdin.flush())
            .map_err(|error| {
                EngineError::new(
                    "renode_cosim_protocol_error",
                    format!("could not write to Renode helper: {error}"),
                )
            })?;
        let count = self.values.len();
        self.read_state(count, control)
    }

    fn read_state(
        &mut self,
        value_count: usize,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        check_renode_cosim_control(control)?;
        let Some(line) = recv_cosim_protocol_line(&self.stdout, control, "Renode")? else {
            let status = self.child.try_wait().ok().flatten();
            return Err(EngineError::new(
                "renode_cosim_helper_exited",
                format!(
                    "Renode co-simulation helper closed its protocol stream (status: {status:?})"
                ),
            ));
        };
        let mut fields = line.split_whitespace();
        if fields.next() != Some("STATE") {
            return Err(EngineError::new(
                "renode_cosim_protocol_error",
                format!("expected `STATE` from Renode helper, got `{}`", line.trim()),
            ));
        }
        let time = fields
            .next()
            .and_then(|field| field.parse::<u64>().ok())
            .ok_or_else(|| {
                EngineError::new("renode_cosim_protocol_error", "invalid Renode helper time")
            })?;
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            let value = match fields.next() {
                Some("0") => LogicValue::Zero,
                Some("1") => LogicValue::One,
                _ => {
                    return Err(EngineError::new(
                        "renode_cosim_protocol_error",
                        format!("invalid Renode helper GPIO state `{}`", line.trim()),
                    ));
                }
            };
            values.push(value);
        }
        if fields.next().is_some() {
            return Err(EngineError::new(
                "renode_cosim_protocol_error",
                format!(
                    "Renode helper returned extra state fields: `{}`",
                    line.trim()
                ),
            ));
        }
        self.current_time = crate::CoSimulationTime::from_picoseconds(time);
        self.values = values;
        Ok(())
    }
}

fn spawn_renode_cosim_helper(
    command: &mut Command,
    config: &RenodeCoSimulationConfig,
    control: &ExecutionControl,
) -> Result<Child, EngineError> {
    check_renode_cosim_control(control)?;
    let started = Instant::now();
    let poll = Duration::from_millis(control.policy.poll_interval_ms);

    loop {
        check_renode_cosim_control(control)?;
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if renode_cosim_spawn_error_is_transient(&error) => {
                if control.policy.timeout_ms == 0 {
                    thread::sleep(poll);
                    continue;
                }

                let limit = Duration::from_millis(control.policy.timeout_ms);
                let elapsed = started.elapsed();
                if elapsed >= limit {
                    return Err(EngineError::new(
                        "execution_timeout",
                        format!(
                            "Renode co-simulation helper could not start within {} ms: {error}",
                            control.policy.timeout_ms
                        ),
                    ));
                }
                thread::sleep(poll.min(limit.saturating_sub(elapsed)));
            }
            Err(error) => {
                return Err(EngineError::new(
                    if error.kind() == io::ErrorKind::NotFound {
                        "renode_cosim_helper_not_found"
                    } else {
                        "renode_cosim_helper_spawn_failed"
                    },
                    format!(
                        "could not execute Renode co-simulation helper `{}`: {error}",
                        config.helper.display()
                    ),
                ));
            }
        }
    }
}

fn renode_cosim_spawn_error_is_transient(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(26) {
        return true;
    }
    false
}

fn spawn_cosim_protocol_reader(stdout: ChildStdout) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    receiver
}

fn recv_cosim_protocol_line(
    receiver: &Receiver<Result<String, String>>,
    control: &ExecutionControl,
    backend: &str,
) -> Result<Option<String>, EngineError> {
    control.policy.validate()?;
    let started = Instant::now();
    let poll = Duration::from_millis(control.policy.poll_interval_ms);
    loop {
        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                format!("{backend} co-simulation was cancelled while waiting for backend state"),
            ));
        }
        let wait = if control.policy.timeout_ms == 0 {
            poll
        } else {
            let limit = Duration::from_millis(control.policy.timeout_ms);
            if started.elapsed() >= limit {
                return Err(EngineError::new(
                    "execution_timeout",
                    format!(
                        "{backend} co-simulation backend did not respond within {} ms",
                        control.policy.timeout_ms
                    ),
                ));
            }
            poll.min(limit.saturating_sub(started.elapsed()))
        };
        match receiver.recv_timeout(wait) {
            Ok(Ok(line)) => return Ok(Some(line)),
            Ok(Err(error)) => {
                return Err(EngineError::new(
                    if backend == "Renode" {
                        "renode_cosim_protocol_error"
                    } else {
                        "ngspice_cosim_protocol_error"
                    },
                    format!("could not read {backend} co-simulation helper state: {error}"),
                ));
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(None),
        }
    }
}

impl Drop for RenodeCoSimulationSession {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "QUIT");
        let _ = self.stdin.flush();
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

fn check_renode_cosim_control(control: &ExecutionControl) -> Result<(), EngineError> {
    control.policy.validate()?;
    if control.cancellation.is_cancelled() {
        return Err(EngineError::new(
            "execution_cancelled",
            "Renode co-simulation was cancelled",
        ));
    }
    Ok(())
}
