# Contributing

`scripts/gate.sh` must be green before a push: build, `cargo test`, `cargo clippy --all-targets -D warnings`,
`cargo fmt --check`, `cargo deny check licenses`, and the named mutants in `scripts/mutants.list`. A new check ships
with its mutant on the same line of that list, in the same change; `scripts/mutants.sh <substring>` runs a subset.

The device layer is the part worth extending: `src/transport/` holds the
`DeviceTransport` trait and its Android, iOS and Appium implementations. A new
backend implements that trait and nothing else changes.

Issues and PRs welcome. For a behaviour change, include the `drengr look` or
`drengr do` output that shows it working on a real device.
