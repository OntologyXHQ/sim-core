use serde::{Deserialize, Serialize};

use crate::{AnalysisKind, CIRCUIT_SCHEMA_VERSION, EngineId, Unit, VERSION};

/// Current normalized simulation-result schema version.
pub const SIMULATION_RESULT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SignalId(pub String);

impl SignalId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisKind {
    Scalar,
    Time,
    DcSweep,
    Frequency,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalogAxis {
    pub kind: AxisKind,
    pub unit: Unit,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalogWaveform {
    pub signal: SignalId,
    pub unit: Unit,
    pub axis: AnalogAxis,
    pub values: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imaginary: Option<Vec<f64>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicValue {
    Zero,
    One,
    X,
    Z,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DigitalTransition {
    pub time: f64,
    pub value: LogicValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DigitalWaveform {
    pub signal: SignalId,
    pub transitions: Vec<DigitalTransition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum Waveform {
    Analog(AnalogWaveform),
    Digital(DigitalWaveform),
}

impl Waveform {
    pub fn point_count(&self) -> usize {
        match self {
            Self::Analog(waveform) => waveform.values.len(),
            Self::Digital(waveform) => waveform.transitions.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
}

/// Deterministic provenance attached to every normalized result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimulationMetadata {
    pub result_schema_version: u16,
    pub circuit_schema_version: u16,
    pub core_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
}

impl SimulationMetadata {
    pub fn current(circuit_schema_version: u16, engine_version: Option<String>) -> Self {
        Self {
            result_schema_version: SIMULATION_RESULT_SCHEMA_VERSION,
            circuit_schema_version,
            core_version: VERSION.to_owned(),
            engine_version,
        }
    }
}

impl Default for SimulationMetadata {
    fn default() -> Self {
        Self::current(CIRCUIT_SCHEMA_VERSION, None)
    }
}

/// Deterministic result-size statistics. Runtime timings intentionally do not
/// live here because the normalized result is also used for reproducibility.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimulationStats {
    pub waveform_count: usize,
    pub point_count: usize,
}

impl SimulationStats {
    pub fn from_waveforms(waveforms: &[Waveform]) -> Self {
        Self {
            waveform_count: waveforms.len(),
            point_count: waveforms.iter().map(Waveform::point_count).sum(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationResult {
    pub engine: EngineId,
    pub analysis: AnalysisKind,
    pub metadata: SimulationMetadata,
    pub stats: SimulationStats,
    #[serde(default)]
    pub waveforms: Vec<Waveform>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl SimulationResult {
    pub fn new(
        engine: EngineId,
        engine_version: Option<String>,
        analysis: AnalysisKind,
        circuit_schema_version: u16,
        waveforms: Vec<Waveform>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        let stats = SimulationStats::from_waveforms(&waveforms);
        Self {
            engine,
            analysis,
            metadata: SimulationMetadata::current(circuit_schema_version, engine_version),
            stats,
            waveforms,
            diagnostics,
        }
    }
}
