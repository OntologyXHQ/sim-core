# Digital simulation

R3 defines solver-independent digital semantics and a built-in pure-Rust reference engine.
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

Canonical R3 component kinds:

| Family | Kinds | Canonical pins |
| --- | --- | --- |
| sources/sinks | `logic_input`, `digital_clock`, `logic_output` | `out`; `out`; `in` |
| combinational | `buffer`, `not_gate` | `in`, `out` |
| binary gates | `and_gate`, `or_gate`, `xor_gate`, `nand_gate`, `nor_gate`, `xnor_gate` | `a`, `b`, `out` |
| bus drive | `tri_state_buffer` | `in`, `enable`, `out` |
| selection | `mux2`, `demux2`, `decoder2_to_4` | scalar selection pins documented by the kind |
| latches | `d_latch`, `sr_latch` | data/control inputs, `q`, optional `nq`, optional async `set`/`reset` |
| flip-flops | `d_flip_flop`, `jk_flip_flop`, `t_flip_flop`, `sr_flip_flop` | data inputs, `clk`, `q`, optional `nq`, optional async `set`/`reset` |
| bus storage | `register` | `d0..dN`, `clk`, optional `enable`/`reset`, `q0..qN` |
| counter | `counter` | `clk`, optional `enable`/`reset`, `q0..qN` |

Sequential primitives accept optional `initial`, `delay`, and `edge` parameters. `edge` is
`rising` by default and may be `falling` in the built-in engine. Registers and counters accept
`width` from 1 through 64; their bit numbering is little-endian (`q0` is the least-significant bit).

`LogicVector` is the public width-aware helper for four-state buses. It converts known vectors to
and from `u64` without collapsing `X` or `Z`.

## Event model

`Analysis::DigitalTransient { stop }` is event driven. Digital results are not sampled at a fixed
step size. Each observed net produces a `DigitalWaveform` containing only state transitions.
Same-time zero-delay settling is normalized deterministically to the final state at that timestamp.
R3.0 gate `delay` uses transport-delay semantics: every logical change is propagated after the delay.

The execution-control contract also applies to the built-in digital engine: cancellation and
wall-clock timeout are checked cooperatively, transition output is bounded, and a hard event-count
ceiling prevents zero-delay oscillators from running forever.

## Multi-driver and bus semantics

Digital nets are driver-aware. `Z` releases a net; equal known drivers resolve to that value;
conflicting known drivers resolve to `X`; any active unknown driver also resolves to `X`.
Validation reports multi-driver nets as a warning rather than rejecting them.

Same-time driver events are committed as one event bucket before dependent components evaluate,
so bus hand-off is deterministic and does not create ordering-dependent glitches.

## Sequential semantics

The built-in engine implements D/SR latches and D/JK/T/SR flip-flops with deterministic
four-state behavior, async set/reset, rising/falling edge selection, and propagation delay.
Registers and counters use the same sequential runtime state and event queue.

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



## R9.2 co-simulation participant

`DigitalCoSimulationParticipant` exposes selected existing R3 nets as scheduler input/output ports. It reuses the same event queue, propagation delays, four-state resolution and sequential runtime state; it does not rerun `DigitalEngine::simulate` for every macro-step. Externally writable nets must not already have an internal driver.
