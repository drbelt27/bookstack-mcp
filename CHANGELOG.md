# Changelog

Notable changes to this maintained continuation of BookStack MCP are recorded here. Workspace versions are not necessarily published GitHub Releases. See the [migration guide](docs/MIGRATION.md) before upgrading.

## Unreleased

- Refresh the repository landing page and organize the detailed documentation into dedicated guides.
- Add release, development-version and build badges.

## 0.15.0 — 2026-11-28 (development)

Installation, diagnostics and resilience.

- Add `bsmcp-server doctor [--json]`: PASS/WARN/FAIL/SKIP checks for config, database, backend reachability (per IP family), internet egress, OAuth metadata, embedder and the technical token's permissions. Checked facts are separated from hypotheses; no secret is printed (PR [#19](https://github.com/drbelt27/bookstack-mcp/pull/19)).
- Add `scripts/install.sh` (interactive or `--yes`, idempotent, never regenerates existing secrets, quotes `.env` values safely, prints reverse-proxy snippets, touches no firewall or proxy) and `scripts/update.sh` (backup first, selective service update, rollback that prints restore commands and never `down -v`) (PR [#18](https://github.com/drbelt27/bookstack-mcp/pull/18)).
- The embedder no longer panics and crash-loops when the model download fails: it binds first, retries with backoff and jitter, and reports `loading`/`ready`/`degraded`. `/health` stays liveness, new `/ready` is readiness; `semantic_search` returns a clear temporary-unavailable error while other tools keep working (PR [#20](https://github.com/drbelt27/bookstack-mcp/pull/20)).
- Compose healthchecks for the server and the embedder, using a built-in `healthcheck` subcommand; the server does not wait for the embedder (PR [#21](https://github.com/drbelt27/bookstack-mcp/pull/21)).
- The delta walk now notices deleted pages: at most hourly it removes indexed pages the backend no longer lists, with safeguards against incomplete or implausibly large listings. `BSMCP_INDEX_DELETION_SWEEP_SECONDS=0` disables it (PR [#23](https://github.com/drbelt27/bookstack-mcp/pull/23)).
- `/status` gains an admin-only System card (versions, embedder state, last walks, last webhook received/rejected, backups) and a button that runs the doctor checks (PR [#24](https://github.com/drbelt27/bookstack-mcp/pull/24)).
- Document webhook authentication through the reverse proxy (BookStack cannot send custom headers), backup and restore, and add an optional `scripts/smoke-test.sh` that is read-only unless `--write` is given (PR [#26](https://github.com/drbelt27/bookstack-mcp/pull/26)).
- Update openssl, rustls, rustls-webpki and rand to clear open Dependabot alerts (PR [#27](https://github.com/drbelt27/bookstack-mcp/pull/27)).

## 0.14.3 — 2026-11-27 (development)

- Fix `get_content_permissions` / `update_content_permissions` for shelves, which returned HTTP 500: BookStack's content-permissions API expects `bookshelf`. The tool schema still uses `shelf` (PR [#17](https://github.com/drbelt27/bookstack-mcp/pull/17)).

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
