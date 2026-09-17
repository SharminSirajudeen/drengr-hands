# Security

Report vulnerabilities to hey@drengr.dev rather than opening a public issue.

Provider API keys are stored in the OS keychain (Keychain on macOS, Credential
Manager on Windows, Secret Service on Linux), never in plaintext files. Set
`DRENGR_KEYCHAIN=disable` to opt out, which falls back to `~/.drengr/`.

Diagnostic bundles written to `~/.drengr/diagnostics/` are redacted on disk
before they are saved. Read one with `drengr diag show <run-id>` before sharing it.
