# Deterministic co-simulation scheduler

R9.1 introduces the solver-independent master-algorithm substrate used to coordinate incremental
simulation backends. It intentionally does not reimplement Renode, Verilator, ngspice/XSPICE, or the
built-in digital engine. Those engines remain domain authorities; the scheduler owns only shared time,
causal exchange, deterministic ordering, bounded settling, execution control, and normalized output.

## Integer timebase

`CoSimulationTime` is an unsigned picosecond tick count (`10^12` ticks/second). External backends may
use finer internal resolution, but values crossing the scheduler boundary are quantized explicitly to
this stable timebase. This avoids cumulative floating-point drift in repeated cross-engine stepping.

## Participant contract

A `CoSimulationParticipant` is one initialized incremental backend session. It exposes:

- a stable participant id;
- explicitly analog or digital input/output/bidirectional ports;
- its current scheduler time;
- the time of its next internal event when known;
- `advance_to(target)` for monotonic macro-step advancement;
- same-time input writes;
- output reads.

`write_input` must settle immediately causal outputs at the current scheduler time before returning.
That contract lets the master iterate delta cycles without embedding solver-specific logic.

The participant surface is deliberately small enough for thin native adapters:

- built-in digital engine: event-queue session;
- Verilator: generated model `eval`/`eventsPending`/`nextTimeSlot` session;
- Renode: external-control virtual-time session;
- ngspice/XSPICE: shared-library controlled transient session.

## Links and domain ownership

`CoSimulationLink` connects one source port to one target port. Links are sorted before execution and
participant storage is a `BTreeMap`, so registration order cannot affect execution order.

The scheduler does not silently convert domains. Analog-to-digital or digital-to-analog links fail with
`cosim_domain_mismatch`; conversion must be an explicit ADC/DAC/logic bridge participant. Analog units
must also match. A target port currently accepts one scheduler driver; multi-driver resolution belongs
inside an explicit participant so R3 four-state semantics are not duplicated in the master.

## Macro steps and delta cycles

At each iteration the scheduler advances to the earliest of:

1. `current + max_step`;
2. the next event reported by any participant;
3. the configured stop time.

All participants advance to that exact time in deterministic id order. Outputs are then routed to inputs
as a synchronous delivery set. If an input changes, another same-time delta cycle is evaluated until no
link changes or `max_delta_cycles` is exceeded. Non-settling combinational feedback fails closed with
`cosim_delta_cycle_limit` rather than hanging.

`max_steps` independently bounds macro-step growth. Existing `ExecutionControl` cancellation and
wall-clock timeout are checked between scheduler operations and passed through to participants.

## Result model

Every output/bidirectional participant port is recorded into the existing normalized waveform model:

- digital outputs become transition-collapsed `DigitalWaveform` values;
- analog outputs become time-axis `AnalogWaveform` samples with their declared unit.

The result uses `AnalysisKind::CoSimulationTransient` and engine id `co-simulation`. Scheduler-specific
counts are returned in `CoSimulationReport::scheduler`.

## R9 boundary

R9.1 is the deterministic scheduler substrate. R9.2 now provides the first native incremental adapters while
keeping the master algorithm backend-independent:

- `DigitalCoSimulationParticipant` reuses the R3 event queue, four-state net resolution and sequential runtime state;
- `VerilatorCoSimulationParticipant` keeps one Verilated model alive across scheduler macro-steps and drives it through `eval()`, `eventsPending()` and `nextTimeSlot()`;
- `RenodeCoSimulationParticipant` connects through a process-isolated helper linked to Renode's official External Control client. GPIO input/output/bidirectional bindings use the shared scheduler clock; targets must be representable at Renode's 1 ns boundary.
- `NgSpiceCoSimulationParticipant` keeps one SharedSpice transient session alive in a process-isolated helper, drives `external` voltage sources, and samples configured voltage vectors without restarting ngspice between macro-steps;
- R9.3 helper crashes, stale Renode time origins, non-representable Renode times, malformed protocol state, non-finite analog values, and backend time mismatches fail closed;
- R9.4 adds explicit `DacCoSimulationParticipant` and Schmitt-style `AdcCoSimulationParticipant` boundaries so the master never performs hidden domain conversion. The portable proof closes Renode's External Control protocol -> live Verilator -> SharedSpice protocol -> ADC feedback on one deterministic timeline.
- `scripts/verify-r9.4-native.sh` is the native closeout gate: it starts a temporary Renode External Control server with a real Cortex-M4 feedback firmware, builds the official Renode/SharedSpice helpers, runs live Verilator, and requires the returned analog/ADC signal to change firmware GPIO behavior.

The R9.2 Verilator boundary is intentionally scalar digital and deterministic two-state. X/Z semantics stay in the R3 participant, and vectors remain owned by the existing R5 batch HDL path until a later adapter needs explicit vector transport.


## R9.3 native helper boundary

The two native backends deliberately do not live inside the Rust scheduler process. `scripts/build-r9.3-native-helpers.sh` builds:

- `ontologyx-renode-cosim-helper` against Renode's official `tools/external_control_client` static library;
- `ontologyx-ngspice-cosim-helper` against the installed SharedSpice library.

The helpers use a tiny line protocol (`STATE`, `SET`, `ADV`, `QUIT`) over inherited pipes. This keeps the public Rust API backend-neutral and makes a helper/native backend termination observable as an `EngineError` instead of terminating the service process.

Renode externally visible scheduler time is restricted to whole nanoseconds. The participant reports a default 1 ns next-event quantum so the correctness-first R9 master never skips an observable GPIO boundary. A later performance pass may replace this conservative quantum with event-aware wakeups without changing the participant API.

SharedSpice inputs are zero-order-held external voltage sources. A write at a scheduler boundary changes the source value used by subsequent transient integration. At time zero the helper recomputes the operating point so initial direct feed-through settles before the first macro-step. Once transient integration has started, a changed external source is observed on the next bounded integration advance rather than by silently restarting ngspice; R9.4's closed-loop proof makes that scheduler-boundary latency explicit.

## R9.4 explicit cross-domain bridges

`DacCoSimulationParticipant` exposes a digital input and voltage output. It maps known zero/one to configured low/high voltages and rejects X/Z so four-state resolution stays owned by the digital domain.

`AdcCoSimulationParticipant` exposes a voltage input and digital output. Values below the low threshold produce zero, values above the high threshold produce one, and the region between thresholds retains the previous known output. This Schmitt behavior makes same-time feedback deterministic without embedding conversion rules in the scheduler.

The R9.4 native firmware fixture configures STM32F4 PD12 as output and PD13 as input, then continuously drives PD12 to the inverse of PD13. The closeout chain is therefore observable in both directions: firmware state drives RTL/analog, the ADC result returns to PD13, and firmware changes PD12 in response.
