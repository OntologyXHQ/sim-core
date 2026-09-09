# SPICE model registry

SPICE models belong to the solver-independent Circuit IR rather than directly to the ngspice API.

A model definition contains a stable ID, language, model kind, declared entry symbol, and inline source.
Current model-backed components include diode, BJT, MOSFET, op-amp, and generic subcircuits.

## Safety boundary

Inline source is fail-closed. Control and external-file directives such as `.control`, `.end`,
`.include`, `.inc`, `.lib`, and `.source` are rejected before execution.
