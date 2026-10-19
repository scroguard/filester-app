# Security Policy

## Supported versions

Only the latest release and the `main` branch receive security fixes.

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Report privately through GitHub's
[private vulnerability reporting](https://github.com/scroguard/filester-app/security/advisories/new)
for this repository. Please include:

- the affected version or commit,
- your distribution and desktop environment,
- reproduction steps or a proof of concept,
- the impact you observed or expect.

You can expect an acknowledgement within 7 days. A fix or mitigation plan
should follow within 30 days for confirmed issues. Credit is given in the
release notes unless you ask otherwise.

## Scope

In scope:

- Leakage of the Filester API key (disk, logs, memory after sign-out, IPC,
  network to any host other than the API origin `u1.filester.me`)
- TLS validation bypasses or cleartext fallbacks
- Path traversal or unsafe handling of server-supplied file and folder names
- Memory-safety issues, including in dependencies as used by this application
- CI/CD or supply-chain weaknesses in this repository

Out of scope:

- Vulnerabilities in the Filester service itself. Report those to Filester.
- Attacks that require an already-compromised user session, for example
  malware running as the same user, which can read the unlocked keyring.

## Audit references

The design-level security controls, and where each one is implemented, are
listed in the [Security & Auditing](README.md#security--auditing) section of
the README.
