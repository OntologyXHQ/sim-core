use std::{collections::BTreeSet, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    Analysis, AnalysisDomain, AnalysisKind, Circuit, NetEndpoint, SimulationResult,
    ValidationReport, validate_circuit,
};

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
    fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult, EngineError>;
}

#[derive(Default)]
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

    pub fn select(&self, analysis: &Analysis) -> Option<Arc<dyn SimulationEngine>> {
        self.engines
            .iter()
            .find(|engine| engine.capabilities().supports(analysis))
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

#[derive(Default)]
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

    pub fn validate(&self, circuit: &Circuit) -> ValidationReport {
        validate_circuit(circuit)
    }

    pub fn simulate(
        &self,
        request: &SimulationRequest,
    ) -> Result<SimulationResult, SimulationError> {
        let report = self.validate(&request.circuit);
        if !report.is_valid() {
            return Err(SimulationError::InvalidCircuit { report });
        }
        let Some(engine) = self.registry.select(&request.analysis) else {
            return Err(SimulationError::NoCompatibleEngine {
                analysis: request.analysis.kind(),
            });
        };
        let engine_id = engine.id();
        engine
            .simulate(request)
            .map_err(|error| SimulationError::Engine {
                engine: engine_id,
                error,
            })
    }
}
