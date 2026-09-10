use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::ngspice::{
    ChildGuard, TempRunDir, bounded_log, enforce_buffer_limit, read_text_limited,
};
use crate::xspice::{
    CompiledProbe, duty_cycle, optional_logic_value, optional_non_negative_seconds, parse_vcd,
    required_logic_value,
};
use crate::{
    Analysis, AnalysisKind, Component, ComponentKind, Diagnostic, DiagnosticLevel,
    EngineCapabilities, EngineError, EngineId, ExecutionControl, LogicValue, ModelDefinition,
    ModelKind, ModelLanguage, ParameterValue, PinDirection, SignalDomain, SimulationEngine,
    SimulationRequest, SimulationResult,
};

pub const VERILATOR_ENGINE_ID: &str = "verilator";
const WRAPPER_FILE: &str = "oxsim_tb.sv";
const BUILD_DIR: &str = "obj_dir";
const BINARY_FILE: &str = "oxsim_verilated";
const BUILD_LOG_FILE: &str = "verilator-build.log";
const RUN_LOG_FILE: &str = "verilator-run.log";
const VCD_FILE: &str = "result.vcd";
const TESTBENCH_MODULE: &str = "oxsim_tb";
const PICOSECONDS_PER_SECOND: f64 = 1_000_000_000_000.0;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerilatorInfo {
    pub available: bool,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VerilatorEngine {
    executable: PathBuf,
}

impl Default for VerilatorEngine {
    fn default() -> Self {
        Self::new("verilator")
    }
}

impl VerilatorEngine {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn info(&self) -> VerilatorInfo {
        match Command::new(&self.executable).arg("--version").output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                let version = if stdout.is_empty() {
                    (!stderr.is_empty()).then_some(stderr)
                } else {
                    Some(stdout)
                };
                VerilatorInfo {
                    available: true,
                    executable: self.executable.display().to_string(),
                    version,
                }
            }
            _ => VerilatorInfo {
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
                "simulation was cancelled before Verilator execution",
            ));
        }

        let compiled = compile_request(request)?;
        let input_bytes = compiled.wrapper.len() as u64
            + compiled
                .sources
                .iter()
                .map(|source| source.source.len() as u64)
                .sum::<u64>();
        enforce_buffer_limit(
            "Verilator HDL input",
            input_bytes,
            control.policy.max_input_bytes,
        )?;

        let run_dir = TempRunDir::create().map_err(|error| {
            EngineError::new(
                "verilator_tempdir_failed",
                format!("could not create Verilator run directory: {error}"),
            )
        })?;
        for source in &compiled.sources {
            fs::write(
                run_dir.path().join(&source.filename),
                source.source.as_bytes(),
            )
            .map_err(|error| {
                EngineError::new(
                    "verilator_source_write_failed",
                    format!("could not write generated HDL source: {error}"),
                )
            })?;
        }
        fs::write(
            run_dir.path().join(WRAPPER_FILE),
            compiled.wrapper.as_bytes(),
        )
        .map_err(|error| {
            EngineError::new(
                "verilator_wrapper_write_failed",
                format!("could not write generated Verilator wrapper: {error}"),
            )
        })?;

        let started = Instant::now();
        let build_log = fs::File::create(run_dir.path().join(BUILD_LOG_FILE)).map_err(|error| {
            EngineError::new(
                "verilator_log_create_failed",
                format!("could not create Verilator build log: {error}"),
            )
        })?;
        let build_stderr = build_log.try_clone().map_err(|error| {
            EngineError::new(
                "verilator_log_create_failed",
                format!("could not clone Verilator build log handle: {error}"),
            )
        })?;
        let mut command = Command::new(&self.executable);
        command
            .arg("--binary")
            .arg("--timing")
            .arg("--trace-vcd")
            .arg("--timescale")
            .arg("1ps/1ps")
            .arg("--x-assign")
            .arg("0")
            .arg("--x-initial")
            .arg("0")
            .arg("--top-module")
            .arg(TESTBENCH_MODULE)
            .arg("--Mdir")
            .arg(BUILD_DIR)
            .arg("-o")
            .arg(BINARY_FILE)
            .arg("-j")
            .arg("1")
            .args(
                compiled
                    .sources
                    .iter()
                    .map(|source| source.filename.as_str()),
            )
            .arg(WRAPPER_FILE)
            .current_dir(run_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(build_log))
            .stderr(Stdio::from(build_stderr));
        let child = command.spawn().map_err(|error| {
            let code = if error.kind() == io::ErrorKind::NotFound {
                "verilator_not_found"
            } else {
                "verilator_spawn_failed"
            };
            EngineError::new(
                code,
                format!("could not execute `{}`: {error}", self.executable.display()),
            )
        })?;
        let mut child = ChildGuard::new(child);
        let build_status = wait_for_verilator_child(
            child.child_mut(),
            run_dir.path(),
            control,
            &started,
            BUILD_LOG_FILE,
            None,
            "build",
        )?;
        child.disarm();
        let build_output = read_optional_log(
            &run_dir.path().join(BUILD_LOG_FILE),
            control.policy.max_log_bytes,
            "verilator_build_log_read_failed",
        )?;
        if !build_status.success() {
            return Err(EngineError::new(
                "verilator_build_failed",
                format!(
                    "Verilator exited with {build_status}: {}",
                    bounded_log(&build_output)
                ),
            ));
        }

        let binary = locate_binary(run_dir.path()).ok_or_else(|| {
            EngineError::new(
                "verilator_binary_missing",
                "Verilator build succeeded without producing the expected simulation binary",
            )
        })?;
        let run_log = fs::File::create(run_dir.path().join(RUN_LOG_FILE)).map_err(|error| {
            EngineError::new(
                "verilator_log_create_failed",
                format!("could not create Verilator runtime log: {error}"),
            )
        })?;
        let run_stderr = run_log.try_clone().map_err(|error| {
            EngineError::new(
                "verilator_log_create_failed",
                format!("could not clone Verilator runtime log handle: {error}"),
            )
        })?;
        let child = Command::new(&binary)
            .arg("+verilator+seed+1")
            .current_dir(run_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(run_log))
            .stderr(Stdio::from(run_stderr))
            .spawn()
            .map_err(|error| {
                EngineError::new(
                    "verilator_runtime_spawn_failed",
                    format!("could not execute generated Verilator binary: {error}"),
                )
            })?;
        let mut child = ChildGuard::new(child);
        let run_status = wait_for_verilator_child(
            child.child_mut(),
            run_dir.path(),
            control,
            &started,
            RUN_LOG_FILE,
            Some(VCD_FILE),
            "runtime",
        )?;
        child.disarm();
        let run_output = read_optional_log(
            &run_dir.path().join(RUN_LOG_FILE),
            control.policy.max_log_bytes,
            "verilator_runtime_log_read_failed",
        )?;
        if !run_status.success() {
            return Err(EngineError::new(
                "verilator_runtime_failed",
                format!(
                    "generated Verilator simulation exited with {run_status}: {}",
                    bounded_log(&run_output)
                ),
            ));
        }

        let vcd = read_text_limited(
            &run_dir.path().join(VCD_FILE),
            control.policy.max_output_bytes,
            "result",
            "verilator_vcd_read_failed",
        )?;
        let waveforms = parse_vcd(&vcd, &compiled.probes)?;
        let mut diagnostics = vec![Diagnostic {
            level: DiagnosticLevel::Info,
            code: "verilator_two_state_semantics".to_owned(),
            message: "Verilator is mostly a two-state simulator; R5 fixes X assignment/initialization to zero for deterministic RTL execution".to_owned(),
        }];
        for warning in extract_verilator_warnings(&build_output)
            .into_iter()
            .chain(extract_verilator_warnings(&run_output))
            .take(16)
        {
            diagnostics.push(Diagnostic {
                level: DiagnosticLevel::Warning,
                code: "verilator_warning".to_owned(),
                message: warning,
            });
        }

        let info = self.info();
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

