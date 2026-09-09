# Analyses

The public contract currently includes:

- `operating_point`
- `dc_sweep`
- `transient`
- `ac_sweep`
- `digital_transient`
- `mixed_signal_transient`

The production ngspice engine currently implements operating point, DC sweep, transient, and AC sweep.
AC results preserve real and imaginary components. The normalized axis distinguishes scalar, time,
DC-sweep, and frequency domains.
