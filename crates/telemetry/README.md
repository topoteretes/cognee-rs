# cognee-telemetry

Cognee product-analytics client (`send_telemetry`). The local sovereign
baseline is fail-closed: compiling the client does not authorize network
emission. Events are sent only when
`COGNEE_PRODUCT_TELEMETRY_ENABLED=1` (or `true`, `yes`, `on`) explicitly opts
in and no higher-priority suppression applies.

`TELEMETRY_DISABLED`, `ENV=test|dev`, and binding-host suppression remain
authoritative. Missing, empty, false-like, and unknown opt-in values emit
nothing.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
