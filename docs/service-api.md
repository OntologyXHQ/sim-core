# Simulation Service API

R7 adds an optional Axum/Tokio service boundary without moving simulation semantics out of the Rust core.
Enable it with the `service` Cargo feature.

## Endpoints

- `GET /healthz` — process health and sim-core version
- `GET /v1/engines` — registered engine descriptors
- `POST /v1/simulations` — enqueue a simulation and return `202 Accepted`
- `GET /v1/simulations/{id}` — current job snapshot/result/error
- `DELETE /v1/simulations/{id}` — cooperative cancellation
- `GET /v1/simulations/{id}/events` — WebSocket stream of full job snapshots

The submission envelope is intentionally small:

```json
{
  "request": {
    "circuit": {},
    "analysis": {},
    "probes": []
  }
}
```

Execution policy is server-owned rather than client-controlled, so a remote caller cannot disable timeout or resource limits.

## Runtime ownership

`SimulationService` stores only bounded in-memory job state and runs synchronous engine calls with `tokio::task::spawn_blocking` behind a semaphore. Existing `ExecutionControl` and `CancellationToken` remain authoritative for engine timeout/cancellation/input/output/log limits.

This service layer remains transport-only. R8 provides an optional durable single-host queue/cache and Linux worker-isolation runtime; distributed workers and remote persistence remain deployment concerns.

## WebSocket events

Each WebSocket message is one serialized `SimulationJobEvent` containing a complete `SimulationJobSnapshot`. Full snapshots keep clients resilient to reconnects and lag without requiring a second event-reducer schema. Terminal snapshots contain either the normalized `SimulationResult` or a stable failure envelope.

R7 does not claim incremental solver waveform streaming because the current engines return normalized results at completion. Progressive solver streaming can be introduced later without changing the job lifecycle contract.

## Default server

The optional `sim-service` binary registers the generic Digital, ngspice, XSPICE, mixed-signal and Verilator engines and binds to `127.0.0.1:4000` by default. Set `OXSIM_BIND` to change the address.

Renode remains target-specific because `RenodeTarget` owns a firmware artifact and platform binding. R8 keeps its executor boundary generic so a production host can supply a configured Renode executor without embedding firmware payloads into the solver-independent `SimulationRequest`.
