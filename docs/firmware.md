# MCU / firmware runtime

R6 adds a deliberately thin Renode-backed MCU execution boundary. Sim Core does not implement a
CPU decoder, MCU peripheral framework, ELF loader, or board model. Renode owns those responsibilities;
Sim Core owns only the solver-independent circuit contract, process controls, GPIO observation,
virtual-time sampling, and normalized results.

## Firmware artifacts

`FirmwareArtifact` carries immutable firmware bytes plus the minimum loader metadata:

- `Elf` and `IntelHex` are handed directly to Renode's native loaders.
- `Binary` additionally requires an explicit load address.
- an optional entry point can override the target CPU PC after loading.

Firmware bytes intentionally live on `RenodeTarget`, not inside `SimulationRequest`. That keeps the
circuit/request schema independent from executable payload size and lets future service workers build
backend instances from job artifacts without changing the public Circuit IR.

## MCU component

`ComponentKind::mcu()` is the R6 circuit boundary. R6 output pins are digital `Output` pins and use
`Pin.name` as a Renode GPIO mapping, for example `gpioPortD@13`.

The initial R6 boundary is intentionally output-only. GPIO input/bidirectional synchronization,
UART/SPI/I2C transactions, MCU ADC/DAC and closed-loop analog/RTL/firmware co-simulation require the
shared deterministic scheduler and are not reimplemented inside this adapter.

## Firmware transient

`Analysis::FirmwareTransient { step, stop }` advances Renode by exact virtual-time increments using
its execution-control surface. Each exposed MCU GPIO is attached to a lightweight Renode LED observer,
sampled at the requested virtual-time points, and normalized to the existing `DigitalWaveform` type.
Consecutive equal samples are collapsed to transitions.

This is a sampled boundary rather than an instruction-level GPIO event stream. The `step` therefore
sets observation resolution; R6 caps the total GPIO sample count with `MAX_RENODE_SAMPLE_POINTS`.

## Execution and isolation

Renode runs as a child process under the existing `ExecutionControl` timeout, cancellation, generated
input, and log-size limits. Firmware and generated scripts use an ephemeral run directory. Hard OS
CPU/memory/filesystem isolation remains a production-worker responsibility in R8.

## Real proof

`tests/renode_engine.rs` loads a tiny Cortex-M4 ELF on Renode's STM32F4 Discovery platform. The
firmware enables GPIOD, toggles PD13, and the test requires the resulting normalized waveform to
contain both low and high states. The fixture is self-contained and does not download firmware at test time.
