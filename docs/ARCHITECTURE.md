# Architecture and features

[Project home](../README.md) · [Configuration](CONFIGURATION.md)

## Features

- Full CRUD on all core BookStack resources (shelves, books, chapters, pages, attachments)
- Full-text search with BookStack query operators
- **Semantic vector search** — natural language search across all content via embeddings (optional). Two modes on `semantic_search`: `standard` (default, vector + keyword + Markov-blanket blend) and `precision` (4-stage cascade — semantic → keyword → Markov-blanket → cross-encoder). An optional `rerank: bool` flag layers a cross-encoder pass on top of the standard top-N (v0.13.0; replaces the pre-v0.13.0 `mode: "rerank"`). Per-page access control enforced via BookStack's API on every result.
- **Settings UI (`/settings`)** — browser-based admin configuration page (token-gated via the same `/authorize` flow). Surfaces only the global server fields the index worker needs (`hive_shelf_id`, `user_journals_shelf_id`).
- **Pluggable database** — SQLite for simple deployments, PostgreSQL + pgvector for production
- **Separate embedder** — background embedding service with pluggable backends (local ONNX, Ollama, OpenAI, Voyage)
- **Cross-encoder reranker (optional)** — embedder exposes `POST /rerank` when `BSMCP_RERANK_PROVIDER` is configured. Three providers: `local` (in-process ONNX cross-encoder via fastembed, default `BAAI/bge-reranker-v2-m3`), `voyage` (Voyage's `/v1/rerank`), `openai` (any OpenAI-shape rerank endpoint — covers Voyage/Jina/Cohere-via-shim/self-hosted). Off by default; consumed by `semantic_search`'s `rerank: true` flag (refinement on the standard mode) + `mode: "precision"` (cascade), and by `search_content`'s `rerank: true` flag (v0.13.0).
- **Server-side markdown to HTML conversion** — send markdown, server converts before sending to BookStack
- **Staging upload flow** — upload local images and attachments through a two-step staging endpoint without exposing local paths to the container ([see below](TOOLS.md#uploading-local-files-images--attachments))
- **OAuth 2.1 support** — use as a Claude.ai or Claude Desktop custom connector without config files
- **Encrypted token storage** — OAuth tokens encrypted at rest with AES-256-GCM
- **Dual transport** — SSE (MCP 2024-11-05) and Streamable HTTP (MCP 2025-03-26)
- **Dynamic structure discovery** — AI automatically learns your BookStack hierarchy on connect
- **Auto-migration** — seamlessly migrate from SQLite to PostgreSQL on startup
- Multi-user support via per-session BookStack API tokens
- Multi-arch Docker images (amd64 + arm64)

## Architecture

```
crates/
  bsmcp-common/       Shared types, traits, config, chunking, vector utils
  bsmcp-db-sqlite/    SQLite backend (rusqlite, bundled)
  bsmcp-db-postgres/  PostgreSQL + pgvector backend (sqlx)
  bsmcp-server/       MCP server binary (axum, no ONNX dependency)
  bsmcp-embedder/     Embedder + reconciliation worker (single binary, role-selected via --role flag)
                      — local ONNX / Ollama / OpenAI / Voyage embedding, job queue worker, HTTP /embed + optional /rerank
                      — reconciliation worker: initial walk + webhook/cron delta walk on the index_jobs queue

docker/
  Dockerfile.server       Lightweight server image (~35MB)
  Dockerfile.embedder     Embedder + worker image with ONNX Runtime (~45MB)
  docker-compose.yml      PostgreSQL deployment (production)
  docker-compose.sqlite.yml  SQLite deployment (simple)
```

The MCP server handles all client-facing protocol, OAuth, and search. The embedder runs separately, polling a database-backed job queue to embed pages and serving a `/embed` HTTP endpoint for query-time embedding (and `/rerank` when a reranker provider is configured). The embedder supports four embedding backends: local ONNX models (fastembed), Ollama, OpenAI-compatible APIs, and Voyage. The reconciliation worker (same binary, run with `--role=worker`) owns the `index_jobs` queue — runs the initial full walk on cold start, then consumes webhook + cron jobs and the periodic delta walk. Run as two compose services (separate embedder + worker) or as one with `--role=both`.

