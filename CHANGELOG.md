# Changelog

## 0.17.0 - R9.4 closed-loop co-simulation

- Added explicit scheduler-native `DacCoSimulationParticipant` and hysteretic `AdcCoSimulationParticipant`; cross-domain conversion remains outside the master scheduler and rejects ambiguous/non-finite values fail-closed.
- Added a portable closed-loop proof chaining the R9.3 Renode protocol participant -> live Verilator -> DAC -> R9.3 SharedSpice protocol participant -> ADC -> firmware input on one deterministic scheduler timeline.
- Added a self-contained Cortex-M4/STM32F4 feedback firmware fixture (PD13 input -> inverted PD12 output) plus reproducible Clang/LLD source/build recipe.
- Added `scripts/verify-r9.4-native.sh`, which starts a temporary Renode External Control server, builds the official Renode client/SharedSpice helpers, and runs the real Renode -> Verilator -> ngspice -> ADC -> Renode feedback closeout gate.
- The crate advances to `0.17.0` without adding a Rust dependency. R9.4 is considered closed only after the native closeout gate passes on a host with the Renode source tree and SharedSpice development library.

## 0.16.0 - R9.3 external backend participants

- Added `RenodeCoSimulationParticipant` with explicit GPIO input/output/bidirectional bindings, a conservative 1 ns scheduler quantum, exact virtual-time checks, and process isolation around Renode's official External Control client.
- Added `NgSpiceCoSimulationParticipant` with canonical-Circuit netlist compilation, SharedSpice `external` voltage-source inputs, persistent transient state, output-vector sampling, and process-isolated native callbacks.
- Added native Renode/ngspice helper sources plus `scripts/build-r9.3-native-helpers.sh`; helper/backend exits, malformed state, stale time origins, non-representable Renode timestamps, and non-finite analog values fail closed.
- Added focused R9.3 participant/protocol gates. The real firmware -> GPIO -> RTL -> analog -> input closed loop remains R9.4.


## [0.15.0] - 2026-09-10

### Added

- R9.2 incremental `DigitalCoSimulationParticipant` adapter that reuses the R3 four-state event queue and sequential state instead of replaying full simulations
- explicit `DigitalCoSimulationBinding` input/output-to-net contract with rejection of externally driven nets that already have internal digital drivers
- live `VerilatorCoSimulationParticipant` backed by one persistent generated C++ model process using Verilator `eval()`, `eventsPending()` and `nextTimeSlot()`
- minimal line protocol for same-time scalar input writes, monotonic time advancement and output snapshots without rebuilding/resetting RTL between scheduler steps
- real scheduler proof chaining the R3 digital clock -> stateful SystemVerilog `always_ff` -> R3 digital inverter on one deterministic co-simulation timeline
- R9.2 contract proofs for four-state digital boundaries, two-state RTL boundaries and invalid external-driver ownership

### Changed

- co-simulation documentation now distinguishes the completed master substrate (R9.1) from the completed Digital/Verilator native participants (R9.2)
- crate version advances to `0.15.0` with no new Rust dependency

### Deliberately deferred

- Renode external-control and ngspice shared-library incremental participants (R9.3)
- analog/digital closed-loop firmware + RTL proof (R9.4)
- Verilator co-simulation vectors and X/Z transport; R9.2 intentionally keeps the live RTL boundary scalar and deterministic two-state

## [0.14.0] - 2026-09-10

### Added

- R9.1 deterministic co-simulation scheduler substrate with a stable unsigned picosecond global timebase
- minimal `CoSimulationParticipant` incremental-session contract for solver-specific backend adapters
- explicit analog/digital participant ports and deterministic point-to-point scheduler links
- earliest-event/max-step macro scheduling independent of participant registration order
- bounded same-time delta-cycle propagation with fail-closed combinational oscillation detection
- existing `ExecutionControl` cancellation and wall-clock timeout checks across scheduler operations
- normalized digital transition and analog sample recording into the existing waveform/result model
- `AnalysisKind::CoSimulationTransient` and scheduler-specific deterministic run statistics
- R9 scheduler proofs for event-time preservation, registration-order independence, delta settling, oscillation limits, domain boundaries and cancellation
- runnable `cosim_chain` example and co-simulation architecture documentation

