//! OntologyX Sim Core: pure Rust circuit contracts and simulation engines.
//!
//! The public circuit IR and normalized result model are solver-independent.
//! R2 adds the first real engine: an isolated ngspice batch-process adapter.
//! R2.1 extends the solver-independent IR with safe inline model definitions
//! and ngspice semiconductor/subcircuit instantiation. R2.2 begins public
//! contract hardening with deterministic result metadata and bounded execution controls.
//! R3 adds a solver-independent four-state digital event foundation and built-in reference engine.
//! R3.1 adds an ngspice/XSPICE adapter with VCD-normalized digital waveforms and parity proofs.
//! R3 completes the digital layer with sequential logic, tri-state/multi-driver resolution,
//! bus-width primitives, multiplexing, decoding, registers, and counters.
//! R4 adds explicit ADC/DAC bridges and a coordinated single-process ngspice/XSPICE mixed-signal transient engine.
//! R5 adds Verilator-backed inline Verilog/SystemVerilog module blocks with normalized VCD waveforms.
//! R6 adds a thin Renode-backed MCU/firmware runtime with virtual-time GPIO observation.
//! R7 adds an optional Axum service boundary for bounded asynchronous simulation jobs.
//! R8 adds an optional durable production runtime with parallel workers, content-addressed caching, reproducibility manifests, crash recovery, and a Linux bubblewrap/prlimit isolation adapter.
//! R9.1 adds the deterministic shared co-simulation scheduler substrate used by incremental backend sessions.
//! R9.2 adds built-in digital and live Verilator participants on the shared scheduler timeline.
//! R9.3 adds process-isolated Renode External Control and ngspice SharedSpice participants.
//! R9.4 adds explicit scheduler-native ADC/DAC bridges and the closed-loop firmware/RTL/analog proof.

pub mod analysis;
pub mod circuit;
pub mod cosim;
pub mod cosim_bridge;
pub mod digital;
pub mod engine;
pub mod firmware;
pub mod mixed;
pub mod model;
pub mod ngspice;
pub mod renode;
#[cfg(feature = "production")]
pub mod runtime;
#[cfg(feature = "service")]
pub mod service;
pub mod units;
pub mod validation;
pub mod verilator;
pub mod waveform;
pub mod xspice;

pub use analysis::{AcScale, Analysis, AnalysisDomain, AnalysisKind};
pub use circuit::{
    Circuit, Component, ComponentId, ComponentKind, Net, NetEndpoint, NetId, ParameterValue, Pin,
    PinDirection, PinId, SignalDomain,
};
pub use cosim::{
    COSIM_SCHEDULER_ID, COSIM_TIMEBASE_HZ, CoSimulationConfig, CoSimulationEndpoint,
    CoSimulationLink, CoSimulationParticipant, CoSimulationPort, CoSimulationReport,
    CoSimulationScheduler, CoSimulationStats, CoSimulationTime, CoSimulationValue,
    DEFAULT_MAX_COSIM_STEPS, DEFAULT_MAX_DELTA_CYCLES,
};
pub use cosim_bridge::{
    ADC_COSIM_ANALOG_IN, ADC_COSIM_DIGITAL_OUT, AdcCoSimulationParticipant, DAC_COSIM_ANALOG_OUT,
    DAC_COSIM_DIGITAL_IN, DacCoSimulationParticipant,
};
pub use digital::{
    DIGITAL_ENGINE_ID, DigitalCoSimulationBinding, DigitalCoSimulationParticipant, DigitalEngine,
    LogicVector, MAX_DIGITAL_BUS_WIDTH, MAX_DIGITAL_EVENTS,
};
pub use engine::{
    CancellationToken, DEFAULT_EXECUTION_POLL_INTERVAL_MS, DEFAULT_EXECUTION_TIMEOUT_MS,
    DEFAULT_MAX_INPUT_BYTES, DEFAULT_MAX_LOG_BYTES, DEFAULT_MAX_OUTPUT_BYTES, EngineCapabilities,
    EngineDescriptor, EngineError, EngineId, EngineRegistry, ExecutionControl, ExecutionPolicy,
    Probe, SimulationEngine, SimulationError, SimulationRequest, Simulator,
};
pub use firmware::{FirmwareArtifact, FirmwareFormat};
pub use mixed::{MIXED_SIGNAL_ENGINE_ID, MixedSignalEngine, MixedSignalInfo};
pub use model::{ModelDefinition, ModelId, ModelKind, ModelLanguage};
pub use ngspice::{
    NGSPICE_COSIM_DEFAULT_HELPER, NgSpiceCoSimulationBinding, NgSpiceCoSimulationConfig,
    NgSpiceCoSimulationParticipant, NgSpiceEngine, NgSpiceInfo,
};
pub use renode::{
    MAX_RENODE_SAMPLE_POINTS, RENODE_COSIM_DEFAULT_HELPER, RENODE_COSIM_TIME_QUANTUM_PS,
    RENODE_ENGINE_ID, RenodeCoSimulationBinding, RenodeCoSimulationConfig,
    RenodeCoSimulationParticipant, RenodeEngine, RenodeInfo, RenodeTarget,
};

#[cfg(feature = "service")]
pub use service::{
    DEFAULT_MAX_CONCURRENT_JOBS, DEFAULT_MAX_RETAINED_JOBS, DEFAULT_MAX_SERVICE_REQUEST_BYTES,
    HealthResponse, ServiceError, ServiceLimits, SimulationJobEvent, SimulationJobFailure,
    SimulationJobId, SimulationJobSnapshot, SimulationJobStatus, SimulationService,
    SubmitSimulation,
};

#[cfg(feature = "production")]
pub use runtime::{
    ArtifactRef, ContentAddressedStore, DEFAULT_MAX_ATTEMPTS, DEFAULT_MAX_PARALLEL_WORKERS,
    DEFAULT_MAX_QUEUED_JOBS, DEFAULT_MAX_RETAINED_TERMINAL_JOBS, DEFAULT_WORKER_CPU_SECONDS,
    DEFAULT_WORKER_FILE_BYTES, DEFAULT_WORKER_MEMORY_BYTES, DEFAULT_WORKER_OPEN_FILES,
    DEFAULT_WORKER_PROCESSES, IsolatedProcessExecutor, IsolatedProcessLimits, ProductionRuntime,
    ProductionRuntimeLimits, RUNTIME_MANIFEST_SCHEMA_VERSION, ReproducibilityManifest,
    RuntimeError, RuntimeExecutor, RuntimeJobFailure, RuntimeJobId, RuntimeJobSnapshot,
    RuntimeJobStatus, SimulatorExecutor, WorkerRequestEnvelope, WorkerResponseEnvelope,
    default_worker_simulator,
};
pub use units::{Quantity, Unit};
pub use validation::{IssueSeverity, ValidationIssue, ValidationReport, validate_circuit};
pub use verilator::{
    VERILATOR_ENGINE_ID, VerilatorCoSimulationParticipant, VerilatorEngine, VerilatorInfo,
};
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