impl SimulationEngine for VerilatorEngine {
    fn id(&self) -> EngineId {
        EngineId::new(VERILATOR_ENGINE_ID)
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

    fn supports_request(&self, request: &SimulationRequest) -> bool {
        request.analysis.kind() == AnalysisKind::DigitalTransient
            && request
                .circuit
                .components
                .iter()
                .any(|component| component.kind == ComponentKind::hdl_module())
            && request
                .circuit
                .components
                .iter()
                .all(|component| is_supported_component(&component.kind))
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

const COSIM_SOURCE_FILE_VERILOG: &str = "oxsim_cosim_model.v";
const COSIM_SOURCE_FILE_SYSTEMVERILOG: &str = "oxsim_cosim_model.sv";
const COSIM_HARNESS_FILE: &str = "oxsim_cosim_main.cpp";
const COSIM_BUILD_DIR: &str = "obj_dir_cosim";
const COSIM_BINARY_FILE: &str = "oxsim_verilator_cosim";
const COSIM_MODEL_PREFIX: &str = "OxCosimModel";
const COSIM_BUILD_LOG_FILE: &str = "verilator-cosim-build.log";

pub struct VerilatorCoSimulationParticipant {
    id: String,
    executable: PathBuf,
    model: ModelDefinition,
    ports: Vec<crate::CoSimulationPort>,
    session: Option<VerilatorCoSimulationSession>,
}

impl VerilatorCoSimulationParticipant {
    pub fn new(
        id: impl Into<String>,
        model: ModelDefinition,
        ports: Vec<crate::CoSimulationPort>,
    ) -> Result<Self, EngineError> {
        Self::with_executable(id, "verilator", model, ports)
    }

    pub fn with_executable(
        id: impl Into<String>,
        executable: impl Into<PathBuf>,
        model: ModelDefinition,
        ports: Vec<crate::CoSimulationPort>,
    ) -> Result<Self, EngineError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(EngineError::new(
                "verilator_cosim_participant_invalid",
                "Verilator co-simulation participant id must not be empty",
            ));
        }
        if model.kind != ModelKind::Module
            || !matches!(
                model.language,
                ModelLanguage::Verilog | ModelLanguage::SystemVerilog
            )
        {
            return Err(EngineError::new(
                "verilator_cosim_model_invalid",
                "Verilator co-simulation requires an inline Verilog/SystemVerilog module model",
            ));
        }
        if !is_safe_identifier(&model.entry) {
            return Err(EngineError::new(
                "verilator_cosim_model_invalid",
                format!(
                    "HDL module entry `{}` is not a safe identifier",
                    model.entry
                ),
            ));
        }
        validate_hdl_source(&model)?;
        validate_cosim_ports(&ports)?;
        Ok(Self {
            id,
            executable: executable.into(),
            model,
            ports,
            session: None,
        })
    }
}

impl crate::CoSimulationParticipant for VerilatorCoSimulationParticipant {
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
            return Err(EngineError::new(
                "execution_cancelled",
                "co-simulation was cancelled before Verilator participant initialization",
            ));
        }
        let session = VerilatorCoSimulationSession::build_and_spawn(
            &self.executable,
            &self.model,
            &self.ports,
            control,
        )?;
        self.session = Some(session);
        Ok(())
    }

    fn next_event_time(&self) -> Option<crate::CoSimulationTime> {
        self.session.as_ref().and_then(|session| session.next_event)
    }

    fn advance_to(
        &mut self,
        target: crate::CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        self.session_mut()?.advance_to(target, control)
    }

    fn read_output(&self, port: &str) -> Result<crate::CoSimulationValue, EngineError> {
        let port_index = self
            .ports
            .iter()
            .position(|candidate| candidate.name == port)
            .ok_or_else(|| {
                EngineError::new(
                    "verilator_cosim_port_missing",
                    format!(
                        "Verilator co-simulation participant `{}` has no port `{port}`",
                        self.id
                    ),
                )
            })?;
        if self.ports[port_index].direction != PinDirection::Output {
            return Err(EngineError::new(
                "verilator_cosim_port_direction",
                format!("Verilator co-simulation port `{port}` is not an output"),
            ));
        }
        let value = self
            .session_ref()?
            .outputs
            .get(port)
            .copied()
            .ok_or_else(|| {
                EngineError::new(
                    "verilator_cosim_protocol_error",
                    format!("Verilator co-simulation output `{port}` has no cached value"),
                )
            })?;
        Ok(crate::CoSimulationValue::Digital(value))
    }

    fn write_input(
        &mut self,
        port: &str,
        time: crate::CoSimulationTime,
        value: &crate::CoSimulationValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        let port_index = self
            .ports
            .iter()
            .position(|candidate| candidate.name == port)
            .ok_or_else(|| {
                EngineError::new(
                    "verilator_cosim_port_missing",
                    format!(
                        "Verilator co-simulation participant `{}` has no port `{port}`",
                        self.id
                    ),
                )
            })?;
        if self.ports[port_index].direction != PinDirection::Input {
            return Err(EngineError::new(
                "verilator_cosim_port_direction",
                format!("Verilator co-simulation port `{port}` is not an input"),
            ));
        }
        let crate::CoSimulationValue::Digital(value) = value else {
            return Err(EngineError::new(
                "verilator_cosim_domain_mismatch",
                format!("Verilator co-simulation port `{port}` requires a digital value"),
            ));
        };
        if !value.is_known() {
            return Err(EngineError::new(
                "verilator_cosim_two_state_input",
                format!(
                    "Verilator co-simulation input `{port}` requires zero/one; use the R3 digital participant for X/Z semantics"
                ),
            ));
        }
        self.session_mut()?
            .set_input(port_index, port, time, *value, control)
    }
}

impl VerilatorCoSimulationParticipant {
    fn session_ref(&self) -> Result<&VerilatorCoSimulationSession, EngineError> {
        self.session.as_ref().ok_or_else(|| {
            EngineError::new(
                "verilator_cosim_not_initialized",
                format!(
                    "Verilator co-simulation participant `{}` is not initialized",
                    self.id
                ),
            )
        })
    }

