# Contributing

`cargo test` must pass and `cargo clippy --all-targets` must not add warnings.

The device layer is the part worth extending: `src/transport/` holds the
`DeviceTransport` trait and its Android, iOS and Appium implementations. A new
backend implements that trait and nothing else changes.

Issues and PRs welcome. For a behaviour change, include the `drengr look` or
`drengr do` output that shows it working on a real device.
