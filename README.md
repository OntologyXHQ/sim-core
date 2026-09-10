# OntologyX Sim Core

`ontologyx-sim-core` is the Rust electrical-simulation engine used by OXSim.
It owns the solver-independent Circuit IR, validation rules, analyses, model registry,
engine abstraction, ngspice adapter, and normalized simulation results.

This repository intentionally contains **no Node.js binding, UI, OXFlow source, application shell, or product code**.

## Current capabilities

- operating-point analysis
- DC sweep
- transient analysis
- complex AC sweep
- resistor, capacitor, inductor, voltage/current source, and ground
- SPICE `.model` devices: diode, BJT, four-terminal MOSFET
- SPICE `.subckt` devices: op-amp and generic subcircuit
- typed SI parameters
- deterministic circuit validation and netlist generation
- safe inline SPICE model validation
- normalized scalar/time/DC/frequency waveforms
- explicit contracts for future digital and mixed-signal engines
- versioned normalized result metadata and deterministic result statistics
- discoverable engine descriptors with capabilities and solver version
- bounded execution policy with timeout, cancellation, input/output/log limits and child cleanup
- built-in four-state digital event engine with constants, clocks, combinational gates and propagation delay
- ngspice/XSPICE digital adapter with VCD-normalized event waveforms and reference-engine parity proofs
- sequential digital primitives: D/SR latches and D/JK/T/SR flip-flops
- tri-state outputs, four-state multi-driver resolution, and width-aware logic vectors
- mux/demux/decoder, registers, and counters as canonical bus-oriented digital primitives
- explicit ADC/DAC bridge components for analog/digital domain crossings
- coordinated ngspice/XSPICE mixed-signal transient simulation with analog + digital waveforms in one result
- Verilator-backed inline Verilog/SystemVerilog module blocks with clock/reset timing and VCD-normalized digital waveforms
- Renode-backed MCU/firmware execution with ELF/HEX/BIN artifacts, virtual-time GPIO sampling, and normalized digital waveforms
- deterministic R9 co-simulation scheduler with integer picosecond time, bounded delta-cycle settling, plus live built-in Digital and stateful Verilator participants
- optional Axum service and filesystem-durable production runtime with isolated workers and reproducibility-aware caching

## Install

```toml
[dependencies]
ontologyx-sim-core = "0.15"
```

Real analog/mixed simulation requires `ngspice` on `PATH`; R5 HDL simulation requires `verilator` plus its C++/GNU Make build toolchain; R6 MCU/firmware simulation requires `renode`.

```bash
ngspice --version
verilator --version
renode -v
```

## Minimal Rust example

```rust
use ontologyx_sim_core::{
    validate_circuit, Circuit, Component, ComponentKind, Net, NetEndpoint, Pin,
    PinDirection, SignalDomain,
};

fn analog_pin(id: &str) -> Pin {
    Pin::new(id, id, SignalDomain::Analog, PinDirection::Passive)
}

let circuit = Circuit::new()
    .with_component(
        Component::new("r1", ComponentKind::resistor())
            .with_pin(analog_pin("a"))
            .with_pin(analog_pin("b")),
    )
    .with_component(
        Component::new("c1", ComponentKind::capacitor())
            .with_pin(analog_pin("a"))
            .with_pin(analog_pin("b")),
    )
    .with_net(
        Net::new("n1")
            .connect(NetEndpoint::new("r1", "b"))
            .connect(NetEndpoint::new("c1", "a")),
    );

assert!(validate_circuit(&circuit).is_valid());
```

For a complete real-ngspice example, see [`examples/voltage_divider.rs`](examples/voltage_divider.rs).

## Controlled execution

Runtime limits are kept outside the serialized circuit/request schema:

```rust
use ontologyx_sim_core::{
    CancellationToken, ExecutionControl, ExecutionPolicy, Simulator,
};

let cancellation = CancellationToken::new();
let control = ExecutionControl::new(ExecutionPolicy {
    timeout_ms: 5_000,
    ..ExecutionPolicy::default()
})
.with_cancellation(cancellation.clone());

// From another thread/task:
// cancellation.cancel();

let result = simulator.simulate_with_control(&request, &control)?;
# Ok::<(), ontologyx_sim_core::SimulationError>(())
```

