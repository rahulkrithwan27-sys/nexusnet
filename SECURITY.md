# Security Policy

## Supported versions

NexusNet is pre-1.0 and under active development. Security fixes are applied to
the latest released minor version and to `main`.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, use GitHub's private
[**Security Advisories**](https://github.com/nexusnet/nexusnet/security/advisories/new)
to report privately, or email **security@nexusnet.dev**.

Please include, where possible:

- A description of the issue and its impact.
- Steps to reproduce or a proof of concept.
- Affected versions and any known mitigations.

## What to expect

- **Acknowledgement** within 3 business days.
- A **triage assessment** and severity rating within 10 business days.
- Coordinated disclosure once a fix is available; we will credit reporters who
  wish to be named.

## Scope

Because NexusNet is a networking framework, we are especially interested in
reports concerning memory safety, cryptographic misuse, denial of service,
protocol parsing, and resource exhaustion. As implementation lands in later
phases, this policy will be expanded with component-specific guidance.
