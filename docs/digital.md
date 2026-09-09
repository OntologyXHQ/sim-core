# Digital simulation

R3.0 introduces solver-independent digital semantics and a built-in pure-Rust reference engine.
The reference engine is intentionally independent of XSPICE syntax so future solver adapters can
be checked for semantic parity.

## Logic values

`LogicValue` is four-state:

- `Zero`
- `One`
- `X` (unknown)
- `Z` (high impedance)

Combinational operators are conservative. `Z` used as a normal logic-gate input is treated as
unknown. Controlling values still dominate (`0 AND X = 0`, `1 OR X = 1`).

## Components

R3.0 canonical kinds and pins:

| Kind | Pins | Parameters |
| --- | --- | --- |
| `logic_input` | `out` output | `value` = bool / 0 / 1 / `x` / `z` |
| `digital_clock` | `out` output | `period`, optional `duty_cycle`, optional `initial` |
| `logic_output` | `in` input | none |
| `buffer` | `in`, `out` | optional `delay` |
| `not_gate` | `in`, `out` | optional `delay` |
| `and_gate`, `or_gate`, `xor_gate`, `nand_gate`, `nor_gate`, `xnor_gate` | `a`, `b`, `out` | optional `delay` |

`period` and `delay` are seconds. `duty_cycle` must be greater than zero and less than one.

## Event model

`Analysis::DigitalTransient { stop }` is event driven. Digital results are not sampled at a fixed
step size. Each observed net produces a `DigitalWaveform` containing only state transitions.
Same-time zero-delay settling is normalized deterministically to the final state at that timestamp.
R3.0 gate `delay` uses transport-delay semantics: every logical change is propagated after the delay.

The execution-control contract also applies to the built-in digital engine: cancellation and
wall-clock timeout are checked cooperatively, transition output is bounded, and a hard event-count
ceiling prevents zero-delay oscillators from running forever.

## Current R3.0 limits

- pure digital circuits only
- one output/bidirectional driver per digital net
- no tri-state bus resolution yet
- no sequential storage elements yet
- no XSPICE adapter yet

R3.1 adds an XSPICE adapter and parity proofs against the R3.0 reference semantics.


## XSPICE adapter

Sim Core 0.7 adds `XSpiceEngine`, which maps the canonical digital component kinds to ngspice
XSPICE event-driven code models. `logic_input` and `digital_clock` are compiled into a generated
`d_source` stimulus table; combinational gates map to the corresponding `d_*` code model.
Event nodes are exported by ngspice as VCD and normalized back into the same transition-based
`DigitalWaveform` used by the reference engine.

The adapter deliberately keeps `DigitalEngine` as the semantic authority. XSPICE parity tests
compare final logic values and representable propagation timing. XSPICE basic gates require a
minimum rise/fall delay, exposed as `XSPICE_MIN_DELAY_SECONDS`; requests below that floor are
clamped and reported through an `xspice_delay_floor` diagnostic.
### XSPICE startup and timing normalization

The built-in `DigitalEngine` intentionally initializes nets as `X`. XSPICE initializes digital event nodes to `ZERO` at simulation start. Adapter parity therefore compares causal transitions after startup rather than treating the solver's initialization artifact as a logical propagation event. The adapter exports VCD with an explicit `1e-15` second timescale and forces XSPICE transport-delay mode so representable event timing remains deterministic. Results carry an `xspice_initialization_semantics` informational diagnostic.

