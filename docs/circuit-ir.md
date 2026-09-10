# Circuit IR

The Circuit IR is solver-independent and versioned independently of engine syntax.

A circuit contains a schema version, components, nets, and optional model definitions.
Components have stable IDs and kinds; pins carry signal domain and direction; nets connect endpoints by component and pin ID.

The current public schema version is `1`.

## Signal domains

- `analog`
- `digital`
- `mixed`
- `reference`

Direct analog-to-digital joins fail validation unless an explicit mixed-signal bridge owns the conversion.


## Mixed-signal bridges

R4 keeps analog and digital nets electrically distinct. `adc_bridge` owns an Analog/Input
`analog_in` pin and Digital/Output `digital_out` pin. `dac_bridge` owns a Digital/Input
`digital_in` pin and Analog/Output `analog_out` pin. Validation rejects a direct analog/digital
join on the same net. Bridge parameters are typed and solver-independent; see
[`mixed-signal.md`](mixed-signal.md).

## HDL module blocks

R5 reuses the model registry instead of introducing a parallel HDL graph. A Verilog or
SystemVerilog `ModelDefinition` has `ModelKind::Module`; an `hdl_module` component references it
through the existing `model` parameter. Every Circuit IR pin remains a scalar digital endpoint.
`Pin.name` maps that endpoint to the HDL top-level port (`data` or `data[bit]`), so vector HDL
ports can be represented without changing the net or waveform schema.


## MCU blocks

R6 adds `ComponentKind::mcu()`. Its initial firmware-runtime boundary exposes digital Output pins;
`Pin.name` identifies the Renode GPIO endpoint as `peripheral@pin` (for example `gpioPortD@13`).
Firmware bytes and board/platform selection are backend configuration on `RenodeTarget`, not Circuit
IR parameters, so executable artifacts do not inflate or couple the serialized circuit schema.
