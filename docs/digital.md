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