The default policy is bounded. Trusted offline callers can opt out explicitly with
`ExecutionPolicy::unbounded()`. Sim Core currently bounds wall-clock runtime and solver
input/output sizes; hard OS memory/CPU sandboxing belongs to the worker/runtime layer.


## XSPICE digital backend

`XSpiceEngine` compiles the same digital Circuit IR used by the built-in `DigitalEngine`
into ngspice XSPICE `d_source`, `d_buffer`, `d_inverter`, `d_and`, `d_or`, `d_xor`,
`d_nand`, `d_nor`, and `d_xnor` code models. Event-node output is exported through VCD
and normalized back into `DigitalWaveform`.

```rust
use ontologyx_sim_core::{SimulationEngine, XSpiceEngine};

let engine = XSpiceEngine::default();
let info = engine.info();
assert!(info.available && info.xspice_available);

let result = engine.simulate(&request)?;
# Ok::<(), ontologyx_sim_core::EngineError>(())
```

XSPICE basic digital gates impose a minimum representable rise/fall delay. Sim Core exposes
this as `XSPICE_MIN_DELAY_SECONDS` and emits an `xspice_delay_floor` diagnostic when a
smaller requested gate delay is clamped. The built-in `DigitalEngine` remains the semantic
reference and supports true zero-delay events.

R3 is complete in the built-in engine: sequential storage, tri-state/multi-driver resolution,
logic vectors up to 64 bits, mux/demux/decoder, registers, and counters all share the same
event queue and normalized waveform model. The XSPICE adapter provides parity for the native
XSPICE subset (`d_tristate`, `d_dff`, `d_jkff`, `d_srff`, `d_dlatch`) plus the combinational
gate set; higher-level bus primitives remain solver-independent reference-engine constructs.

## Mixed-signal backend

`MixedSignalEngine` runs `Analysis::MixedSignalTransient` as one coordinated ngspice/XSPICE
transient. `adc_bridge` and `dac_bridge` are explicit Circuit IR components; analog and digital
pins never share a net directly. A single normalized result can contain both analog sampled
waveforms and digital event transitions. See [`docs/mixed-signal.md`](docs/mixed-signal.md).

## Verilog/SystemVerilog backend

`VerilatorEngine` keeps R5 intentionally thin: inline Verilog/SystemVerilog remains in the existing
model registry, `hdl_module` pins map Circuit IR endpoints to HDL ports, and sim-core generates only
a deterministic SystemVerilog testbench wrapper. Verilator owns HDL parsing, elaboration, timing,
C++ generation, compilation, and execution. VCD output is normalized into the same
`DigitalWaveform` contract used by the other digital backends.

```rust
use ontologyx_sim_core::{ModelDefinition, VerilatorEngine};

let rtl = ModelDefinition::system_verilog_module(
    "counter",
    "counter",
    "module counter(input logic clk, output logic q); always_ff @(posedge clk) q <= ~q; endmodule",
);
let engine = VerilatorEngine::default();
# let _ = (rtl, engine);
```

R5 deliberately uses deterministic two-state Verilator execution. Four-state X/Z semantics remain
owned by the R3 reference engine. Closed-loop analog↔RTL scheduling is deferred to the shared
co-simulation scheduler rather than implemented as a second custom timestep loop. See
[`docs/verilator.md`](docs/verilator.md).

## MCU/firmware backend

`RenodeEngine` keeps R6 minimal: Renode owns CPU instruction execution, SoC/peripheral models and
ELF/HEX/BIN loading. Sim Core configures a `RenodeTarget`, advances Renode in virtual time, observes
explicit MCU GPIO outputs, and normalizes them into existing `DigitalWaveform` results. No CPU emulator
or executable parser is implemented in this crate. See [`docs/firmware.md`](docs/firmware.md).

## Simulation service API

R7 is optional behind the `service` Cargo feature. `SimulationService` wraps a configured `Simulator` with bounded asynchronous jobs, REST lifecycle endpoints, cooperative cancellation and WebSocket lifecycle/result streaming. Axum owns HTTP/WebSocket transport and Tokio owns async scheduling; sim-core does not implement a custom web runtime or queue framework.

