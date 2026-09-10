# Roadmap

The standalone core continues the simulation roadmap without taking product/UI ownership.

- R2.2: core contract hardening (result provenance + bounded execution policy complete)
- R3: complete digital subsystem: four-state event engine, XSPICE adapter/parity, sequential logic, tri-state/multi-driver resolution, bus primitives, mux/demux/decoder, registers, and counters (complete)
- R4: mixed-signal bridges and coordinated transient simulation (complete)
- R5: Verilator-backed Verilog/SystemVerilog module blocks with normalized VCD waveforms (complete)
- R6: Renode-backed MCU/Firmware Runtime with firmware artifacts, virtual-time GPIO observation, normalized waveforms, and real Cortex-M proof (complete)
- R7: Axum simulation service API with bounded async jobs, REST lifecycle, WebSocket events, cancellation and service-owned execution limits (complete)
- R8: production runtime: durable single-host queue, parallel workers, content-addressed cache/artifacts, bounded retries/recovery, reproducibility manifests, and Linux bubblewrap/prlimit isolation (complete)
- R9.1: deterministic co-simulation master substrate: integer global time, participant/link contracts, bounded macro steps, delta-cycle settling, cancellation and unified waveforms (complete)
- R9.2: incremental built-in digital + Verilator participant adapters (complete)
- R9.3: Renode external-control + ngspice shared-library participant adapters (complete)
- R9.4: explicit scheduler-native DAC/ADC bridge participants + portable closed-loop firmware-protocol ↔ real Verilator ↔ analog-protocol proof (implementation complete); real Renode + SharedSpice closeout gate: `scripts/verify-r9.4-native.sh`
- R10: debugging/inspection runtime: pause/step, symbols/registers/MMIO/IRQ traces, HDL signals, and unified timeline
- later: additional analog engines, sweeps, Monte Carlo, sensitivity, and performance hardening
