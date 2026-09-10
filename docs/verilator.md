# Verilator / HDL simulation

R5 adds Verilog/SystemVerilog blocks without implementing an HDL simulator in Rust.

## Model and component

HDL source is stored in a module model:

```rust
let model = ModelDefinition::system_verilog_module(
    "counter",
    "counter",
    "module counter(input logic clk, output logic [1:0] q); ... endmodule",
);
```

An `hdl_module` component references that model with `parameters["model"]`. Circuit pins stay
scalar. `Pin.name` maps them to the HDL top-level port:

- `clk` -> scalar port `clk`
- `q[0]`, `q[1]` -> two bits of vector port `q`

This keeps the serialized Circuit IR solver-independent and avoids adding a second HDL-only graph.

## Runtime

The adapter writes inline module sources plus one generated `oxsim_tb.sv`, then runs:

```text
verilator --binary --timing --trace-vcd ...
```

Verilator owns elaboration, C++ generation, compilation and execution. The generated testbench
owns only canonical constants/clocks, net wiring, module instantiation, stop time and VCD dumping.
VCD signals are parsed into existing `DigitalWaveform` values.

R5 uses one-picosecond generated testbench precision and deterministic `--x-assign 0` /
`--x-initial 0`. Verilator is mostly two-state, so HDL boundary constants/clocks must be known
zero/one. Use the built-in R3 engine when four-state X/Z propagation is the semantic requirement.

## Deliberate limits

To keep R5 small and trustworthy:

- supported circuit components in a Verilator request are `hdl_module`, `logic_input`,
  `digital_clock`, and `logic_output`;
- HDL module pins are Input/Output only;
- closed-loop analog↔RTL scheduling is not emulated by a hand-written timestep loop;
- compilation caching is left to the production runtime milestone;
- inline HDL cannot use include/DPI/host file or process I/O/testbench dump/control tasks.

The next shared co-simulation scheduler can connect Verilator modules to mixed-signal and MCU
backends without changing the R5 model/pin/result contracts.


## R9.2 live participant

`VerilatorCoSimulationParticipant` builds one Verilated top model and keeps that generated process alive across scheduler steps. A tiny stdin/stdout protocol sets scalar inputs, advances the model to an integer picosecond target, and returns scalar output state plus Verilator's next delayed event. The adapter delegates stateful RTL evaluation to `eval()`, `eventsPending()` and `nextTimeSlot()` rather than replaying batch simulations. The R9.2 live boundary is intentionally deterministic two-state; X/Z remains a Digital participant concern.
