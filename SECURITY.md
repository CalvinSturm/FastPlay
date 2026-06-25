# Security Policy

## Supported Versions

FastPlay is an early-stage project. Security fixes are applied to the latest
release and the `main` branch.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| `main` | Yes |
| Older releases | No |

Users should reproduce and verify fixes with the newest available release.

## Reporting a Vulnerability

Do not open a public issue, discussion, or pull request for a suspected
vulnerability.

Use GitHub's private vulnerability reporting form from the repository's
**Security** tab when it is available. If private vulnerability reporting is
not available, open a minimal public issue titled `Private security contact
requested` without including vulnerability details. The maintainer will
establish a private channel before requesting technical information.

In the private report, include:

- A description of the vulnerability and its impact
- The affected FastPlay version or commit
- Reproduction steps or a minimal proof of concept
- Relevant media-file characteristics, if applicable
- Windows version, architecture, GPU, and driver version
- Whether hardware or software decode is affected
- Any suggested mitigation or fix

Avoid including sensitive personal data. If a proof-of-concept media file is
required, share the smallest practical file through a private channel.

The maintainer will acknowledge the report when it has been reviewed, assess
severity and scope, coordinate a fix where appropriate, and credit the reporter
unless anonymity is requested. Disclosure timing will be coordinated after a
fix or mitigation is available.

## Scope

Examples of relevant reports include:

- Memory-safety violations in Rust or FFI boundaries
- Unsafe handling of malformed or adversarial media or subtitle files
- Path handling that permits unintended file access or overwrite
- Installer or update-distribution integrity issues
- Exposure of raw pointers, COM interfaces, or invalid GPU resources across
  safe API boundaries

General playback bugs, crashes without a security impact, feature requests, and
performance regressions should use the public issue templates.

FastPlay relies on FFmpeg, Rust crates, Windows APIs, and GPU drivers. Reports
that originate in an upstream dependency are still useful when FastPlay can
mitigate exposure, but they may also need to be reported to the upstream
project.
