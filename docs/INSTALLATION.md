# Installation and connection

[Project home](../README.md) · [Configuration](CONFIGURATION.md) · [Operations](OPERATIONS.md) · [Upgrading](MIGRATION.md)

## Setup

### Prerequisites

- A BookStack instance with API access enabled
- A BookStack API token (created in your BookStack user profile under "API Tokens")
- Docker and Docker Compose (for container deployment)

### Quick Start (PostgreSQL — recommended)

```bash
cp .env.example .env
# Edit .env with your BookStack URL, encryption key, and database password

docker compose -f docker/docker-compose.yml up -d
```

This starts four containers:
- **bsmcp-postgres** — PostgreSQL 17 with pgvector extension
- **bsmcp-server** — MCP server (port 8080)
- **bsmcp-embedder** — Background embedding service (`--role=embedder`, default); also serves `/rerank` when a reranker is configured
- **bsmcp-worker** — Reconciliation worker (same image as the embedder, started with `--role=worker`): initial walk on cold start, webhook + cron job consumption, periodic delta walk

### Quick Start (SQLite — simple)

```bash
cp .env.example .env
# Edit .env with your BookStack URL and encryption key

docker compose -f docker/docker-compose.sqlite.yml up -d
```

This starts three containers (server + embedder + worker) sharing a SQLite database file.

### Run from source

The project builds multi-arch (`linux/amd64` + `linux/arm64`) container images on GHCR — `ghcr.io/drbelt27/bsmcp-server` and `ghcr.io/drbelt27/bsmcp-embedder` (check package visibility before pulling). Native binaries for **`bsmcp-server` only** are attached to each GitHub Release for `linux-x86_64`, `linux-aarch64`, `darwin-x86_64`, `darwin-aarch64`, and `windows-x86_64`. The embedder is **not** distributed as a bare binary — it depends on ONNX Runtime (a per-platform C++ shared library), so running it outside Docker is awkward. Either run the published embedder container, or build from source:

```bash
# Server
cargo run --release -p bsmcp-server

# Embedder (separate terminal)
cargo run --release -p bsmcp-embedder
```

