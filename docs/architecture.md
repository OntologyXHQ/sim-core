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
   +--> future XSPICE adapter
   +--> future Verilator engine
   |
   v
normalized simulation result
```

The repository is deliberately Rust-only. Language bindings and OXSim product integration are downstream adapters.
Engine adapters must not leak solver-specific netlist syntax into the public Circuit IR.


## Process execution boundary

The ngspice adapter runs the solver as a child process instead of linking solver state into
the host process. `ExecutionControl` provides timeout, cancellation and bounded solver
input/output. A child-process guard kills and reaps an unfinished solver on every controlled
error path before the temporary run directory is removed. This boundary is intentionally
portable and does not claim OS-level memory/CPU isolation.
