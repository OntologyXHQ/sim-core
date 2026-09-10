# Compatibility

## Rust

Minimum supported Rust version: 1.88.

## ngspice

The analog engine currently discovers `ngspice` from `PATH`, or accepts an explicit executable path through the Rust API.
The repository verification suite is fail-closed when real-ngspice proof is requested.

No Node.js, npm, browser, UI, or OXFlow runtime is required by this repository.


## Mixed-signal

R4 mixed-signal execution requires an ngspice build with usable XSPICE code models, including
`adc_bridge` and `dac_bridge`. Analog devices share `NgSpiceEngine` compatibility. The digital
portion supports the XSPICE-native R3 primitive subset; built-in higher-level bus primitives are
not lowered into mixed-signal netlists in R4.


## Verilator

R5 discovers `verilator` from `PATH`, or accepts an explicit executable path through
`VerilatorEngine::new`. Full repository verification is fail-closed and requires a Verilator
build that supports `--binary`, `--timing`, and `--trace-vcd` (the supported Verilator 5.x
workflow). GNU Make and a compatible C++ compiler are indirect Verilator build requirements.

R5 uses deterministic two-state execution (`--x-assign 0`, `--x-initial 0`). The built-in R3
`DigitalEngine` remains authoritative when true X/Z propagation is required. HDL boundary
inputs are therefore restricted to known zero/one values.

Inline HDL module models reject include directives, DPI, host process/file I/O, and testbench
control/dump tasks. This is an admission boundary, not an OS sandbox; hard CPU/memory/filesystem
isolation remains a production worker/runtime responsibility.


## Renode

R6 uses the Renode Monitor/process interface and native ELF/HEX/BIN loaders. Canonical CI is pinned
to Renode 1.17.0 stable. The real integration proof uses the bundled STM32F4 Discovery platform and
a self-contained Cortex-M4 ELF fixture; no network firmware download occurs during tests.
