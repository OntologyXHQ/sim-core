//! OntologyX Sim Core: pure Rust circuit contracts and simulation engines.
//!
//! The public circuit IR and normalized result model are solver-independent.
//! R2 adds the first real engine: an isolated ngspice batch-process adapter.
//! R2.1 extends the solver-independent IR with safe inline model definitions
//! and ngspice semiconductor/subcircuit instantiation. R2.2 begins public
//! contract hardening with deterministic result metadata and bounded execution controls.
//! R3 adds a solver-independent four-state digital event foundation and built-in reference engine.
//! R3.1 adds an ngspice/XSPICE adapter with VCD-normalized digital waveforms and parity proofs.

pub mod analysis;
pub mod circuit;
pub mod digital;
pub mod engine;
pub mod model;
pub mod ngspice;
pub mod units;
pub mod validation;
pub mod waveform;
pub mod xspice;

pub use analysis::{AcScale, Analysis, AnalysisDomain, AnalysisKind};
pub use circuit::{
    Circuit, Component, ComponentId, ComponentKind, Net, NetEndpoint, NetId, ParameterValue, Pin,
    PinDirection, PinId, SignalDomain,
};
pub use digital::{DIGITAL_ENGINE_ID, DigitalEngine, MAX_DIGITAL_EVENTS};
pub use engine::{
    CancellationToken, DEFAULT_EXECUTION_POLL_INTERVAL_MS, DEFAULT_EXECUTION_TIMEOUT_MS,
    DEFAULT_MAX_INPUT_BYTES, DEFAULT_MAX_LOG_BYTES, DEFAULT_MAX_OUTPUT_BYTES, EngineCapabilities,
    EngineDescriptor, EngineError, EngineId, EngineRegistry, ExecutionControl, ExecutionPolicy,
    Probe, SimulationEngine, SimulationError, SimulationRequest, Simulator,
};
pub use model::{ModelDefinition, ModelId, ModelKind, ModelLanguage};
pub use ngspice::{NgSpiceEngine, NgSpiceInfo};
pub use units::{Quantity, Unit};
pub use validation::{IssueSeverity, ValidationIssue, ValidationReport, validate_circuit};
pub use waveform::{
    AnalogAxis, AnalogWaveform, AxisKind, Diagnostic, DiagnosticLevel, DigitalTransition,
    DigitalWaveform, LogicValue, SIMULATION_RESULT_SCHEMA_VERSION, SignalId, SimulationMetadata,
    SimulationResult, SimulationStats, Waveform,
};
pub use xspice::{XSPICE_ENGINE_ID, XSPICE_MIN_DELAY_SECONDS, XSpiceEngine, XSpiceInfo};

/// Current public Circuit IR schema version.
pub const CIRCUIT_SCHEMA_VERSION: u16 = 1;
/// Current crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
