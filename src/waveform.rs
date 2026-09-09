use serde::{Deserialize, Serialize};

use crate::{AnalysisKind, EngineId, Unit};

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationResult {
    pub engine: EngineId,
    pub analysis: AnalysisKind,
    #[serde(default)]
    pub waveforms: Vec<Waveform>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}
