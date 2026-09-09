# OntologyX Sim Core

`ontologyx-sim-core` is the pure Rust electrical-simulation engine used by OXSim.
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

## Install

```toml
[dependencies]
ontologyx-sim-core = "0.3"
```

Real analog simulation currently requires `ngspice` on `PATH`.

```bash
ngspice --version
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

## Repository contract

Run the full release-readiness gate with:

```bash
./scripts/verify.sh
```

It checks formatting, clippy with warnings denied, all Rust tests, real ngspice integration,
the runnable example, and a Cargo package dry-run.

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
            v
      ngspice adapter
            |
            v
 isolated ngspice process
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
- [Roadmap](docs/roadmap.md)

## Related repositories

- OXSim product: https://github.com/ontologyxhq/oxsim
- OXFlow: https://github.com/ontologyxhq/oxflow

## License

Dual-licensed under MIT or Apache-2.0, at your option.
