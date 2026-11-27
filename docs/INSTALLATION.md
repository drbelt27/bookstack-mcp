# Installation and connection

[Project home](../README.md) · [Configuration](CONFIGURATION.md) · [Upgrading](MIGRATION.md)

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

