# Architecture

OntologyX Sim Core owns electrical truth and simulation semantics.

```text
consumer
   |
   v
Circuit IR / validation
   |
   v
engine registry
   |
   +--> ngspice process adapter
   +--> built-in Rust digital event engine
   +--> XSPICE digital adapter
   +--> mixed-signal ngspice/XSPICE adapter
   +--> Verilator HDL process adapter
   +--> Renode MCU/firmware process adapter
   |
   v
normalized simulation result
```

With the optional `service` feature, an Axum/Tokio job boundary sits above `Simulator`; it does not alter Circuit IR or engine semantics.

The repository is deliberately Rust-only. Language bindings and OXSim product integration are downstream adapters.
Engine adapters must not leak solver-specific netlist syntax into the public Circuit IR.


## Process execution boundary

The ngspice adapter runs the solver as a child process instead of linking solver state into
the host process. `ExecutionControl` provides timeout, cancellation and bounded solver
input/output. A child-process guard kills and reaps an unfinished solver on every controlled
error path before the temporary run directory is removed. This boundary is intentionally
portable and does not claim OS-level memory/CPU isolation.


## Digital engine layering

`DigitalEngine` is the solver-independent reference implementation for four-state event semantics.
`XSpiceEngine` is an external adapter over ngspice XSPICE. Both consume the same Circuit IR and
produce the same normalized `DigitalWaveform` result shape; parity tests guard the overlap.


## Mixed-signal engine layering

`MixedSignalEngine` reuses the analog ngspice compiler and the XSPICE-native digital lowering
path, then inserts explicit ADC/DAC node bridges. Analog samples and digital events are exported
from the same solver run and normalized into one `SimulationResult`. Sim Core does not perform
a second external timestep loop, avoiding split-brain analog/digital scheduling.


## Verilator HDL layering

`VerilatorEngine` is deliberately thin. Circuit IR `hdl_module` instances reference inline
Verilog/SystemVerilog `ModelDefinition::Module` sources. Sim Core generates only a deterministic
SystemVerilog wrapper/testbench for circuit nets, constants, clocks, module instances and VCD
probes. Verilator owns HDL parsing, elaboration, timing, C++ generation, compilation and runtime.
The resulting VCD is normalized through the same digital waveform parser used by the XSPICE path.

`SimulationEngine::supports_request` is the request-aware extension point that allows multiple
digital backends to share `DigitalTransient` without registration-order coupling. This hook is
intended to be reused by future MCU/firmware backends.

R5 does not claim closed-loop analog↔RTL co-simulation. That requires a shared deterministic
scheduler across Verilator, mixed-signal and MCU runtimes and is intentionally kept out of the
minimal HDL adapter.


## Renode firmware layering

`RenodeEngine` follows the same thin-adapter rule as R5. A `RenodeTarget` binds one `mcu` component
to an immutable firmware artifact and a Renode platform description. Renode owns CPU emulation,
firmware loading and peripheral behavior. Sim Core adds only deterministic process control,
virtual-time stepping, GPIO observation, and normalization into the existing digital waveform model.

R6 intentionally does not create a second cross-engine scheduler. Bidirectional GPIO and closed-loop
Renode↔Verilator↔mixed-signal exchange remain the responsibility of the future shared deterministic
co-simulation scheduler.


## R7 service layering

`SimulationService` is an optional adapter above `Simulator`. HTTP handlers enqueue bounded in-memory jobs, a Tokio semaphore limits simultaneous blocking simulation calls, and `spawn_blocking` keeps synchronous engines off the async reactor. DELETE reuses the existing cooperative `CancellationToken`; WebSocket clients receive full lifecycle snapshots and the terminal normalized result/error.

The service deliberately does not implement a durable queue, artifact store, authentication/TLS policy, process sandbox, distributed scheduling or hard CPU/memory isolation. Those are deployment/production-runtime responsibilities in R8.


## R9 deterministic co-simulation layering

`CoSimulationScheduler` is the solver-independent master algorithm. It does not parse HDL, emulate a
CPU, solve analog equations, or duplicate the R3 event engine. Each backend contributes an incremental
`CoSimulationParticipant` session and keeps ownership of its internal state. The scheduler owns only
integer global time, deterministic ordering, bounded advancement, same-time causal exchange, execution
control, and normalized output recording. Domain conversion stays explicit through bridge participants.

R9 is intentionally staged: R9.1 stabilizes the master contract; R9.2 binds the built-in Digital runtime and
Verilator through native incremental sessions; R9.3 adds Renode and ngspice incremental control; R9.4
closes the loop with a real firmware/RTL/mixed-signal integration proof.
