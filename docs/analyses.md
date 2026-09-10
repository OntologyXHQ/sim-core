# Analyses

The public contract currently includes:

- `operating_point`
- `dc_sweep`
- `transient`
- `ac_sweep`
- `digital_transient`
- `mixed_signal_transient`
- `firmware_transient`
- scheduler-only result kind `co_simulation_transient`

The production ngspice engine currently implements operating point, DC sweep, transient, and AC sweep.
AC results preserve real and imaginary components. The normalized axis distinguishes scalar, time,
DC-sweep, and frequency domains.


## Mixed-signal transient

`Analysis::MixedSignalTransient { step, stop }` is executed by `MixedSignalEngine`. Both values
must be finite and positive and `step <= stop`. The result can contain analog and digital
waveforms from the same coordinated solver run.


## Firmware transient

`Analysis::FirmwareTransient { step, stop }` is executed by `RenodeEngine`. `step` and `stop` are
finite positive seconds. Renode advances guest virtual time by `step`, while MCU GPIO output state
is sampled and normalized to `DigitalWaveform`. This sampled boundary is intentionally minimal;
closed-loop external input scheduling is deferred to the shared co-simulation scheduler.


## Co-simulation transient result kind

R9.1 adds `AnalysisKind::CoSimulationTransient` for normalized results produced by
`CoSimulationScheduler`. It is intentionally not an `Analysis` request variant yet: participant
sessions and cross-engine links are runtime configuration rather than solver-independent Circuit IR.
The service/production request schema therefore remains unchanged while the R9 adapter contracts are
stabilized.