### Changed

- roadmap splits R9 into scheduler substrate, native incremental adapters, and final real closed-loop proof rather than coupling the master algorithm to one backend
- crate version advances to `0.14.0` without adding a new dependency

### Deliberately deferred

- native incremental Digital/Verilator sessions (R9.2)
- Renode external-control and ngspice shared-library sessions (R9.3)
- real firmware ↔ RTL ↔ mixed-signal closed-loop proof (R9.4)

## [0.13.0] - 2026-09-10

### Added

- R8 optional production runtime behind the `production` feature
- filesystem-durable queue with atomic claim/requeue/recovery semantics and bounded parallel workers
- retry of explicitly retryable engine failures with a bounded attempt budget
- SHA-256 reproducibility manifests covering request bytes, core version, engine descriptors, execution policy and executor/toolchain fingerprint
- content-addressed artifact and result-cache storage under SHA-256 namespaces
- `sim-worker` process protocol for isolated simulation execution
- Linux `bubblewrap` + `prlimit` executor with isolated PID/IPC/UTS/network namespaces, read-only system mounts and hard address-space/CPU/process/open-file/file-size limits
- R8 contract proofs for cache reuse, parallel execution, retries, crash recovery, artifact integrity and fail-closed limits
- Linux isolation proof for a real subprocess-backed digital simulation worker

### Changed

- production persistence uses standard filesystem atomicity rather than introducing Redis/SQLite as a mandatory core dependency
- hard-isolated workers reuse the existing simulator and execution-control contracts instead of implementing solver-specific process management again
- runtime artifact digests are validated before path materialization and isolated child processes are kill/wait guarded on early resource-limit exits
- roadmap advances to shared deterministic co-simulation scheduling and developer/debug tooling

### Deliberately deferred

- distributed/multi-host queue coordination and remote object storage; the R8 store is intentionally single-host filesystem durable
- target-specific hard-isolated Renode worker materialization; the generic runtime can host configured executors, while the stock isolated worker registers request-contained engines only
- authentication/TLS remain deployment-edge responsibilities rather than simulation-core semantics

## [0.12.0] - 2026-09-10

### Added

- R7 optional Axum/Tokio simulation service without moving solver semantics into the transport layer
- bounded asynchronous in-memory job lifecycle with opaque IDs and terminal normalized results
- REST health, engine discovery, submit/status/cancel endpoints
- WebSocket lifecycle/result snapshots for reconnect-friendly job observation
- service-owned request-body, retained-job, concurrency and existing `ExecutionPolicy` limits
- optional `sim-service` loopback binary registering generic Digital/ngspice/XSPICE/Mixed/Verilator engines
- R7 contract tests for lifecycle/result streaming, cooperative cancellation and fail-closed limits

### Changed

- `EngineRegistry` and `Simulator` are cheaply cloneable through their existing `Arc<dyn SimulationEngine>` storage so a service can share one configured registry safely
- full verification now compiles/tests all features and explicitly proves the R7 service contract
- roadmap advances to R8 production runtime

### Deliberately deferred

- durable queues, artifact stores, target-specific Renode job materialization, authentication/TLS deployment policy, distributed workers and hard OS CPU/memory/filesystem isolation remain R8
- progressive solver waveform streaming remains future work; R7 streams complete lifecycle snapshots and terminal normalized results

## [0.11.0] - 2026-09-10

### Added

- R6 MCU/Firmware Runtime as a thin Renode process adapter rather than a custom CPU/peripheral emulator
- immutable `FirmwareArtifact` support for ELF, Intel HEX and raw binary payloads
- `ComponentKind::mcu()` and `Analysis::FirmwareTransient` public contracts
- `RenodeTarget` platform/firmware binding outside `SimulationRequest` so executable bytes do not inflate Circuit IR
- output-only MCU GPIO bridge using Renode `Miscellaneous.LED` observers and exact virtual-time `RunFor` sampling
- normalization of sampled firmware GPIO state into the existing `DigitalWaveform` result model
- bounded Renode process execution using existing timeout, cancellation, input and log limits
- self-contained Cortex-M4/STM32F4 firmware fixture and real firmware→GPIO integration proof

