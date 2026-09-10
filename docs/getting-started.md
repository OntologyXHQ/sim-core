# Getting started

OntologyX Sim Core is a Rust library.

```toml
[dependencies]
ontologyx-sim-core = "0.15"
```

The current analog engine launches `ngspice` as an isolated child process; ngspice is not bundled.

```bash
ngspice --version
```

Use `NgSpiceEngine::info()` when an application needs to inspect runtime availability before simulation.
The canonical Circuit IR, validation types, engine contracts, and result model are all exported by the crate.


## Optional service

Enable the R7 service boundary only when needed:

```toml
[dependencies]
ontologyx-sim-core = { version = "0.12", features = ["service"] }
```

Run the bundled loopback server with:

```bash
cargo run --features service --bin sim-service
```
