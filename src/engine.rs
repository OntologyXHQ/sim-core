use std::{
    collections::BTreeSet,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

use crate::{
    Analysis, AnalysisDomain, AnalysisKind, Circuit, NetEndpoint, SimulationResult,
    ValidationReport, validate_circuit,
};

pub const DEFAULT_EXECUTION_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_EXECUTION_POLL_INTERVAL_MS: u64 = 10;
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EngineId(pub String);

impl EngineId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EngineCapabilities {
    pub analog: bool,
    pub digital: bool,
    pub mixed_signal: bool,
    #[serde(default)]
    pub analyses: BTreeSet<AnalysisKind>,
}

impl EngineCapabilities {
    pub fn supports(&self, analysis: &Analysis) -> bool {
        let domain_supported = match analysis.domain() {
            AnalysisDomain::Analog => self.analog,
            AnalysisDomain::Digital => self.digital,
            AnalysisDomain::Mixed => self.mixed_signal,
        };
        domain_supported && self.analyses.contains(&analysis.kind())
    }
}

/// Stable, serializable description of a registered simulation engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EngineDescriptor {
    pub id: EngineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub capabilities: EngineCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub endpoint: NetEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationRequest {
    pub circuit: Circuit,
    pub analysis: Analysis,
    #[serde(default)]
    pub probes: Vec<Probe>,
}

/// Portable execution limits applied outside the serialized circuit/request schema.
/// A value of zero disables the corresponding limit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    /// Maximum wall-clock runtime for one engine invocation. `0` disables timeout.
    pub timeout_ms: u64,
    /// Poll interval used by process-backed engines while enforcing controls.
    pub poll_interval_ms: u64,
    /// Maximum generated solver input size. `0` disables the limit.
    pub max_input_bytes: u64,
    /// Maximum normalized solver-result file size. `0` disables the limit.
    pub max_output_bytes: u64,
    /// Maximum solver log file size. `0` disables the limit.
    pub max_log_bytes: u64,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_EXECUTION_TIMEOUT_MS,
            poll_interval_ms: DEFAULT_EXECUTION_POLL_INTERVAL_MS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_log_bytes: DEFAULT_MAX_LOG_BYTES,
        }
    }
}

impl ExecutionPolicy {
    /// An explicit unbounded policy for trusted/offline workloads.
    pub const fn unbounded() -> Self {
        Self {
            timeout_ms: 0,
            poll_interval_ms: DEFAULT_EXECUTION_POLL_INTERVAL_MS,
            max_input_bytes: 0,
            max_output_bytes: 0,
            max_log_bytes: 0,
        }
    }

