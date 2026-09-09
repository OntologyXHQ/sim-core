# Changelog

## 0.3.0 — standalone core baseline

- extracted the verified Rust simulation authority from OXSim into a pure-Rust repository
- renamed the public crate from the unpublished `ontologyx-sim` identity to `ontologyx-sim-core`
- retained Circuit IR schema version 1 and the existing simulation behavior
- retained the process-isolated ngspice engine and model registry
- removed Node/N-API and product-shell ownership from the core boundary