```bash
cargo run --features service --bin sim-service
```

The default binary binds only to `127.0.0.1:4000`. See [`docs/service-api.md`](docs/service-api.md).

## Production runtime

R8 is optional behind the `production` Cargo feature. `ProductionRuntime` adds a durable single-host queue, bounded parallel workers, retry/recovery, reproducibility manifests, content-addressed artifacts and result caching. The generic runtime accepts any `RuntimeExecutor`; `SimulatorExecutor` runs a configured simulator in-process, while `IsolatedProcessExecutor` delegates hard Linux sandboxing to `bubblewrap` and resource ceilings to `prlimit`.

```bash
cargo test --features production --test r8_complete
```

The stock `sim-worker` binary keeps the worker protocol tiny and registers the request-contained Digital/ngspice/XSPICE/Mixed/Verilator engines. Renode remains target-specific because its firmware/platform binding lives outside `SimulationRequest`; production hosts can provide a configured executor without moving firmware bytes into Circuit IR. See [`docs/production-runtime.md`](docs/production-runtime.md).

## Repository contract

Create a clean source-state snapshot with:

```bash
cargo snapshot
```

The archive is written to `~/Downloads` and excludes Git metadata and generated build output.

Run the full release-readiness gate with:

```bash
./scripts/verify.sh
```

It checks formatting, clippy with warnings denied, all Rust tests including the optional R7 service and R8 production features, real ngspice/XSPICE, Verilator and Renode integration,
the runnable examples, and a Cargo package dry-run.

## Architecture

```text
Rust consumer / OXSim backend
            |
            v
Circuit IR + validation
            |
            v
Simulation engine contract
            |
       +----+-------------------------------+----------------+----------------+
       |                     |                 |                |                |
       v                     v                 v                v                v
 DigitalEngine          NgSpiceEngine   MixedSignalEngine  VerilatorEngine  RenodeEngine
 (in-process)                |                 |                |                |
       |                 isolated ngspice      |         Verilator process  Renode process
       |                     |          ngspice + XSPICE       |         virtual-time GPIO
       +---------+-----------+                 |          VCD normalization      |
                 |                             |                |                |
                 +---- XSpiceEngine -----------+----------------+----------------+
                 |     event nodes / VCD
                 |
                 v
      normalized results + diagnostics
```

Solver-specific syntax stays behind engine adapters. OXSim and OXFlow consume this
engine; they do not own electrical truth.

## Documentation

- [Getting started](docs/getting-started.md)
- [Circuit IR](docs/circuit-ir.md)
- [Analyses](docs/analyses.md)
- [SPICE models](docs/spice-models.md)
- [Architecture](docs/architecture.md)
- [Compatibility](docs/compatibility.md)
- [Public contracts](docs/public-contracts.md)
- [Digital simulation](docs/digital.md)
- [Mixed-signal simulation](docs/mixed-signal.md)
- [Verilog/SystemVerilog with Verilator](docs/verilator.md)
- [MCU/firmware runtime with Renode](docs/firmware.md)
- [Simulation service API](docs/service-api.md)
- [Roadmap](docs/roadmap.md)

## Related repositories

- OXSim product: https://github.com/ontologyxhq/oxsim
- OXFlow: https://github.com/ontologyxhq/oxflow

## License

Dual-licensed under MIT or Apache-2.0, at your option.


## Deterministic co-simulation scheduler

R9.1 adds the backend-independent master scheduler without reimplementing any simulator. R9.2 adds two thin
native adapters: the built-in Digital participant retains its R3 event queue/four-state state across scheduler
steps, while the Verilator participant retains one generated model process and advances it incrementally. The
scheduler still owns only the shared picosecond timeline, deterministic ordering, bounded delta cycles and
normalized waveforms. See [`docs/co-simulation.md`](docs/co-simulation.md).

Digital and Verilator incremental adapters are complete in R9.2, and Renode/ngspice incremental participants are complete in R9.3. R9.4 adds explicit scheduler-native DAC/ADC participants plus a portable closed-loop proof with live Verilator. The native closeout gate in `scripts/verify-r9.4-native.sh` runs real Renode firmware, Verilator, SharedSpice and returned GPIO feedback on one scheduler timeline.
