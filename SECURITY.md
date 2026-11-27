# Security policy

## Reporting a vulnerability

Please do **not** open a public issue. Use GitHub's private reporting:
**Security → Report a vulnerability** on this repository.

Include the version (`server_starting` log line), what you observed and how to
reproduce it. Never include real tokens, secrets or customer data.

You can expect an acknowledgement within a few days. This is a single-maintainer
project: fixes ship as a patch release and are noted in the release notes.

## Scope

In scope: authentication/OAuth, per-page ACL filtering, multi-user isolation,
SSRF protection on URL uploads, webhook authentication, secret handling.

Supported: the latest release and the rolling `:dev` image.
