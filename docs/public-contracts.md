# Public contracts

Sim Core keeps solver-independent contracts stable so OXSim, services, CLIs and
future language adapters do not need to understand ngspice-specific behavior.

## Versioned schemas

Two versions are intentionally separate:

- `CIRCUIT_SCHEMA_VERSION` identifies the serialized Circuit IR.
- `SIMULATION_RESULT_SCHEMA_VERSION` identifies normalized result envelopes.

A crate release may evolve without forcing either serialized schema to change.
Schema constants only change when their serialized contract changes.

## Engine descriptors

Every `SimulationEngine` exposes an `EngineDescriptor` containing:

- stable engine id
- optional engine version
- analog/digital/mixed-signal capabilities
- supported analysis kinds

`Simulator::engine_descriptors()` is the canonical discovery surface for API
adapters and product capability negotiation.

## Simulation results

`SimulationResult::new(...)` is the canonical constructor. It stamps every
normalized result with deterministic provenance:

- result schema version
- circuit schema version used by the request
- Sim Core crate version
- solver/engine version when available

It also derives deterministic size statistics:

- waveform count
- point/transition count

Wall-clock timings and timestamps are deliberately excluded from this envelope;
they are execution telemetry, not reproducibility metadata.

## Errors

`SimulationError::code()` exposes stable top-level codes:

- `invalid_circuit`
- `no_compatible_engine`
- `engine_error`

Engine-specific failures retain their stable `EngineError.code` alongside a
human-readable message and retryability bit. R2.2 execution controls use stable
engine-level codes without changing the outer `SimulationError` envelope:

- `execution_cancelled`
- `execution_timeout`
- `execution_resource_limit`
- `execution_policy_invalid`

`EngineError::is_cancelled()`, `is_timeout()` and `is_resource_limit()` are
convenience classifiers for adapters.

## Diagnostics

Diagnostics are normalized as severity + machine-readable code + message.
Engine adapters should preserve deterministic ordering and avoid embedding
non-deterministic host paths in normalized diagnostic messages when possible.

## Compatibility rule

Before 1.0 the Rust API may still evolve, but serialized schema changes must be
explicit and covered by round-trip/golden contract tests. Digital, mixed-signal
and HDL engines must use the same envelopes rather than introducing parallel
result types.


## Execution control

Execution policy is intentionally separate from serialized `SimulationRequest`, so runtime
limits do not alter the Circuit IR or result schemas. `ExecutionControl` combines:

- a bounded `ExecutionPolicy`
- a cloneable `CancellationToken`

The default policy currently enforces a 60-second wall-clock timeout, 8 MiB generated
solver-input limit, 64 MiB solver-output limit, and 4 MiB solver-log limit. `0` disables an
individual limit; `ExecutionPolicy::unbounded()` disables all size/time limits explicitly.

Process-backed engines override `simulate_with_control`. The ngspice adapter polls the
child process, cancellation token and output-file sizes, and uses an RAII child guard so
error, timeout, cancellation and resource-limit exits kill and reap the child before its
temporary run directory is removed.

These are portable execution/output limits. OS-level hard memory/CPU sandboxing is not
claimed by this layer and remains a worker/runtime concern.


## Digital event contract

R3 defines solver-independent four-state digital semantics using `LogicValue::{Zero, One, X, Z}`.
The built-in `DigitalEngine` is the reference implementation for `Analysis::DigitalTransient`;
it emits event-based `DigitalWaveform` transitions instead of sampled boolean arrays.

Canonical R3 digital components use explicit pin contracts:

- `logic_input(out)`
- `digital_clock(out)`
- `logic_output(in)`
- `buffer(in, out)` and `not_gate(in, out)`
- binary gates `and_gate`, `or_gate`, `xor_gate`, `nand_gate`, `nor_gate`, `xnor_gate` with `a`, `b`, `out`

