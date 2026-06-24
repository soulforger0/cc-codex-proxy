---
name: Bug report
description: Report something that is not working as expected
title: "Bug: "
labels: [bug]
body:
  - type: textarea
    id: summary
    attributes:
      label: Summary
      description: What happened?
    validations:
      required: true
  - type: textarea
    id: steps
    attributes:
      label: Steps to reproduce
      description: Include the smallest set of steps that reproduces the issue.
      placeholder: |
        1. Launch CCCodexProxy.app
        2. Start the proxy
        3. Run claude ...
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: Expected behavior
    validations:
      required: true
  - type: textarea
    id: environment
    attributes:
      label: Environment
      description: Include macOS version, Claude Code version, app or CLI path, and commit/release version.
      placeholder: |
        - macOS:
        - Claude Code:
        - CC Codex Proxy:
        - App or CLI:
    validations:
      required: true
  - type: textarea
    id: logs
    attributes:
      label: Sanitized logs
      description: Remove OAuth tokens, account identifiers, private prompts, and session details before sharing.
      render: text
