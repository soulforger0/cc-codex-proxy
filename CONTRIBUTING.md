# Contributing

Thanks for helping improve CC Codex Proxy.

## Local Checks

Run the checks that match your change before opening a pull request:

```sh
cargo test --all
swift build --package-path macos/CCCodexProxy
```

For app bundle changes, also run:

```sh
scripts/build-app.sh
```

## Development Notes

- Keep the proxy local-only unless there is a deliberate design change.
- Avoid committing OAuth tokens, logs, account identifiers, or private Claude Code session details.
- Prefer small, focused pull requests with a clear test plan.
- Match the existing Rust and Swift style in nearby files.

## Reporting Issues

When filing a bug, include:

- macOS version
- Claude Code version, if relevant
- whether the app or CLI path was used
- relevant log snippets with secrets removed
- steps to reproduce
