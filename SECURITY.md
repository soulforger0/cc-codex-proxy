# Security Policy

CC Codex Proxy handles local OAuth tokens and Claude Code traffic. Please report security-sensitive issues privately instead of opening a public issue with secrets or session details.

## Supported Versions

The project is early-stage. Security fixes target the latest `main` branch and latest GitHub release.

## Reporting a Vulnerability

Please contact the maintainer through GitHub with a minimal description first. Do not include OAuth tokens, account identifiers, private prompts, or complete logs in public issues.

Helpful details include:

- affected version or commit
- macOS version
- whether the app or CLI path was used
- impact and reproduction steps
- sanitized logs or proof-of-concept details

## Local Data

Runtime data is stored under `~/Library/Application Support/CCCodexProxy` and logs under `~/Library/Logs/CCCodexProxy`. Treat those files as sensitive when sharing diagnostics.
