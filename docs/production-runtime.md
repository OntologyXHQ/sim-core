# Production runtime

R8 adds a production-oriented execution layer without moving queue, caching or sandbox semantics into individual simulation engines. Enable it with the `production` Cargo feature.

## Ownership

`ProductionRuntime` owns durable job lifecycle, bounded parallel dispatch, retry/recovery, content-addressed artifacts, result caching and reproducibility manifests. `RuntimeExecutor` is the narrow backend contract. `SimulatorExecutor` adapts any configured in-process `Simulator`; `IsolatedProcessExecutor` runs the stock `sim-worker` in a separate Linux sandbox.

The default store is intentionally single-host and filesystem-backed. Queue states live under `queue/`, `running/` and `terminal/`; cache objects live under `cache/sha256/`; immutable artifacts live under `artifacts/sha256/`. Claims use same-filesystem rename, and `ProductionRuntime::open` recovers stale running records after a crash.

## Reproducibility and cache identity

Every submitted request gets a `ReproducibilityManifest`. The cache key covers:

- manifest schema version and sim-core crate version;
- SHA-256 of the serialized `SimulationRequest`;
- sorted engine descriptors, including backend versions when available;
- host-owned `ExecutionPolicy`;
- executor/toolchain fingerprint.

A cache hit returns the existing normalized `SimulationResult` without invoking the executor. The explicit executor fingerprint is where a deployment records its immutable worker/toolchain image identity.

## Linux hard isolation

`IsolatedProcessExecutor` delegates sandbox construction to `bubblewrap` and hard process ceilings to `prlimit`. It unshares user, PID, IPC, UTS and network namespaces; exposes only read-only system/toolchain paths, a fresh `/tmp`, and the one writable job directory; and applies address-space, CPU-time, process-count, open-file and output-file-size limits. Existing wall-clock timeout, cooperative cancellation and input/output/log limits remain enforced by `ExecutionPolicy`.

The stock worker has no listener and no queue implementation. It reads one `WorkerRequestEnvelope`, executes it with the normal simulator contract and writes one `WorkerResponseEnvelope`.

## Scope

R8 is deliberately not Redis, Kubernetes or a distributed scheduler. Multi-host leasing, remote object storage, deployment authentication/TLS and shared deterministic Renode↔Verilator↔mixed-signal co-scheduling remain outside this milestone.
