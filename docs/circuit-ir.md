# Circuit IR

The Circuit IR is solver-independent and versioned independently of engine syntax.

A circuit contains a schema version, components, nets, and optional model definitions.
Components have stable IDs and kinds; pins carry signal domain and direction; nets connect endpoints by component and pin ID.

The current public schema version is `1`.

## Signal domains

- `analog`
- `digital`
- `mixed`
- `reference`

Direct analog-to-digital joins fail validation unless an explicit mixed-signal bridge owns the conversion.
