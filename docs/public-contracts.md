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
Pure digital nets currently reject multiple output/bidirectional drivers; resolved multi-driver/tri-state buses are deferred to a later digital milestone.
`XSpiceEngine` is the external ngspice/XSPICE backend. It consumes the same Circuit IR and emits the same `DigitalWaveform`. Parity covers the native XSPICE subset: combinational gates, tri-state, D/JK/T/SR flip-flop mappings, and D latch. Higher-level mux/demux/decoder/register/counter behavior remains defined by the solver-independent `DigitalEngine`. XSPICE minimum-delay and ZERO-at-start differences remain explicit diagnostics rather than hidden normalization.


## Complete R3 digital contract

`LogicVector` is the canonical width-aware helper for 1..=64 scalar digital bits. Digital nets are
driver-aware: `Z` releases a net, equal known drivers agree, conflicting known drivers resolve to
`X`, and an active unknown driver resolves the net to `X`.

Sequential primitives share deterministic event semantics, async set/reset handling, selectable
rising/falling clock edges in the reference engine, and the same execution-control limits as all
other simulation work.
