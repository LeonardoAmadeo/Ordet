# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.1.x-beta | Yes, active beta, please report issues |

Ordet is local-first and makes no network requests. Ordet never sends
document text anywhere and never rewrites text on its own.

## Reporting a Vulnerability

- Do not open public issues for data-loss, clipboard exfiltration, keychain, or
  file-handling vulnerabilities.
- Contact the maintainers privately with: Ordet version (`ordet -V`),
  OS/terminal name and size, and steps to reproduce.
- Expect an acknowledgement within 72 hours. Fixes ship as a new beta tag with
  a CHANGELOG entry; credit on request.

## Handling Notes

- Crash snapshots (`recovery.json`) and revision history (`history.json`) are
  plain text; do not include secrets in drafts you want kept private on shared
  machines.
