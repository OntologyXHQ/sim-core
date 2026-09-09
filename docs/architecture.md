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
   +--> future XSPICE/digital engine
   +--> future Verilator engine
   |
   v
normalized simulation result
```

The repository is deliberately Rust-only. Language bindings and OXSim product integration are downstream adapters.
Engine adapters must not leak solver-specific netlist syntax into the public Circuit IR.
