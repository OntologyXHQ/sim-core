//! OntologyX Sim Core: pure Rust circuit contracts and simulation engines.
//!
//! The public circuit IR and normalized result model are solver-independent.
//! R2 adds the first real engine: an isolated ngspice batch-process adapter.
//! R2.1 extends the solver-independent IR with safe inline model definitions
//! and ngspice semiconductor/subcircuit instantiation.

pub mod analysis;
pub mod circuit;
pub mod engine;
pub mod model;
pub mod ngspice;
pub mod units;
pub mod validation;
pub mod waveform;

pub use analysis::{AcScale, Analysis, AnalysisDomain, AnalysisKind};
pub use circuit::{
    Circuit, Component, ComponentId, ComponentKind, Net, NetEndpoint, NetId, ParameterValue, Pin,
    PinDirection, PinId, SignalDomain,
};
pub use engine::{
    EngineCapabilities, EngineError, EngineId, EngineRegistry, Probe, SimulationEngine,
    SimulationError, SimulationRequest, Simulator,
};
pub use model::{ModelDefinition, ModelId, ModelKind, ModelLanguage};
pub use ngspice::{NgSpiceEngine, NgSpiceInfo};
pub use units::{Quantity, Unit};
pub use validation::{IssueSeverity, ValidationIssue, ValidationReport, validate_circuit};
pub use waveform::{
    AnalogAxis, AnalogWaveform, AxisKind, Diagnostic, DiagnosticLevel, DigitalTransition,
    DigitalWaveform, LogicValue, SignalId, SimulationResult, Waveform,
};

/// Current public Circuit IR schema version.
pub const CIRCUIT_SCHEMA_VERSION: u16 = 1;
/// Current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
