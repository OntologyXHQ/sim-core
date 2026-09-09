# Compatibility

## Rust

Minimum supported Rust version: 1.88.

## ngspice

The analog engine currently discovers `ngspice` from `PATH`, or accepts an explicit executable path through the Rust API.
The repository verification suite is fail-closed when real-ngspice proof is requested.

No Node.js, npm, browser, UI, or OXFlow runtime is required by this repository.