    fn session_mut(&mut self) -> Result<&mut VerilatorCoSimulationSession, EngineError> {
        let id = self.id.clone();
        self.session.as_mut().ok_or_else(|| {
            EngineError::new(
                "verilator_cosim_not_initialized",
                format!("Verilator co-simulation participant `{id}` is not initialized"),
            )
        })
    }
}

struct VerilatorCoSimulationSession {
    _run_dir: TempRunDir,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    current_time: crate::CoSimulationTime,
    next_event: Option<crate::CoSimulationTime>,
    outputs: BTreeMap<String, LogicValue>,
    input_values: BTreeMap<String, LogicValue>,
    output_ports: Vec<String>,
}

impl VerilatorCoSimulationSession {
    fn build_and_spawn(
        executable: &Path,
        model: &ModelDefinition,
        ports: &[crate::CoSimulationPort],
        control: &ExecutionControl,
    ) -> Result<Self, EngineError> {
        let source_file = match model.language {
            ModelLanguage::Verilog => COSIM_SOURCE_FILE_VERILOG,
            ModelLanguage::SystemVerilog => COSIM_SOURCE_FILE_SYSTEMVERILOG,
            _ => unreachable!("validated Verilator co-simulation language"),
        };
        let harness = generate_cosim_harness(ports)?;
        let input_bytes = model.source.len() as u64 + harness.len() as u64;
        enforce_buffer_limit(
            "Verilator co-simulation HDL input",
            input_bytes,
            control.policy.max_input_bytes,
        )?;

        let run_dir = TempRunDir::create().map_err(|error| {
            EngineError::new(
                "verilator_tempdir_failed",
                format!("could not create Verilator co-simulation run directory: {error}"),
            )
        })?;
        fs::write(run_dir.path().join(source_file), model.source.as_bytes()).map_err(|error| {
            EngineError::new(
                "verilator_source_write_failed",
                format!("could not write Verilator co-simulation HDL source: {error}"),
            )
        })?;
        fs::write(run_dir.path().join(COSIM_HARNESS_FILE), harness.as_bytes()).map_err(
            |error| {
                EngineError::new(
                    "verilator_wrapper_write_failed",
                    format!("could not write Verilator co-simulation C++ harness: {error}"),
                )
            },
        )?;

        let build_log =
            fs::File::create(run_dir.path().join(COSIM_BUILD_LOG_FILE)).map_err(|error| {
                EngineError::new(
                    "verilator_log_create_failed",
                    format!("could not create Verilator co-simulation build log: {error}"),
                )
            })?;
        let build_stderr = build_log.try_clone().map_err(|error| {
            EngineError::new(
                "verilator_log_create_failed",
                format!("could not clone Verilator co-simulation build log handle: {error}"),
            )
        })?;
        let started = Instant::now();
        let child = Command::new(executable)
            .arg("--cc")
            .arg("--exe")
            .arg("--build")
            .arg("--timing")
            .arg("--timescale")
            .arg("1ps/1ps")
            .arg("--x-assign")
            .arg("0")
            .arg("--x-initial")
            .arg("0")
            .arg("--top-module")
            .arg(&model.entry)
            .arg("--prefix")
            .arg(COSIM_MODEL_PREFIX)
            .arg("--Mdir")
            .arg(COSIM_BUILD_DIR)
            .arg("-o")
            .arg(COSIM_BINARY_FILE)
            .arg("-j")
            .arg("1")
            .arg(source_file)
            .arg(COSIM_HARNESS_FILE)
            .current_dir(run_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(build_log))
            .stderr(Stdio::from(build_stderr))
            .spawn()
            .map_err(|error| {
                EngineError::new(
                    if error.kind() == io::ErrorKind::NotFound {
                        "verilator_not_found"
                    } else {
                        "verilator_spawn_failed"
                    },
                    format!(
                        "could not execute `{}` for co-simulation: {error}",
                        executable.display()
                    ),
                )
            })?;
        let mut build_child = ChildGuard::new(child);
        let build_status = wait_for_verilator_child(
            build_child.child_mut(),
            run_dir.path(),
            control,
            &started,
            COSIM_BUILD_LOG_FILE,
            None,
            "co-simulation build",
        )?;
        build_child.disarm();
        let build_output = read_optional_log(
            &run_dir.path().join(COSIM_BUILD_LOG_FILE),
            control.policy.max_log_bytes,
            "verilator_cosim_build_log_read_failed",
        )?;
        if !build_status.success() {
            return Err(EngineError::new(
                "verilator_cosim_build_failed",
                format!(
                    "Verilator co-simulation build exited with {build_status}: {}",
                    bounded_log(&build_output)
                ),
            ));
        }

        let binary = run_dir.path().join(COSIM_BUILD_DIR).join(COSIM_BINARY_FILE);
        if !binary.is_file() {
            return Err(EngineError::new(
                "verilator_cosim_binary_missing",
                "Verilator co-simulation build succeeded without producing the expected binary",
            ));
        }
        let mut child = Command::new(&binary)
            .arg("+verilator+seed+1")
            .current_dir(run_dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| {
                EngineError::new(
                    "verilator_cosim_runtime_spawn_failed",
                    format!("could not execute generated Verilator co-simulation binary: {error}"),
                )
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            EngineError::new(
                "verilator_cosim_protocol_error",
                "generated Verilator co-simulation binary has no stdin pipe",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            EngineError::new(
                "verilator_cosim_protocol_error",
                "generated Verilator co-simulation binary has no stdout pipe",
            )
        })?;
        let output_ports = ports
            .iter()
            .filter(|port| port.direction == PinDirection::Output)
            .map(|port| port.name.clone())
            .collect::<Vec<_>>();
        let input_values = ports
            .iter()
            .filter(|port| port.direction == PinDirection::Input)
            .map(|port| (port.name.clone(), LogicValue::Zero))
            .collect::<BTreeMap<_, _>>();
        let mut session = Self {
            _run_dir: run_dir,
            child,
            stdin,
            stdout: BufReader::new(stdout),
            current_time: crate::CoSimulationTime::ZERO,
            next_event: None,
            outputs: BTreeMap::new(),
            input_values,
            output_ports,
        };
        session.read_state(control)?;
        Ok(session)
    }

    fn set_input(
        &mut self,
        port_index: usize,
        port: &str,
        time: crate::CoSimulationTime,
        value: LogicValue,
        control: &ExecutionControl,
    ) -> Result<bool, EngineError> {
        if time != self.current_time {
            return Err(EngineError::new(
                "verilator_cosim_time_invalid",
                "Verilator co-simulation input writes must occur at the participant current time",
            ));
        }
        let previous = self
            .input_values
            .get(port)
            .copied()
            .unwrap_or(LogicValue::Zero);
        if previous == value {
            return Ok(false);
        }
        self.send_command(
            &format!(
                "SET {port_index} {}",
                if value == LogicValue::One { 1 } else { 0 }
            ),
            control,
        )?;
        self.input_values.insert(port.to_owned(), value);
        Ok(true)
    }

    fn advance_to(
        &mut self,
        target: crate::CoSimulationTime,
        control: &ExecutionControl,
    ) -> Result<(), EngineError> {
        if target < self.current_time {
            return Err(EngineError::new(
                "verilator_cosim_time_invalid",
                "Verilator co-simulation participant cannot move backwards in time",
            ));
        }
        if target == self.current_time {
            return Ok(());
        }
        self.send_command(&format!("ADV {}", target.as_picoseconds()), control)?;
        if self.current_time != target {
            return Err(EngineError::new(
                "verilator_cosim_protocol_error",
                format!(
                    "Verilator co-simulation session advanced to {} ps instead of {} ps",
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
        check_cosim_control(control)?;
        writeln!(self.stdin, "{command}")
            .and_then(|_| self.stdin.flush())
            .map_err(|error| {
                EngineError::new(
                    "verilator_cosim_protocol_error",
                    format!("could not write to Verilator co-simulation session: {error}"),
                )
            })?;
        self.read_state(control)
    }

    fn read_state(&mut self, control: &ExecutionControl) -> Result<(), EngineError> {
        check_cosim_control(control)?;
        let mut line = String::new();
        let count = self.stdout.read_line(&mut line).map_err(|error| {
            EngineError::new(
                "verilator_cosim_protocol_error",
                format!("could not read Verilator co-simulation session state: {error}"),
            )
        })?;
        if count == 0 {
            let status = self.child.try_wait().ok().flatten();
            return Err(EngineError::new(
                "verilator_cosim_runtime_failed",
                format!(
                    "Verilator co-simulation session closed unexpectedly{}",
                    status.map_or(String::new(), |status| format!(" with {status}"))
                ),
            ));
        }
        parse_cosim_state_line(
            line.trim(),
            &self.output_ports,
            &mut self.current_time,
            &mut self.next_event,
            &mut self.outputs,
        )
    }
}

impl Drop for VerilatorCoSimulationSession {
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

fn validate_cosim_ports(ports: &[crate::CoSimulationPort]) -> Result<(), EngineError> {
    if ports.is_empty() {
        return Err(EngineError::new(
            "verilator_cosim_port_missing",
            "Verilator co-simulation participant requires at least one scalar digital port",
        ));
    }
    let mut names = BTreeSet::new();
    for port in ports {
        if port.domain != SignalDomain::Digital || port.unit.is_some() {
            return Err(EngineError::new(
                "verilator_cosim_port_invalid",
                format!(
                    "Verilator co-simulation port `{}` must be digital without an analog unit",
                    port.name
                ),
            ));
        }
        if !matches!(port.direction, PinDirection::Input | PinDirection::Output) {
            return Err(EngineError::new(
                "verilator_cosim_port_invalid",
                format!(
                    "Verilator co-simulation port `{}` must be input or output in R9.2",
                    port.name
                ),
            ));
        }
        if !is_safe_identifier(&port.name) {
            return Err(EngineError::new(
                "verilator_cosim_port_invalid",
                format!(
                    "Verilator co-simulation port `{}` must be a scalar HDL identifier in R9.2",
                    port.name
                ),
            ));
        }
        if !names.insert(port.name.clone()) {
            return Err(EngineError::new(
                "verilator_cosim_port_duplicate",
                format!(
                    "Verilator co-simulation port `{}` is declared more than once",
                    port.name
                ),
            ));
        }
    }
    Ok(())
}

fn generate_cosim_harness(ports: &[crate::CoSimulationPort]) -> Result<String, EngineError> {
    validate_cosim_ports(ports)?;
    let inputs = ports
        .iter()
        .enumerate()
        .filter(|(_, port)| port.direction == PinDirection::Input)
        .collect::<Vec<_>>();
    let outputs = ports
        .iter()
        .filter(|port| port.direction == PinDirection::Output)
        .collect::<Vec<_>>();
    let mut lines = vec![
        format!("#include \"{COSIM_MODEL_PREFIX}.h\""),
        "#include \"verilated.h\"".to_owned(),
        "#include <cstdint>".to_owned(),
        "#include <iostream>".to_owned(),
        "#include <memory>".to_owned(),
        "#include <sstream>".to_owned(),
        "#include <string>".to_owned(),
        String::new(),
        format!("static void emit_state(VerilatedContext* contextp, {COSIM_MODEL_PREFIX}* top) {{"),
        "  std::cout << \"STATE \" << contextp->time();".to_owned(),
    ];
    for port in &outputs {
        lines.push(format!(
            "  std::cout << \" {}=\" << static_cast<unsigned>(top->{} & 1U);",
            port.name, port.name
        ));
    }
    lines.extend([
        "  if (top->eventsPending()) std::cout << \" NEXT=\" << top->nextTimeSlot();".to_owned(),
        "  else std::cout << \" NEXT=-\";".to_owned(),
        "  std::cout << std::endl;".to_owned(),
        "}".to_owned(),
        String::new(),
        "int main(int argc, char** argv) {".to_owned(),
        "  auto contextp = std::make_unique<VerilatedContext>();".to_owned(),
        "  contextp->commandArgs(argc, argv);".to_owned(),
        format!("  auto top = std::make_unique<{COSIM_MODEL_PREFIX}>(contextp.get());"),
    ]);
    for (_, port) in &inputs {
        lines.push(format!("  top->{} = 0;", port.name));
    }
    lines.extend([
        "  top->eval();".to_owned(),
        "  emit_state(contextp.get(), top.get());".to_owned(),
        "  std::string line;".to_owned(),
        "  while (std::getline(std::cin, line)) {".to_owned(),
        "    std::istringstream input(line);".to_owned(),
        "    std::string command;".to_owned(),
        "    input >> command;".to_owned(),
        "    if (command == \"QUIT\") break;".to_owned(),
        "    if (command == \"SET\") {".to_owned(),
        "      std::size_t index = 0; unsigned value = 0; input >> index >> value;".to_owned(),
        "      switch (index) {".to_owned(),
    ]);
    for (index, port) in &inputs {
        lines.push(format!(
            "        case {index}: top->{} = value & 1U; break;",
            port.name
        ));
    }
    lines.extend([
        "        default: std::cerr << \"invalid SET port index\" << std::endl; return 2;".to_owned(),
        "      }".to_owned(),
        "      top->eval();".to_owned(),
        "      emit_state(contextp.get(), top.get());".to_owned(),
        "      continue;".to_owned(),
        "    }".to_owned(),
        "    if (command == \"ADV\") {".to_owned(),
        "      std::uint64_t target = 0; input >> target;".to_owned(),
        "      if (target < contextp->time()) { std::cerr << \"time moved backwards\" << std::endl; return 3; }".to_owned(),
        "      std::uint64_t internal_steps = 0;".to_owned(),
        "      while (top->eventsPending() && top->nextTimeSlot() <= target) {".to_owned(),
        "        if (++internal_steps > 1000000ULL) { std::cerr << \"internal event limit exceeded\" << std::endl; return 5; }".to_owned(),
        "        const auto next = top->nextTimeSlot();".to_owned(),
        "        contextp->time(next);".to_owned(),
        "        top->eval();".to_owned(),
        "      }".to_owned(),
        "      if (contextp->time() < target) { contextp->time(target); top->eval(); }".to_owned(),
        "      emit_state(contextp.get(), top.get());".to_owned(),
        "      continue;".to_owned(),
        "    }".to_owned(),
        "    std::cerr << \"unknown command\" << std::endl; return 4;".to_owned(),
        "  }".to_owned(),
        "  top->final();".to_owned(),
        "  return 0;".to_owned(),
        "}".to_owned(),
    ]);
    Ok(lines.join("\n"))
}

fn parse_cosim_state_line(
    line: &str,
    output_ports: &[String],
    current_time: &mut crate::CoSimulationTime,
    next_event: &mut Option<crate::CoSimulationTime>,
    outputs: &mut BTreeMap<String, LogicValue>,
) -> Result<(), EngineError> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("STATE") {
        return Err(EngineError::new(
            "verilator_cosim_protocol_error",
            format!("expected Verilator STATE response, received `{line}`"),
        ));
    }
    let time = fields
        .next()
        .ok_or_else(|| {
            EngineError::new(
                "verilator_cosim_protocol_error",
                "Verilator STATE response is missing time",
            )
        })?
        .parse::<u64>()
        .map_err(|_| {
            EngineError::new(
                "verilator_cosim_protocol_error",
                format!("invalid Verilator STATE time in `{line}`"),
            )
        })?;
    let mut seen_outputs = BTreeSet::new();
    let mut parsed_next = None;
    for field in fields {
        let Some((name, raw)) = field.split_once('=') else {
            continue;
        };
        if name == "NEXT" {
            if raw != "-" {
                parsed_next = Some(crate::CoSimulationTime::from_picoseconds(
                    raw.parse::<u64>().map_err(|_| {
                        EngineError::new(
                            "verilator_cosim_protocol_error",
                            format!("invalid Verilator NEXT time in `{line}`"),
                        )
                    })?,
                ));
            }
            continue;
        }
        if output_ports.iter().any(|port| port == name) {
            let value = match raw {
                "0" => LogicValue::Zero,
                "1" => LogicValue::One,
                _ => {
                    return Err(EngineError::new(
                        "verilator_cosim_protocol_error",
                        format!("invalid two-state Verilator output `{field}`"),
                    ));
                }
            };
            outputs.insert(name.to_owned(), value);
            seen_outputs.insert(name.to_owned());
        }
    }
    if seen_outputs.len() != output_ports.len() {
        return Err(EngineError::new(
            "verilator_cosim_protocol_error",
            format!("Verilator STATE response omitted one or more configured outputs: `{line}`"),
        ));
    }
    *current_time = crate::CoSimulationTime::from_picoseconds(time);
    if parsed_next.is_some_and(|next| next <= *current_time) {
        return Err(EngineError::new(
            "verilator_cosim_protocol_error",
            "Verilator reported a non-future next event",
        ));
    }
    *next_event = parsed_next;
    Ok(())
}

fn check_cosim_control(control: &ExecutionControl) -> Result<(), EngineError> {
    control.policy.validate()?;
    if control.cancellation.is_cancelled() {
        return Err(EngineError::new(
            "execution_cancelled",
            "Verilator co-simulation was cancelled",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct HdlSourceFile {
    filename: String,
    source: String,
}

#[derive(Clone, Debug)]
struct CompiledVerilatorRequest {
    sources: Vec<HdlSourceFile>,
    wrapper: String,
    probes: Vec<CompiledProbe>,
}

fn compile_request(request: &SimulationRequest) -> Result<CompiledVerilatorRequest, EngineError> {
    let Analysis::DigitalTransient { stop } = &request.analysis else {
        return Err(EngineError::new(
            "verilator_unsupported_analysis",
            "the Verilator engine only supports digital_transient analysis",
        ));
    };
    let stop = *stop;
    if !stop.is_finite() || stop <= 0.0 {
        return Err(EngineError::new(
            "verilator_invalid_analysis",
            "digital transient stop time must be finite and greater than zero",
        ));
    }
    if !request
        .circuit
        .components
        .iter()
        .any(|component| component.kind == ComponentKind::hdl_module())
    {
        return Err(EngineError::new(
            "verilator_hdl_module_missing",
            "Verilator execution requires at least one hdl_module component",
        ));
    }
    for component in &request.circuit.components {
        if !is_supported_component(&component.kind) {
            return Err(EngineError::new(
                "verilator_unsupported_component",
                format!(
                    "component `{}` of kind `{}` is not part of the R5 Verilator boundary",
                    component.id.as_str(),
                    component.kind.as_str()
                ),
            ));
        }
    }

    let hdl_models = request
        .circuit
        .models
        .iter()
        .filter(|model| {
            model.kind == ModelKind::Module
                && matches!(
                    model.language,
                    ModelLanguage::Verilog | ModelLanguage::SystemVerilog
                )
        })
        .collect::<Vec<_>>();
    if hdl_models.is_empty() {
        return Err(EngineError::new(
            "verilator_model_missing",
            "Verilator execution requires at least one inline Verilog/SystemVerilog module model",
        ));
    }
    for model in hdl_models.iter().copied() {
        validate_hdl_source(model)?;
    }

    let model_map = hdl_models
        .iter()
        .copied()
        .map(|model| (model.id.as_str(), model))
        .collect::<BTreeMap<_, _>>();
    let mut endpoint_node = BTreeMap::<(String, String), String>::new();
    let mut net_node = BTreeMap::<String, String>::new();
    for (net_index, net) in request.circuit.nets.iter().enumerate() {
        let node = format!("ox_n{net_index}");
        net_node.insert(net.id.as_str().to_owned(), node.clone());
        for endpoint in &net.endpoints {
            let key = (
                endpoint.component.as_str().to_owned(),
                endpoint.pin.as_str().to_owned(),
            );
            if endpoint_node.insert(key.clone(), node.clone()).is_some() {
                return Err(EngineError::new(
                    "verilator_endpoint_multiple_nets",
                    format!(
                        "endpoint `{}.{}` is connected to multiple nets",
                        key.0, key.1
                    ),
                ));
            }
        }
    }

    let mut lines = vec![format!("module {TESTBENCH_MODULE};")];
    for node in net_node.values() {
        lines.push(format!("  wire {node};"));
    }

    for (component_index, component) in request.circuit.components.iter().enumerate() {
        match component.kind.as_str() {
            "logic_input" => {
                compile_logic_input(component, component_index, &endpoint_node, &mut lines)?
            }
            "digital_clock" => {
                compile_clock(component, component_index, stop, &endpoint_node, &mut lines)?
            }
            "logic_output" => {
                require_connected_pin(component, "in", &endpoint_node)?;
            }
            "hdl_module" => compile_hdl_instance(
                component,
                component_index,
                &endpoint_node,
                &model_map,
                &mut lines,
            )?,
            _ => unreachable!("supported component set checked above"),
        }
    }

    let stop_ticks = seconds_to_picoseconds(stop, "analysis stop time")?;
    lines.extend([
        "  initial begin".to_owned(),
        format!("    $dumpfile(\"{VCD_FILE}\");"),
        format!("    $dumpvars(0, {TESTBENCH_MODULE});"),
        format!("    #{stop_ticks};"),
        "    $finish;".to_owned(),
        "  end".to_owned(),
        "endmodule".to_owned(),
        String::new(),
    ]);

    let sources = hdl_models
        .iter()
        .copied()
        .enumerate()
        .map(|(index, model)| HdlSourceFile {
            filename: format!(
                "hdl_{index}.{}",
                match model.language {
                    ModelLanguage::Verilog => "v",
                    ModelLanguage::SystemVerilog => "sv",
                    ModelLanguage::Spice => unreachable!("filtered HDL model"),
                }
            ),
            source: model.source.clone(),
        })
        .collect::<Vec<_>>();
    let probes = compile_probes(request, &endpoint_node, &net_node)?;

    Ok(CompiledVerilatorRequest {
        sources,
        wrapper: lines.join("\n"),
        probes,
    })
}

fn is_supported_component(kind: &ComponentKind) -> bool {
    *kind == ComponentKind::hdl_module()
        || *kind == ComponentKind::logic_input()
        || *kind == ComponentKind::digital_clock()
        || *kind == ComponentKind::logic_output()
}

fn compile_logic_input(
    component: &Component,
    component_index: usize,
    endpoint_node: &BTreeMap<(String, String), String>,
    lines: &mut Vec<String>,
) -> Result<(), EngineError> {
    let node = require_connected_pin(component, "out", endpoint_node)?;
    let value = required_logic_value(component, "value")?;
    if !value.is_known() {
        return Err(component_error(
            component,
            "Verilator R5 boundary inputs must be known zero/one values; use the R3 reference engine when X/Z propagation is required",
        ));
    }
    let driver = format!("ox_drv_{component_index}");
    lines.push(format!("  logic {driver} = {};", logic_literal(value)));
    lines.push(format!("  assign {node} = {driver};"));
    Ok(())
}

fn compile_clock(
    component: &Component,
    component_index: usize,
    _stop: f64,
    endpoint_node: &BTreeMap<(String, String), String>,
    lines: &mut Vec<String>,
) -> Result<(), EngineError> {
    let node = require_connected_pin(component, "out", endpoint_node)?;
    let period = optional_non_negative_seconds(component, "period")?
        .ok_or_else(|| component_error(component, "required parameter `period` is missing"))?;
    if period <= 0.0 {
        return Err(component_error(
            component,
            "parameter `period` must be greater than zero",
        ));
    }
    let duty = duty_cycle(component)?;
    let initial = optional_logic_value(component, "initial")?.unwrap_or(LogicValue::Zero);
    if !initial.is_known() {
        return Err(component_error(
            component,
            "Verilator clock initial value must be zero or one",
        ));
    }
    let high_ticks = seconds_to_picoseconds(period * duty, "clock high duration")?;
    let low_ticks = seconds_to_picoseconds(period * (1.0 - duty), "clock low duration")?;
    let driver = format!("ox_clk_{component_index}");
    lines.push(format!("  logic {driver} = {};", logic_literal(initial)));
    lines.push(format!("  assign {node} = {driver};"));
    lines.push("  initial begin".to_owned());
    if initial == LogicValue::One {
        lines.push("    forever begin".to_owned());
        lines.push(format!("      #{high_ticks} {driver} = 1'b0;"));
        lines.push(format!("      #{low_ticks} {driver} = 1'b1;"));
        lines.push("    end".to_owned());
    } else {
        lines.push("    forever begin".to_owned());
        lines.push(format!("      #{low_ticks} {driver} = 1'b1;"));
        lines.push(format!("      #{high_ticks} {driver} = 1'b0;"));
        lines.push("    end".to_owned());
    }
    lines.push("  end".to_owned());
    Ok(())
}

fn compile_hdl_instance(
    component: &Component,
    component_index: usize,
    endpoint_node: &BTreeMap<(String, String), String>,
    model_map: &BTreeMap<&str, &ModelDefinition>,
    lines: &mut Vec<String>,
) -> Result<(), EngineError> {
    let model_id = match component.parameters.get("model") {
        Some(ParameterValue::Text(model_id)) if !model_id.trim().is_empty() => model_id.as_str(),
        Some(_) => {
            return Err(component_error(
                component,
                "parameter `model` must be a non-empty HDL model id",
            ));
        }
        None => {
            return Err(component_error(
                component,
                "required parameter `model` is missing",
            ));
        }
    };
    let model = model_map.get(model_id).ok_or_else(|| {
        component_error(
            component,
            format!("HDL model `{model_id}` is not a Verilog/SystemVerilog module model"),
        )
    })?;
    if !is_safe_identifier(&model.entry) {
        return Err(component_error(
            component,
            format!(
                "HDL model entry `{}` is not a safe module identifier",
                model.entry
            ),
        ));
    }

    let mut bindings = BTreeMap::<String, Vec<(Option<usize>, String)>>::new();
    for pin in &component.pins {
        if pin.domain != SignalDomain::Digital
            || !matches!(pin.direction, PinDirection::Input | PinDirection::Output)
        {
            return Err(component_error(
                component,
                format!(
                    "HDL pin `{}` must be a digital input or output in R5",
                    pin.id.as_str()
                ),
            ));
        }
        let node = require_connected_pin(component, pin.id.as_str(), endpoint_node)?;
        let (port, bit) = parse_port_reference(&pin.name).map_err(|message| {
            component_error(
                component,
                format!(
                    "pin `{}` has invalid HDL port mapping: {message}",
                    pin.id.as_str()
                ),
            )
        })?;
        bindings
            .entry(port)
            .or_default()
            .push((bit, node.to_owned()));
    }
    if bindings.is_empty() {
        return Err(component_error(
            component,
            "hdl_module must expose at least one mapped digital pin",
        ));
    }

    let mut parameter_bindings = Vec::new();
    for (key, value) in &component.parameters {
        let Some(name) = key.strip_prefix("param.") else {
            continue;
        };
        if !is_safe_identifier(name) {
            return Err(component_error(
                component,
                format!("HDL parameter override `{name}` is not a safe identifier"),
            ));
        }
        parameter_bindings.push(format!(".{name}({})", parameter_literal(value)?));
    }

    let mut port_bindings = Vec::new();
    for (port, mut bits) in bindings {
        bits.sort_by_key(|(bit, _)| *bit);
        let expression = match bits.as_slice() {
            [(None, node)] => node.clone(),
            _ if bits.iter().all(|(bit, _)| bit.is_some()) => {
                let mut seen = BTreeSet::new();
                let max_bit = bits
                    .iter()
                    .map(|(bit, _)| bit.expect("checked vector binding"))
                    .max()
                    .expect("non-empty binding");
                for (bit, _) in &bits {
                    let bit = bit.expect("checked vector binding");
                    if !seen.insert(bit) {
                        return Err(component_error(
                            component,
                            format!("HDL port `{port}` maps bit {bit} more than once"),
                        ));
                    }
                }
                if seen.len() != max_bit + 1 {
                    return Err(component_error(
                        component,
                        format!(
                            "HDL vector port `{port}` must map every bit from 0 through {max_bit}"
                        ),
                    ));
                }
                let by_bit = bits
                    .into_iter()
                    .map(|(bit, node)| (bit.expect("checked vector binding"), node))
                    .collect::<BTreeMap<_, _>>();
                format!(
                    "{{{}}}",
                    (0..=max_bit)
                        .rev()
                        .map(|bit| by_bit
                            .get(&bit)
                            .expect("contiguous vector mapping")
                            .as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            _ => {
                return Err(component_error(
                    component,
                    format!("HDL port `{port}` mixes scalar and vector pin mappings"),
                ));
            }
        };
        port_bindings.push(format!(".{port}({expression})"));
    }

    let parameter_clause = if parameter_bindings.is_empty() {
        String::new()
    } else {
        format!(" #({})", parameter_bindings.join(", "))
    };
    lines.push(format!(
        "  {}{} ox_u{} ({});",
        model.entry,
        parameter_clause,
        component_index,
        port_bindings.join(", ")
    ));
    Ok(())
}

fn compile_probes(
    request: &SimulationRequest,
    endpoint_node: &BTreeMap<(String, String), String>,
    net_node: &BTreeMap<String, String>,
) -> Result<Vec<CompiledProbe>, EngineError> {
    if request.probes.is_empty() {
        return Ok(request
            .circuit
            .nets
            .iter()
            .map(|net| CompiledProbe {
                signal: format!("net:{}", net.id.as_str()),
                node: net_node
                    .get(net.id.as_str())
                    .cloned()
                    .expect("compiled Verilator net owns node alias"),
            })
            .collect());
    }

    request
        .probes
        .iter()
        .map(|probe| {
            let key = (
                probe.endpoint.component.as_str().to_owned(),
                probe.endpoint.pin.as_str().to_owned(),
            );
            let node = endpoint_node.get(&key).cloned().ok_or_else(|| {
                EngineError::new(
                    "verilator_unknown_probe",
                    format!("probe endpoint `{}.{}` is not connected", key.0, key.1),
                )
            })?;
            let net = request
                .circuit
                .nets
                .iter()
                .find(|net| net_node.get(net.id.as_str()) == Some(&node))
                .map(|net| net.id.as_str())
                .ok_or_else(|| {
                    EngineError::new(
                        "verilator_unknown_probe_net",
                        "probe references a node without a circuit net",
                    )
                })?;
            Ok(CompiledProbe {
                signal: probe.alias.clone().unwrap_or_else(|| format!("net:{net}")),
                node,
            })
        })
        .collect()
}

fn require_connected_pin<'a>(
    component: &Component,
    pin: &str,
    endpoint_node: &'a BTreeMap<(String, String), String>,
) -> Result<&'a str, EngineError> {
    component
        .pins
        .iter()
        .find(|candidate| candidate.id.as_str() == pin)
        .ok_or_else(|| component_error(component, format!("required pin `{pin}` is missing")))?;
    endpoint_node
        .get(&(component.id.as_str().to_owned(), pin.to_owned()))
        .map(String::as_str)
        .ok_or_else(|| component_error(component, format!("pin `{pin}` must be connected")))
}

fn parse_port_reference(value: &str) -> Result<(String, Option<usize>), String> {
    let value = value.trim();
    if is_safe_identifier(value) {
        return Ok((value.to_owned(), None));
    }
    let Some(open) = value.rfind('[') else {
        return Err(format!("`{value}` is not a valid HDL identifier"));
    };
    if !value.ends_with(']') {
        return Err(format!("`{value}` has an unterminated vector index"));
    }
    let base = &value[..open];
    if !is_safe_identifier(base) {
        return Err(format!("`{base}` is not a valid HDL port identifier"));
    }
    let raw_bit = &value[open + 1..value.len() - 1];
    let bit = raw_bit
        .parse::<usize>()
        .map_err(|_| format!("`{raw_bit}` is not a non-negative vector index"))?;
    Ok((base.to_owned(), Some(bit)))
}

fn is_safe_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| {
            character == '_' || character == '$' || character.is_ascii_alphanumeric()
        })
}

fn parameter_literal(value: &ParameterValue) -> Result<String, EngineError> {
    match value {
        ParameterValue::Boolean(value) => Ok(if *value { "1" } else { "0" }.to_owned()),
        ParameterValue::Integer(value) => Ok(value.to_string()),
        ParameterValue::Number(value) if value.is_finite() => Ok(format!("{value:.17}")),
        ParameterValue::Text(value) => Ok(format!("\"{}\"", escape_sv_string(value))),
        ParameterValue::Number(_) | ParameterValue::Quantity(_) => Err(EngineError::new(
            "verilator_parameter_invalid",
            "HDL parameter overrides support boolean, integer, finite number, or text values",
        )),
    }
}

fn escape_sv_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn validate_hdl_source(model: &ModelDefinition) -> Result<(), EngineError> {
    if model.source.contains('\0') {
        return Err(EngineError::new(
            "verilator_unsafe_source",
            format!("HDL model `{}` contains a NUL byte", model.id.as_str()),
        ));
    }
    let lowered = model.source.to_ascii_lowercase();
    for forbidden in [
        "`include",
        "$system",
        "$fopen",
        "$fclose",
        "$fwrite",
        "$fdisplay",
        "$readmemh",
        "$readmemb",
        "$writememh",
        "$writememb",
        "$dumpfile",
        "$dumpvars",
        "$finish",
        "$stop",
        "import \"dpi-c\"",
        "export \"dpi-c\"",
        "$c(",
    ] {
        if lowered.contains(forbidden) {
            return Err(EngineError::new(
                "verilator_unsafe_source",
                format!(
                    "HDL model `{}` uses host/testbench construct `{forbidden}` which is outside the R5 inline-module boundary",
                    model.id.as_str()
                ),
            ));
        }
    }
    Ok(())
}

fn logic_literal(value: LogicValue) -> &'static str {
    match value {
        LogicValue::Zero => "1'b0",
        LogicValue::One => "1'b1",
        LogicValue::X => "1'bx",
        LogicValue::Z => "1'bz",
    }
}

fn seconds_to_picoseconds(seconds: f64, label: &str) -> Result<u64, EngineError> {
    let ticks = seconds * PICOSECONDS_PER_SECOND;
    if !ticks.is_finite() || ticks <= 0.0 || ticks > u64::MAX as f64 {
        return Err(EngineError::new(
            "verilator_time_invalid",
            format!("{label} cannot be represented by the 1 ps R5 Verilator timeline"),
        ));
    }
    Ok(ticks.round().max(1.0) as u64)
}

fn locate_binary(run_dir: &Path) -> Option<PathBuf> {
    [
        run_dir.join(BUILD_DIR).join(BINARY_FILE),
        run_dir.join(BINARY_FILE),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn wait_for_verilator_child(
    child: &mut std::process::Child,
    run_dir: &Path,
    control: &ExecutionControl,
    started: &Instant,
    log_file: &str,
    result_file: Option<&str>,
    phase: &str,
) -> Result<std::process::ExitStatus, EngineError> {
    let poll_interval = Duration::from_millis(control.policy.poll_interval_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                return Err(EngineError::new(
                    "verilator_wait_failed",
                    format!("could not query Verilator {phase} process status: {error}"),
                ));
            }
        }
        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                format!("Verilator {phase} execution was cancelled"),
            ));
        }
        if control.policy.timeout_ms != 0
            && started.elapsed() >= Duration::from_millis(control.policy.timeout_ms)
        {
            return Err(EngineError::new(
                "execution_timeout",
                format!(
                    "Verilator execution exceeded total timeout of {} ms",
                    control.policy.timeout_ms
                ),
            )
            .retryable(true));
        }
        enforce_file_size(&run_dir.join(log_file), "log", control.policy.max_log_bytes)?;
        if let Some(result_file) = result_file {
            enforce_file_size(
                &run_dir.join(result_file),
                "result",
                control.policy.max_output_bytes,
            )?;
        }
        thread::sleep(poll_interval);
    }
}

fn enforce_file_size(path: &Path, resource: &str, max_bytes: u64) -> Result<(), EngineError> {
    match fs::metadata(path) {
        Ok(metadata) => enforce_buffer_limit(resource, metadata.len(), max_bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EngineError::new(
            "verilator_resource_inspection_failed",
            format!("could not inspect Verilator {resource} file: {error}"),
        )),
    }
}

fn read_optional_log(path: &Path, max_bytes: u64, error_code: &str) -> Result<String, EngineError> {
    match read_text_limited(path, max_bytes, "log", error_code) {
        Ok(value) => Ok(value),
        Err(error) if error.code() == error_code && !path.exists() => Ok(String::new()),
        Err(error) => Err(error),
    }
}

fn extract_verilator_warnings(log: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    log.lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("%Warning") || line.to_ascii_lowercase().starts_with("warning")
        })
        .filter(|line| seen.insert((*line).to_owned()))
        .map(str::to_owned)
        .collect()
}

fn component_error(component: &Component, message: impl Into<String>) -> EngineError {
    EngineError::new(
        "verilator_component_invalid",
        format!("component `{}`: {}", component.id.as_str(), message.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Circuit, Net, NetEndpoint, Pin, Probe};

    fn digital_pin(id: &str, name: &str, direction: PinDirection) -> Pin {
        Pin::new(id, name, SignalDomain::Digital, direction)
    }

    #[test]
    fn port_reference_supports_scalar_and_vector_bits() {
        assert_eq!(
            parse_port_reference("data").unwrap(),
            ("data".to_owned(), None)
        );
        assert_eq!(
            parse_port_reference("data[7]").unwrap(),
            ("data".to_owned(), Some(7))
        );
        assert!(parse_port_reference("data[-1]").is_err());
        assert!(parse_port_reference("7data").is_err());
    }

    #[test]
    fn compiler_generates_vector_binding_and_deterministic_testbench() {
        let module = ModelDefinition::system_verilog_module(
            "invert2",
            "invert2",
            "module invert2(input logic [1:0] a, output logic [1:0] y); assign y = ~a; endmodule",
        );
        let zero = Component::new("zero", ComponentKind::logic_input())
            .with_pin(digital_pin("out", "out", PinDirection::Output))
            .with_parameter("value", ParameterValue::Integer(0));
        let one = Component::new("one", ComponentKind::logic_input())
            .with_pin(digital_pin("out", "out", PinDirection::Output))
            .with_parameter("value", ParameterValue::Integer(1));
        let dut = Component::new("dut", ComponentKind::hdl_module())
            .with_pin(digital_pin("a0", "a[0]", PinDirection::Input))
            .with_pin(digital_pin("a1", "a[1]", PinDirection::Input))
            .with_pin(digital_pin("y0", "y[0]", PinDirection::Output))
            .with_pin(digital_pin("y1", "y[1]", PinDirection::Output))
            .with_parameter("model", ParameterValue::Text("invert2".into()));
        let circuit = Circuit::new()
            .with_model(module)
            .with_component(zero)
            .with_component(one)
            .with_component(dut)
            .with_net(
                Net::new("a0")
                    .connect(NetEndpoint::new("zero", "out"))
                    .connect(NetEndpoint::new("dut", "a0")),
            )
            .with_net(
                Net::new("a1")
                    .connect(NetEndpoint::new("one", "out"))
                    .connect(NetEndpoint::new("dut", "a1")),
            )
            .with_net(Net::new("y0").connect(NetEndpoint::new("dut", "y0")))
            .with_net(Net::new("y1").connect(NetEndpoint::new("dut", "y1")));
        let request = SimulationRequest {
            circuit,
            analysis: Analysis::DigitalTransient { stop: 10e-9 },
            probes: vec![Probe {
                endpoint: NetEndpoint::new("dut", "y0"),
                alias: Some("y0".into()),
            }],
        };
        let compiled = compile_request(&request).unwrap();
        assert!(compiled.wrapper.contains("invert2 ox_u2"));
        assert!(compiled.wrapper.contains(".a({ox_n1, ox_n0})"));
        assert!(compiled.wrapper.contains(".y({ox_n3, ox_n2})"));
        assert!(compiled.wrapper.contains("#10000;"));
        assert_eq!(compiled.probes[0].signal, "y0");
        assert_eq!(compiled.probes[0].node, "ox_n2");
    }

    #[test]
    fn cosim_harness_uses_incremental_verilator_time_api_and_bounded_events() {
        let ports = vec![
            crate::CoSimulationPort::digital("clk", PinDirection::Input),
            crate::CoSimulationPort::digital("q", PinDirection::Output),
        ];
        let harness = generate_cosim_harness(&ports).unwrap();
        assert!(harness.contains("top->eval();"));
        assert!(harness.contains("top->eventsPending()"));
        assert!(harness.contains("top->nextTimeSlot()"));
        assert!(harness.contains("internal_steps > 1000000ULL"));
        assert!(harness.contains("case 0: top->clk = value & 1U; break;"));
    }

    #[test]
    fn cosim_state_protocol_rejects_non_future_events_and_parses_outputs() {
        let outputs = vec!["q".to_owned()];
        let mut current = crate::CoSimulationTime::ZERO;
        let mut next = None;
        let mut values = BTreeMap::new();
        parse_cosim_state_line(
            "STATE 100 q=1 NEXT=125",
            &outputs,
            &mut current,
            &mut next,
            &mut values,
        )
        .unwrap();
        assert_eq!(current.as_picoseconds(), 100);
        assert_eq!(next.unwrap().as_picoseconds(), 125);
        assert_eq!(values.get("q"), Some(&LogicValue::One));

        let error = parse_cosim_state_line(
            "STATE 100 q=0 NEXT=100",
            &outputs,
            &mut current,
            &mut next,
            &mut values,
        )
        .unwrap_err();
        assert_eq!(error.code(), "verilator_cosim_protocol_error");
    }

    #[test]
    fn unsafe_inline_host_io_is_rejected() {
        let model = ModelDefinition::system_verilog_module(
            "unsafe",
            "unsafe",
            "module unsafe; initial $system(\"touch /tmp/nope\"); endmodule",
        );
        let error = validate_hdl_source(&model).unwrap_err();
        assert_eq!(error.code(), "verilator_unsafe_source");
    }
}
