# Security

Please report security issues privately through the security reporting facilities of the
`ontologyxhq/sim-core` repository when available rather than opening a public issue.

Inline SPICE model source is treated as untrusted input. Simulator-control and external-file
directives remain fail-closed before execution. New engine adapters must preserve process and
filesystem isolation boundaries appropriate to their runtime.
