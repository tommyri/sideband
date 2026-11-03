# Security reporting

SocialSpeak is a development preview. No version currently carries a production
security-support commitment. See [security design and known gaps](docs/security.md).

Please do not disclose exploitable vulnerabilities, credentials, private server
addresses, recordings or personal information in public issues or pull requests.

On the official repository, use **Security → Advisories → Report a vulnerability**
when private vulnerability reporting is enabled. If that option is unavailable,
open an issue asking the maintainer to enable a private reporting channel, without
including vulnerability details. Do not assume an unlisted email address or a fork
is a private or monitored reporting channel.

A private report should include the affected commit/version, reproduction steps,
expected versus actual behavior, impact, and a minimal sanitized reproducer.
Omit real credentials and user data. Coordinate disclosure with the maintainer;
there is no promised response SLA while the project is in development.

Repository maintainers: enable private vulnerability reporting, secret scanning
and push protection before inviting public use. The local/CI secret scan reduces
accidental exposure but cannot prove the absence of secrets or vulnerabilities.
