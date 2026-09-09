# Contributing

OntologyX Sim Core is intentionally Rust-only.

Before opening a pull request, run:

```bash
./scripts/verify.sh
```

Core changes must preserve solver-independent public contracts. UI, graph editing,
Node bindings, application persistence, and OXSim product behavior belong outside this repository.
