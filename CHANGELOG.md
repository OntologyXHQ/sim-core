# Changelog

## [0.6.0] - 2026-09-09

### Added

- built-in pure-Rust four-state digital event engine
- digital constants, clocks, buffer/inverter and AND/OR/XOR/NAND/NOR/XNOR gates
- deterministic event-driven digital waveforms with zero/one/unknown/high-impedance semantics
- propagation-delay support and bounded digital event/output execution
- validation for multiple drivers on pure digital nets
- public digital component constructors and stable digital-engine id

### Changed

- R3 digital semantics now have a solver-independent reference implementation before the XSPICE adapter

## [0.5.0] - 2026-09-09

### Added

- public `ExecutionPolicy`, `ExecutionControl` and cloneable `CancellationToken`
- bounded default execution policy with wall-clock, input, output and log limits
- stable execution error classifiers for cancellation, timeout and resource limits
- real process-control proofs for timeout, cancellation and output growth

### Changed

- ngspice execution now uses a controllable child process with RAII kill/reap cleanup
- `Simulator::simulate` now runs through the bounded default execution policy

## [0.4.0] - 2026-09-09

### Added

- canonical `cargo snapshot` command backed by `.cargo/config.toml`
- engine descriptors with solver version and capabilities
- versioned deterministic simulation-result metadata and result-size statistics
- stable top-level simulation error codes
- public contract documentation for pre-digital hardening

### Changed

- normalized simulation results now use `SimulationResult::new` to stamp provenance


## 0.3.0 — standalone core baseline

- extracted the verified Rust simulation authority from OXSim into a pure-Rust repository
- renamed the public crate from the unpublished `ontologyx-sim` identity to `ontologyx-sim-core`
- retained Circuit IR schema version 1 and the existing simulation behavior
- retained the process-isolated ngspice engine and model registry
- removed Node/N-API and product-shell ownership from the core boundary