### Changed

- full verification and CI now include a pinned Renode stable runtime
- roadmap advances to R7 Simulation Service API while preserving the future shared co-simulation scheduler boundary

### Fixed

- hardened the R6 Renode proof by reusing the STM32F4 Discovery platform-owned PD12 `UserLED` observer and a deterministic hold-high Cortex-M fixture

### Deliberately deferred

- MCU GPIO input/bidirectional synchronization, UART/SPI/I2C transaction bridges, and MCU ADC/DAC exposure; Renode already owns those peripherals and cross-engine exchange belongs to the shared deterministic scheduler

## [0.10.0] - 2026-09-10

### Added

- R5 Verilator-backed Verilog/SystemVerilog module execution without implementing an HDL parser or simulator in Rust
- `ModelKind::Module` with Verilog/SystemVerilog model languages and `ComponentKind::hdl_module()`
- scalar Circuit IR pin mappings to scalar/vector HDL ports, including deterministic parameter overrides
- request-aware engine selection so HDL requests route to Verilator without registration-order coupling
- deterministic clock/input SystemVerilog wrapper generation and VCD normalization into existing digital waveforms
- real Verilator proofs for plain Verilog combinational logic and clocked SystemVerilog RTL
- bounded Verilator build/runtime execution using the existing cancellation, timeout, input/output, and log limits

### Changed

- XSPICE VCD parsing now preserves multiple references that share one VCD identifier, allowing wrapper aliases to normalize correctly
- canonical CI/full verification now requires Verilator in addition to ngspice/XSPICE
- roadmap advances to R6 MCU/Firmware Runtime with Renode as the preferred primary backend

### Deliberately deferred

- closed-loop analog↔RTL co-simulation; this belongs to the shared deterministic scheduler used by Verilator, mixed-signal, and MCU runtimes rather than a second hand-written R5 timestep loop

## [0.9.0] - 2026-09-10

### Added

- complete R4 mixed-signal subsystem with explicit `adc_bridge` and `dac_bridge` Circuit IR kinds
- bridge validation for pin domains/directions, threshold windows, voltage windows, delay/slew, and input-load parameters
- `MixedSignalEngine` backed by one coordinated ngspice/XSPICE transient process
- mixed analog + digital probing into one normalized `SimulationResult`
- reuse of the existing ngspice analog/model compiler inside mixed runs, including semiconductor and subcircuit support
- reuse of the XSPICE-native digital gate, tri-state, latch, and flip-flop lowering path
- mixed-signal round-trip example and real ngspice/XSPICE integration proof

### Changed

- direct analog/digital net joins now fail validation even when a legacy mixed-domain pin is present; R4 domain crossing is component-explicit
- roadmap advances to R5 Verilator-backed Verilog/SystemVerilog integration

## [0.8.0] - 2026-09-09

### Added

- complete R3 digital subsystem in the pure-Rust `DigitalEngine`
- D/SR latches and D/JK/T/SR flip-flops with edge selection, async set/reset, initial state, and propagation delay
- driver-aware digital nets with tri-state release, deterministic same-time resolution, and four-state contention semantics
- public `LogicVector` bus helper with stable up-to-64-bit conversion
- canonical `mux2`, `demux2`, `decoder2_to_4`, `register`, and `counter` primitives
- width-aware register/counter state on the same event-driven execution/runtime limits
- native XSPICE mappings for `d_tristate`, `d_dff`, `d_jkff`, `d_srff`, and `d_dlatch`
- full-R3 regression suite covering sequential state, buses, contention, selection logic, registers, and counters
- XSPICE parity proofs for tri-state resolution and D flip-flop behavior

### Changed

- multiple digital drivers are now resolved instead of rejected; validation emits a warning
- digital event processing commits same-time driver events as a deterministic bucket before dependent evaluation
- R3 is now considered complete; subsequent simulation work moves to R4 mixed-signal bridges


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