Gate `delay` and clock `period` are seconds (`Unit::Second`). Clock `duty_cycle` is dimensionless.
Pure digital nets support multiple output/bidirectional drivers using the R3 four-state resolution rules; tri-state `Z` releases a driver and conflicting active values resolve to `X`.
`XSpiceEngine` is the external ngspice/XSPICE backend. It consumes the same Circuit IR and emits the same `DigitalWaveform`. Parity covers the native XSPICE subset: combinational gates, tri-state, D/JK/T/SR flip-flop mappings, and D latch. Higher-level mux/demux/decoder/register/counter behavior remains defined by the solver-independent `DigitalEngine`. XSPICE minimum-delay and ZERO-at-start differences remain explicit diagnostics rather than hidden normalization.


## Complete R3 digital contract

`LogicVector` is the canonical width-aware helper for 1..=64 scalar digital bits. Digital nets are
driver-aware: `Z` releases a net, equal known drivers agree, conflicting known drivers resolve to
`X`, and an active unknown driver resolves the net to `X`.

Sequential primitives share deterministic event semantics, async set/reset handling, selectable
rising/falling clock edges in the reference engine, and the same execution-control limits as all
other simulation work.


## R4 mixed-signal contract

- `ComponentKind::adc_bridge()` and `ComponentKind::dac_bridge()` are stable IR kinds.
- domain crossing is explicit: analog and digital pins do not share a net.
- `MixedSignalEngine` advertises only `MixedSignalTransient` in the mixed domain.
- mixed runs preserve the existing process timeout/cancellation/resource-limit contract.
- normalized mixed results contain the existing `Waveform::Analog` and `Waveform::Digital`
  variants; no solver-specific waveform type leaks into the public result schema.


## R5 HDL / Verilator contract

- `ModelKind::Module` with `ModelLanguage::Verilog` or `ModelLanguage::SystemVerilog` owns inline RTL source.
- `ComponentKind::hdl_module()` references a module model through its `model` parameter.
- existing scalar Circuit IR pins map to HDL ports through `Pin.name`: `port` for scalar ports and `port[bit]` for vector bits. Vector mappings must be contiguous from bit 0.
- R5 HDL pins are digital Input/Output only. Bidirectional/inout ownership is deferred to the shared co-simulation boundary.
- `VerilatorEngine` consumes `Analysis::DigitalTransient`, produces existing `Waveform::Digital` results, and does not introduce an HDL-specific result envelope.
- engine selection is request-aware through `SimulationEngine::supports_request`; canonical R3/XSPICE engines refuse requests containing `hdl_module`, so HDL routing does not depend on registration order.
- R5 Verilator execution is deterministic two-state execution. Canonical X/Z semantics remain owned by `DigitalEngine`; known 0/1 is required at the Verilator boundary.
- process timeout, cancellation, input/result/log limits and child cleanup remain under `ExecutionControl`. Generated compiler artifacts are ephemeral but hard OS disk/memory/CPU isolation is deferred to the production worker runtime.
- inline module admission rejects host/testbench facilities (`include`, DPI, `$system`, file I/O, dump/control tasks). This reduces accidental host coupling but is not a security sandbox.


## R6 MCU / firmware contract

- `FirmwareArtifact` carries immutable ELF/Intel HEX/raw binary bytes without introducing an executable parser into Sim Core. Raw binary additionally requires a load address.
- `ComponentKind::mcu()` is the Circuit IR boundary for executable MCU devices. R6 pins are digital Output only and map through `Pin.name` as `renodePeripheral@pin`.
- `RenodeTarget` owns platform selection, firmware payload and optional CPU/entry-point configuration outside `SimulationRequest`.
- `RenodeEngine` owns only `Analysis::FirmwareTransient` for its configured MCU component and produces existing `Waveform::Digital` results.
- Renode owns CPU emulation, SoC/peripheral models and firmware loading. Sim Core owns adapter validation, process execution controls, virtual-time sample scheduling, diagnostics and waveform normalization.
- R6 GPIO is sampled at the requested virtual-time step and capped by `MAX_RENODE_SAMPLE_POINTS`; this is not claimed to be an instruction-level edge trace.
- MCU input/bidirectional pins and closed-loop Renode↔Verilator↔mixed-signal exchange are deliberately deferred to the shared deterministic co-simulation scheduler.