The server is pure Rust + bundled SQLite and builds cleanly on any target the Rust toolchain supports. The embedder depends on [`fastembed`](https://crates.io/crates/fastembed), which links ONNX Runtime; the crate downloads a matching prebuilt at build time for common targets, but cross-compiling or running on uncommon platforms may require installing ONNX Runtime separately. For most users, running the embedder from the published container avoids that complexity entirely.

## Guided installation

`scripts/install.sh` prepares `.env` and picks the compose file; it does not start containers and never touches firewall, reverse proxy or system config.

```bash
scripts/install.sh                      # interactive
scripts/install.sh --yes --db sqlite --semantic on \
  --public-domain mcp.example.com --bookstack-url https://kb.example.com   # non-interactive
scripts/install.sh --dry-run ...        # checks only, writes nothing
```

- Copies `.env.example` to `.env` only if missing; generates missing `BSMCP_ENCRYPTION_KEY`, `BSMCP_WEBHOOK_SECRET` and the postgres password; **never** regenerates an existing secret. `.env` is `chmod 600`.
- If `.env` already exists the script runs in-place and asks before changing any value.
- Values are written single-quoted only when needed, so both `docker compose` and `set -a; . ./.env` accept them.
- Checks prerequisites, port conflicts, `docker compose config -q`, container egress and container access to the BookStack URL, then prints the next commands plus Apache and Nginx proxy snippets (SSE-friendly) as text.

## Updating and rolling back

```bash
scripts/update.sh                       # bsmcp-server only
scripts/update.sh --all                 # every service except postgres
scripts/update.sh --service bsmcp-embedder --channel stable   # stable = :latest, or pass a tag
scripts/update.sh --rollback            # previous image digests
```

The script refuses if nothing is running, takes a backup first (sqlite: `sqlite3 .backup` in the container, else copy of db + `-wal` + `-shm`; postgres: `pg_dump -Fc`) under `.bsmcp-update/backups/`, records the previous image digests in `.bsmcp-update/<timestamp>.json`, then pulls and restarts only the selected services (`up -d --no-deps`; never `down`, never volumes). It waits for healthy, shows logs on failure and runs `/health` and `bsmcp-server doctor` when available. Channel and rollback pins are kept in `.bsmcp-update/override.yml`; pass it as an extra `-f` in later manual compose commands. For production pin a version tag or digest instead of `:dev`.

A rollback prints the exact restore commands but never runs them: if the new version ran a database migration, restoring the image alone may not be enough and the backup must be restored too.

## Troubleshooting from real installs

Seen on Ubuntu 24.04 with Virtualmin/Webmin:

- **Container cannot reach the public BookStack URL** (works from the host): no NAT hairpin. Add `extra_hosts: ["kb.example.com:<LAN IP of the BookStack host>"]` to bsmcp-server, bsmcp-embedder and bsmcp-worker in a compose override. HTTPS and TLS verification stay on because the hostname is unchanged.
- **Containers cannot download the Hugging Face model**: the host firewall (nftables, often managed by Webmin) blocks forwarding from Docker bridges. Allow forwarding and masquerade for `docker0`/`br-*`, restart Docker, then check with `docker run --rm curlimages/curl -sSI https://huggingface.co`.
- **`.env` values with spaces** (e.g. `BSMCP_INSTANCE_DESC`) break `source .env` unless quoted: use single quotes, `BSMCP_INSTANCE_DESC='My knowledge base'`. The install script does this for you.
- **Technical (embedder) token**: needs admin-level role permissions even though it only calls the read-only roles API.

## Connecting

The MCP endpoint URL is:

```
https://your-host/mcp/sse
```

> **Important:** Use the full path including `/mcp/sse` — not just the base domain.

### Claude.ai (Custom Connector)

1. Go to **Settings > Integrations > Add custom MCP** in Claude.ai
2. Enter the MCP endpoint URL: `https://your-host/mcp/sse`
3. A login form opens in your browser — enter your BookStack API **Token ID** and **Token Secret**
4. Once authorized, BookStack tools appear automatically in your conversations

### Claude Desktop (Custom Connector)

1. Add a custom connector with URL: `https://your-host/mcp/sse`
2. When connecting, a login form opens in your browser with instructions
3. Enter your BookStack API **Token ID** and **Token Secret**

No config files needed — authentication happens entirely through the browser via OAuth 2.1.

### Claude Code (Direct Bearer Token)

Add to your MCP server configuration:

```json
{
  "mcpServers": {
    "bookstack": {
      "url": "https://your-host/mcp/sse",
      "headers": {
        "Authorization": "Bearer YOUR_TOKEN_ID:YOUR_TOKEN_SECRET"
      }
    }
  }
}
```

The token ID and secret come from your BookStack API token (created under **My Account > Access & Security > API Tokens**).


## Diagnostics

Run the built-in doctor inside the running container:

```
docker compose exec bsmcp-server bsmcp-server doctor          # readable table
docker compose exec bsmcp-server bsmcp-server doctor --json   # machine output
```

It checks config, database, backend reachability (IPv4/IPv6 per address), internet egress (skip with `BSMCP_DOCTOR_OFFLINE=1`), the server's own OAuth metadata, the embedder, and the API token. To validate the worker's token, pass `BSMCP_INDEX_TOKEN_ID`/`BSMCP_INDEX_TOKEN_SECRET` (or the `BSMCP_EMBED_TOKEN_*` pair) with `-e`. Exit code is 1 if any check FAILs.

Not checkable from inside the container: iptables/nftables, host firewall, reverse proxy config, Docker daemon. `scripts/install.sh` covers the host-side checks.

### System card on /status

When semantic search is enabled, `/status` shows an admin-only **System** card (sign in through `/settings` first; non-admins see the page as before): server and embedder versions, embedder load state and model, last full/delta walk and deletion sweep, **last webhook received and last rejected** (a rejection means the secret header is missing or wrong, the usual reverse-proxy mistake), backup age, and a **Run diagnostics** button that runs the same checks as the CLI.