    pub fn validate(&self) -> Result<(), EngineError> {
        if self.poll_interval_ms == 0 {
            return Err(EngineError::new(
                "execution_policy_invalid",
                "execution poll interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Cooperative, cloneable cancellation signal shared across threads/adapters.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Non-serialized execution controls for one simulation run.
#[derive(Clone, Debug, Default)]
pub struct ExecutionControl {
    pub policy: ExecutionPolicy,
    pub cancellation: CancellationToken,
}

impl ExecutionControl {
    pub fn new(policy: ExecutionPolicy) -> Self {
        Self {
            policy,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// Solver-specific failure with a stable machine-readable code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EngineError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl EngineError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }

    pub fn is_cancelled(&self) -> bool {
        self.code == "execution_cancelled"
    }

    pub fn is_timeout(&self) -> bool {
        self.code == "execution_timeout"
    }

    pub fn is_resource_limit(&self) -> bool {
        self.code == "execution_resource_limit"
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for EngineError {}

pub trait SimulationEngine: Send + Sync {
    fn id(&self) -> EngineId;
    fn capabilities(&self) -> EngineCapabilities;

    /// Human/tool-facing engine version. Engines that cannot determine a
    /// version cheaply may leave this unset.
    fn version(&self) -> Option<String> {
        None
    }

    fn descriptor(&self) -> EngineDescriptor {
        EngineDescriptor {
            id: self.id(),
            version: self.version(),
            capabilities: self.capabilities(),
        }
    }

    /// Request-aware capability hook used when multiple engines share an analysis kind.
    /// Backends with component-specific ownership (for example Verilator HDL blocks)
    /// should override this rather than relying on registration order.
    fn supports_request(&self, request: &SimulationRequest) -> bool {
        self.capabilities().supports(&request.analysis)
    }

    fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult, EngineError>;

    /// Controlled execution surface. In-process engines may use the default
    /// implementation; process-backed engines should override it to enforce
    /// timeout/cancellation/resource controls while the child is running.
    fn simulate_with_control(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, EngineError> {
        control.policy.validate()?;
        if control.cancellation.is_cancelled() {
            return Err(EngineError::new(
                "execution_cancelled",
                "simulation was cancelled before execution",
            ));
        }
        self.simulate(request)
    }
}

#[derive(Clone, Default)]
pub struct EngineRegistry {
    engines: Vec<Arc<dyn SimulationEngine>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<E: SimulationEngine + 'static>(&mut self, engine: E) {
        self.engines.push(Arc::new(engine));
    }

    pub fn ids(&self) -> Vec<EngineId> {
        self.engines.iter().map(|engine| engine.id()).collect()
    }

    pub fn descriptors(&self) -> Vec<EngineDescriptor> {
        self.engines
            .iter()
            .map(|engine| engine.descriptor())
            .collect()
    }

    pub fn select(&self, analysis: &Analysis) -> Option<Arc<dyn SimulationEngine>> {
        self.engines
            .iter()
            .find(|engine| engine.capabilities().supports(analysis))
            .cloned()
    }

    pub fn select_request(&self, request: &SimulationRequest) -> Option<Arc<dyn SimulationEngine>> {
        self.engines
            .iter()
            .find(|engine| engine.supports_request(request))
            .cloned()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SimulationError {
    InvalidCircuit {
        report: ValidationReport,
    },
    NoCompatibleEngine {
        analysis: AnalysisKind,
    },
    Engine {
        engine: EngineId,
        error: EngineError,
    },
}

impl SimulationError {
    /// Stable top-level error code for API adapters and telemetry.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidCircuit { .. } => "invalid_circuit",
            Self::NoCompatibleEngine { .. } => "no_compatible_engine",
            Self::Engine { .. } => "engine_error",
        }
    }
}

impl fmt::Display for SimulationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCircuit { report } => write!(
                f,
                "circuit validation failed with {} error(s)",
                report.error_count()
            ),
            Self::NoCompatibleEngine { analysis } => {
                write!(f, "no registered engine supports {analysis:?}")
            }
            Self::Engine { engine, error } => {
                write!(f, "engine `{}` failed: {error}", engine.as_str())
            }
        }
    }
}
impl std::error::Error for SimulationError {}

#[derive(Clone, Default)]
pub struct Simulator {
    registry: EngineRegistry,
}

impl Simulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_engine<E: SimulationEngine + 'static>(&mut self, engine: E) {
        self.registry.register(engine);
    }

    pub fn engine_ids(&self) -> Vec<EngineId> {
        self.registry.ids()
    }

    pub fn engine_descriptors(&self) -> Vec<EngineDescriptor> {
        self.registry.descriptors()
    }

    pub fn validate(&self, circuit: &Circuit) -> ValidationReport {
        validate_circuit(circuit)
    }

    pub fn simulate(
        &self,
        request: &SimulationRequest,
    ) -> Result<SimulationResult, SimulationError> {
        self.simulate_with_control(request, &ExecutionControl::default())
    }

    pub fn simulate_with_control(
        &self,
        request: &SimulationRequest,
        control: &ExecutionControl,
    ) -> Result<SimulationResult, SimulationError> {
        let report = self.validate(&request.circuit);
        if !report.is_valid() {
            return Err(SimulationError::InvalidCircuit { report });
        }
        let Some(engine) = self.registry.select_request(request) else {
            return Err(SimulationError::NoCompatibleEngine {
                analysis: request.analysis.kind(),
            });
        };
        let engine_id = engine.id();
        engine
            .simulate_with_control(request, control)
            .map_err(|error| SimulationError::Engine {
                engine: engine_id,
                error,
            })
    }
}
