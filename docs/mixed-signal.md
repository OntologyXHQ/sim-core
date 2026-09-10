# Mixed-signal simulation

R4 adds an explicit analog/digital boundary to the solver-independent Circuit IR and a
single-process ngspice/XSPICE transient backend.

## Why explicit bridges

Analog and digital pins never share a net directly. Crossing domains requires one of two
components:

- `adc_bridge`: analog voltage input -> digital event output
- `dac_bridge`: digital event input -> analog voltage output

The bridge is part of the Circuit IR. ngspice/XSPICE model syntax remains private to the
engine adapter.

## ADC bridge contract

`ComponentKind::adc_bridge()` requires:

- `analog_in`: Analog/Input pin
- `digital_out`: Digital/Output pin
- `low_threshold`: voltage at or below which the digital result is low
- `high_threshold`: voltage at or above which the digital result is high

`low_threshold` must be strictly lower than `high_threshold`. Values between the two
thresholds are the solver's undefined digital region. Optional `rise_delay` and `fall_delay`
are non-negative time quantities.

## DAC bridge contract

`ComponentKind::dac_bridge()` requires:

- `digital_in`: Digital/Input pin
- `analog_out`: Analog/Output pin
- `low_voltage`: analog target for digital low
- `high_voltage`: analog target for digital high

`low_voltage` must be strictly lower than `high_voltage`. `unknown_voltage` defaults to the
midpoint. Optional `rise_time`, `fall_time`, and `input_load` control analog edge shape and
loading.

## Coordinated transient

`MixedSignalEngine` accepts `Analysis::MixedSignalTransient { step, stop }`. It compiles the
analog circuit, XSPICE event primitives, and all ADC/DAC bridges into one ngspice process.
The XSPICE event scheduler and ngspice analog timestep solver therefore coordinate inside the
same simulation rather than being externally stepped by Sim Core.

A mixed result may contain both normalized waveform variants:

- `Waveform::Analog` with the shared transient time axis
- `Waveform::Digital` with event transitions

## Supported components

The mixed backend supports the full analog component set already owned by `NgSpiceEngine`:
R/L/C, voltage/current sources, diode, BJT, MOSFET, op-amp subcircuits, and generic
subcircuits.

Its digital side supports the XSPICE-native R3 subset: logic inputs, clocks, logic outputs,
buffer/inverter/basic two-input gates, tri-state buffer, D/JK/T/SR flip-flops, and D latch.
Higher-level solver-independent bus primitives (`mux2`, `demux2`, `decoder2_to_4`, `register`,
`counter`) remain owned by the built-in `DigitalEngine` and are not lowered into the R4
mixed backend yet.

## Runtime boundary

The mixed backend uses the same child-process execution policy as the analog and XSPICE
adapters: timeout, cooperative cancellation, input/output/log limits, kill/reap cleanup, and
stable engine errors. Hard OS CPU/memory isolation remains an R6/R7 worker concern.