## R7 Simulation Service contract

- the service surface is optional behind the `service` Cargo feature; core simulation users do not need Axum/Tokio dependencies.
- `POST /v1/simulations` creates an opaque in-memory job and returns a queued snapshot.
- job states are `queued`, `running`, `cancelling`, `succeeded`, `failed`, and `cancelled`.
- `GET /v1/simulations/{id}` returns the latest complete snapshot; terminal success embeds the existing `SimulationResult`.
- `DELETE /v1/simulations/{id}` is idempotent and reuses `CancellationToken`.
- `GET /v1/simulations/{id}/events` streams complete `SimulationJobEvent` snapshots over WebSocket and closes after a terminal event.
- service execution policy is configured by the host and cannot be weakened by a request payload.
- request body size, retained job count and concurrent blocking jobs are bounded by `ServiceLimits`.
- R7 uses `spawn_blocking` to protect the Tokio reactor but does not claim OS-level worker isolation or durable execution; those remain R8 concerns.

## R8 Production Runtime contract

- the production runtime is optional behind the `production` Cargo feature; ordinary simulation-core consumers do not pull queue/cache dependencies.
- `ProductionRuntime` persists job snapshots as one-file state records and uses same-filesystem rename for queue → running → terminal claims. On open, stale running records are recovered back to the queue.
- `max_parallel_workers`, queue retention, retry attempts and the existing `ExecutionPolicy` are host-owned and fail closed when invalid.
- retries occur only for simulation-engine failures explicitly marked retryable and are bounded by `max_attempts`.
- `ReproducibilityManifest` hashes the serialized request plus core version, engine descriptors, execution policy and executor/toolchain fingerprint; the resulting digest is also the result-cache identity.
- `ContentAddressedStore` stores immutable bytes by lowercase SHA-256 and verifies both digest and length on read; externally supplied digest strings are validated before becoming filesystem paths.
- `IsolatedProcessExecutor` is Linux-specific production hardening built from existing OS tools: bubblewrap provides fresh user/PID/IPC/UTS/network namespaces and a minimal mount view, while `prlimit` applies address-space, CPU-time, process-count, open-file and file-size ceilings.
- isolated worker input/output/log sizes and wall-clock cancellation/timeout continue to use the existing `ExecutionPolicy`; child cleanup is guarded even when an early resource-limit error occurs.
- the stock `sim-worker` intentionally contains no queue logic and no network listener. It accepts one request envelope and writes one response envelope, keeping scheduling and isolation ownership outside the simulation engines.
- R8 is a single-host durable runtime, not a distributed queue. Multi-host coordination/object storage are future deployment/runtime concerns.



## R9 co-simulation scheduler and adapter contract

- Scheduler-visible time is integer picoseconds through `CoSimulationTime`; floating-point seconds are conversion helpers only.
- Participant ids and port names are stable strings; execution order is deterministic and independent of registration order.
- Scheduler ports are explicitly analog or digital. Analog ports declare units; digital ports do not.
- Cross-domain conversion is never implicit: analog↔digital links require an explicit bridge participant.
- Each target port has at most one scheduler link. Driver resolution remains a participant/domain responsibility.
- `next_event_time` must be strictly later than the participant current time.
- `advance_to` must end exactly at the requested scheduler time.
- Same-time `write_input` must not advance participant time and must settle immediately causal output changes before returning.
- Delta-cycle and macro-step limits fail closed rather than allowing non-terminating feedback.
- Existing cancellation and timeout controls remain authoritative.
- R9.1 owns the master algorithm. R9.2 adds `DigitalCoSimulationParticipant` and `VerilatorCoSimulationParticipant` without moving domain semantics into the scheduler.
- Digital adapter bindings map scheduler ports to existing R3 nets; an externally writable net may not already have an internal driver.
- Verilator adapter ports are scalar digital input/output ports and accept only known zero/one values; X/Z stays owned by the R3 participant.
- Verilator model state persists across scheduler advances in one generated process; it is not rebuilt or replayed per macro-step.
- Renode/ngspice incremental adapters and the real closed-loop proof remain R9.3/R9.4.
