# Changelog

## [0.7.0] - 2026-09-09

### Added

- `XSpiceEngine` as a second digital backend using ngspice XSPICE event models
- XSPICE capability probing and stable `XSpiceInfo` runtime metadata
- deterministic Circuit IR compilation to `d_source`, `d_buffer`, `d_inverter`, and binary gate code models
- VCD event-node parsing into normalized `DigitalWaveform` results
- real reference-engine parity proofs for combinational logic, propagation delay, and clocked events
- explicit `XSPICE_MIN_DELAY_SECONDS` timing floor and result diagnostic when clamping is required

### Changed

- ngspice process-control primitives are shared internally with the XSPICE adapter so timeout, cancellation, and output limits stay consistent across process-backed engines

### Fixed

- corrected the XSPICE VCD export timescale so sub-nanosecond event timestamps are preserved
- aligned parity tests with XSPICE ZERO-at-start initialization while keeping the built-in reference engine X-initialized
- forced XSPICE transport-delay mode to match the canonical DigitalEngine semantics

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
