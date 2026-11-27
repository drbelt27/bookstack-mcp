# Changelog

Notable changes to this maintained continuation of BookStack MCP are recorded here. Workspace versions are not necessarily published GitHub Releases. See the [migration guide](docs/MIGRATION.md) before upgrading.

## Unreleased

- Refresh the repository landing page and organize the detailed documentation into dedicated guides.
- Add release, development-version and build badges.

## 0.14.2 — 2026-11-27 (development)

- Improve HTTP failure diagnostics by logging the underlying cause chain (PR [#9](https://github.com/drbelt27/bookstack-mcp/pull/9)).
- Add repository governance and security policy (PR [#10](https://github.com/drbelt27/bookstack-mcp/pull/10)).

## 0.14.1 — 2026-11-27 (development)

- Fix remote file downloads when a hostname resolves to multiple addresses. Preserve all validated DNS addresses for IPv4/IPv6 fallback without weakening SSRF protections (PR [#7](https://github.com/drbelt27/bookstack-mcp/pull/7)).
- Remote image upload and embedding verified on a real BookStack installation.

## 0.14.0 — maintained continuation

- Retain the upstream BookStack and LibStack integrations, OAuth and optional semantic search.
- Harden access checks and client abuse limits (PR [#4](https://github.com/drbelt27/bookstack-mcp/pull/4)).
- Publish development images under the maintained repository's GHCR namespace (PR [#3](https://github.com/drbelt27/bookstack-mcp/pull/3)).
- Align API request budgets with BookStack (PR [#5](https://github.com/drbelt27/bookstack-mcp/pull/5)).
- Clarify licensing and development guidance (PRs [#2](https://github.com/drbelt27/bookstack-mcp/pull/2), [#6](https://github.com/drbelt27/bookstack-mcp/pull/6)).

## Earlier upstream versions

The detailed historical upgrade instructions, including breaking changes from earlier upstream versions, are preserved in the [migration guide](docs/MIGRATION.md). See also the [archived upstream releases](https://github.com/bees-roadhouse/bookstack-mcp/releases).
