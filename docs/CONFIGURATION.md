# Configuration reference

[Project home](../README.md) · [Installation](INSTALLATION.md) · [Tools](TOOLS.md)

### Configuration

#### Server Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `BSMCP_BACKEND` | No | `bookstack` | Content backend: `bookstack` or `libstack`. Selects which URL pair below is read and which client every session gets. See [LibStack backend](#libstack-backend). |
| `BSMCP_BOOKSTACK_URL` | If bookstack | - | BookStack URL the server dials the API on. May be an internal host (Docker service name, private IP). |
| `BSMCP_BOOKSTACK_PUBLIC_URL` | No | `BSMCP_BOOKSTACK_URL` | Browser-reachable BookStack URL, used for every link shown to a human. **Set this whenever `BSMCP_BOOKSTACK_URL` is an internal host** — the failure is silent: everything works, but users get links they can't open. |
| `BSMCP_LIBSTACK_URL` | If libstack | - | LibStack URL the server dials `/api` on. May be an internal host. |
| `BSMCP_LIBSTACK_PUBLIC_URL` | No | `BSMCP_LIBSTACK_URL` | Browser-reachable LibStack URL for every link shown to a human (`/p/{id}`, `/c/{id}`). |
| `BSMCP_ENCRYPTION_KEY` | Yes | - | 32+ char key for AES-256-GCM token encryption |
| `BSMCP_DB_BACKEND` | No | `sqlite` | Database backend: `sqlite` or `postgres` |
| `BSMCP_DATABASE_URL` | If postgres | - | PostgreSQL connection string |
| `BSMCP_DB_PATH` | No | `/data/bookstack-mcp.db` | SQLite database path |
| `BSMCP_PUBLIC_DOMAIN` | No | - | Public domain for OAuth redirects (e.g. `mcp.example.com`) |
| `BSMCP_INTERNAL_DOMAIN` | No | - | Internal/Docker-network domain |
| `BSMCP_HOST` | No | `0.0.0.0` | Bind address |
| `BSMCP_PORT` | No | `8080` | Bind port |
| `BSMCP_INSTANCE_NAME` | No | - | Instance name shown to AI |
| `BSMCP_INSTANCE_DESC` | No | - | Instance description shown to AI |
| `BSMCP_SEMANTIC_SEARCH` | No | `false` | Enable semantic search tools |
| `BSMCP_EMBEDDER_URL` | No | `http://bsmcp-embedder:8081` | Embedder HTTP endpoint |
| `BSMCP_WEBHOOK_SECRET` | If semantic | - | BookStack webhook secret |
| `BSMCP_ACCESS_TOKEN_TTL` | No | `86400` | Access token TTL in seconds (24h) |
| `BSMCP_REFRESH_TOKEN_TTL` | No | `7776000` | Refresh token TTL in seconds (90d) |
| `BSMCP_BACKUP_INTERVAL` | No | - | Hours between backups (0 = disabled) |
| `BSMCP_BACKUP_PATH` | No | `/data/backups` | Backup directory |
| `BSMCP_BOOKSTACK_RATE_LIMIT_PER_MIN` | No | `180` | BookStack API request budget per MCP process. This is not a shared limit across the server, embedder, worker, or replicas. |
| `BSMCP_BOOKSTACK_PER_TOKEN_RATE_LIMIT_PER_MIN` | No | `60` | Per-credential budget in each process (initial burst: 90 requests). The Compose services all use 60 by default. |
| `BSMCP_MAX_SESSIONS_PER_TOKEN` | No | `5` | Concurrent legacy SSE sessions allowed per API token. Raise it when one token is shared by several MCP clients on a host. Zero or unparseable values fall back to the default. |
| `BSMCP_STRUCTURE_CACHE_TTL_SECS` | No | `60` | How long the `initialize` structure blurb is cached, per instance + API token. Without it every client connect re-enumerates the whole shelf tree (~1 API call per shelf). `0` disables caching. Tool responses are never cached, and a sweep that hit upstream errors is never cached. The cache is per process, so at N replicas expect the hit rate to divide by N. |

### BookStack API request limits

BookStack defaults to `API_REQUESTS_PER_MIN=180` per user. If the server, embedder and worker use the same BookStack account, their separate MCP buckets **do not add up to a shared cap**: three processes at 60/min can collectively consume 180/min, while initial bursts can exceed that. With distinct accounts, count the processes using each account separately. BookStack's own 429 response remains the final protection; MCP retries it, but a search can return incomplete results when an ACL check times out.

For the default BookStack setting, keep `BSMCP_BOOKSTACK_PER_TOKEN_RATE_LIMIT_PER_MIN=60` and `BSMCP_BOOKSTACK_RATE_LIMIT_PER_MIN=180` across the Compose services. To increase capacity:

1. On **your BookStack installation**, set `API_REQUESTS_PER_MIN=600` in BookStack's persistent configuration (for Docker, the BookStack service environment, not an MCP container). Recreate/restart BookStack and check the `X-RateLimit-Limit` response header on an authenticated `/api/books?count=1` request; it must report `600`. Do not edit a running container's `.env`: that change is lost when the container is recreated.
2. Set `BSMCP_BOOKSTACK_RATE_LIMIT_PER_MIN` and `BSMCP_BOOKSTACK_PER_TOKEN_RATE_LIMIT_PER_MIN` in the MCP Compose `.env` and recreate the MCP services. For example, with BookStack at 600 and three processes sharing one account, start at `BSMCP_BOOKSTACK_RATE_LIMIT_PER_MIN=600` and `BSMCP_BOOKSTACK_PER_TOKEN_RATE_LIMIT_PER_MIN=120` per process. Leave room for other clients and bursts; these figures are a starting point, not a guarantee.
3. Run wide and concurrent searches with the intended client accounts. Watch BookStack's HTTP 429s, MCP response `stats.acl_unverified`/`stats.warning`, latency, and BookStack CPU/RAM. Reduce limits or concurrency if results are incomplete. An ACL timeout fails closed and withholds the page; raising the limit alone does not fix a slow BookStack instance.

Each replica has its own buckets. If several replicas or other integrations share an account, plan their **aggregate** traffic against BookStack's per-user limit; these environment settings do not enforce a distributed cap. Avoid exposing a larger BookStack budget without monitoring, because it permits higher load from any API client using that account.

#### Embedder Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `BSMCP_EMBED_TOKEN_ID` | Yes | - | BookStack API token ID for crawling |
| `BSMCP_EMBED_TOKEN_SECRET` | Yes | - | BookStack API token secret |
| `BSMCP_EMBED_PROVIDER` | No | `local` | Embedding backend: `local` (fastembed ONNX), `ollama`, `openai` (or OpenAI-compatible), `voyage`. See [Embedding Providers](#embedding-providers) for per-provider config. |
| `BSMCP_EMBED_MODEL` | No | (per provider) | Model name (see [Embedding Providers](#embedding-providers)) |
| `BSMCP_EMBED_API_KEY` | If openai | - | API key for OpenAI embedding provider |
| `BSMCP_EMBED_API_URL` | No | (per provider) | Base URL for Ollama or OpenAI-compatible endpoint |
| `BSMCP_EMBED_DIMS` | No | (auto) | Embedding dimensions (auto-detected for Ollama) |
| `BSMCP_MODEL_PATH` | No | `/data/models` | ONNX model cache directory (local provider only) |
| `BSMCP_EMBED_CPUS` | No | `0` (unlimited) | Docker CPU limit for embedder |
| `BSMCP_EMBED_JOB_TIMEOUT` | No | `14400` | Seconds before stuck jobs reset |
| `BSMCP_EMBED_BATCH_SIZE` | No | `32` | Chunks per embedding batch |
| `BSMCP_EMBED_DELAY_MS` | No | `50` | Delay between pages (API throttle) |
| `BSMCP_EMBED_POLL_INTERVAL` | No | `5` | Seconds between job queue polls |
| `BSMCP_EMBED_ON_STARTUP` | No | `false` | `true` = auto-embed on startup, `clean` = clear all embeddings first |
| `BSMCP_EMBED_FAILURE_THRESHOLD` | No | `10` | Failed pages before a job is marked failed |
| `BSMCP_EMBED_CONSECUTIVE_ABORT` | No | `10` | Consecutive failures before a job aborts early |
| `BSMCP_EMBED_HOST` | No | `0.0.0.0` | Embedder listen address |
| `BSMCP_EMBED_PORT` | No | `8081` | Embedder listen port |
| `BSMCP_RERANK_PROVIDER` | No | (unset) | Cross-encoder rerank provider: `local`, `voyage`, `openai`, `none`. Off by default; enables `POST /rerank` on the embedder, which the server consumes for `semantic_search`/`search_content` `rerank: true` and `semantic_search mode: "precision"`. See [Reranker Providers](#reranker-providers). |
| `BSMCP_RERANK_MODEL` | If reranker on | (per provider) | Reranker model. Defaults: `BAAI/bge-reranker-v2-m3` (local), `rerank-2` (voyage). Required for `openai`. |
| `BSMCP_RERANK_API_KEY` | If voyage/openai | - | API key for external rerank provider. |
| `BSMCP_RERANK_API_URL` | If openai | (per provider) | Base URL. Voyage defaults to `https://api.voyageai.com`; openai requires explicit URL. |

#### Role Selector

The embedder image hosts both the embedder loop and the reconciliation worker loop. Select which runs per container via `--role` (CLI flag, primary) or `BSMCP_ROLE` (env fallback). Default is `embedder`.

| Variable / flag | Default | Description |
|---|---|---|
| `--role=embedder` / `BSMCP_ROLE=embedder` | (default) | Runs the embed job queue, the `/embed` + `/rerank` HTTP endpoints, and the cross-encoder when configured. Does NOT spawn the reconciliation worker — operators running embedder-only get no automatic index retry-chain reconciliation. |
| `--role=worker` / `BSMCP_ROLE=worker` | - | Runs the reconciliation worker only: initial full walk, periodic delta walk, lifecycle housekeeper across `index_jobs` + `embed_jobs`. No HTTP listener, no ONNX model loaded. |
| `--role=both` / `BSMCP_ROLE=both` | - | Runs both loops in one process. Useful for single-host SQLite deployments. |

The CLI flag wins over the env. Setting both (compose `command:` + `BSMCP_ROLE`) is belt-and-suspenders — recommended for clarity in compose files.

#### Worker Variables

Read when the embedder is running with `--role=worker` or `--role=both`. The worker shares the same database as the server and owns the `index_jobs` queue.

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `BSMCP_BACKEND` / `BSMCP_BOOKSTACK_URL` / `BSMCP_LIBSTACK_URL` | Yes | - | Same as server — the worker and embedder walk whichever backend the server fronts |
| `BSMCP_ENCRYPTION_KEY` | Yes | - | Must match the server's (the DB layer initializes its encryption context on every connection) |
| `BSMCP_INDEX_TOKEN_ID` | Yes* | - | Admin BookStack API token ID for the worker. Falls back to `BSMCP_EMBED_TOKEN_ID` if unset, so single-token deployments don't have to duplicate creds. |
| `BSMCP_INDEX_TOKEN_SECRET` | Yes* | - | Admin BookStack API token secret. Falls back to `BSMCP_EMBED_TOKEN_SECRET`. |
| `BSMCP_DB_BACKEND` | No | `sqlite` | Must match the server's (shared DB) |
| `BSMCP_DB_PATH` | No | `/data/bookstack-mcp.db` | SQLite path |
| `BSMCP_DATABASE_URL` | If postgres | - | PostgreSQL connection string |
| `BSMCP_INDEX_DELTA_INTERVAL_SECONDS` | No | `300` | Cadence of the periodic delta walk. `0` = disable the delta walk (webhook-driven only). |

\* Required, but the fallback to `BSMCP_EMBED_TOKEN_*` covers most setups.

#### Job Lifecycle Variables

Read by the worker role's lifecycle housekeeper. Apply to both `embed_jobs` and `index_jobs`.

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `BSMCP_JOB_TIMEOUT_SECS` | No | `3600` | Hard timeout — any job running longer than this is failed. |
| `BSMCP_JOB_RECONCILE_SECS` | No | `300` | Reconciler poll interval (scan for failed jobs and retry them). |
| `BSMCP_JOB_MAX_RETRY_CHAIN` | No | `5` | Max retry-chain length before a job is given up on. |
| `BSMCP_JOB_CLOSE_GRACE_SECS` | No | `30` | Audit-grace before succeeded/cancelled jobs are archived to closed status. |

See `.env.example` for the full list with comments.

### Semantic Search Setup

1. Set `BSMCP_SEMANTIC_SEARCH=true` in your server env
2. Set `BSMCP_WEBHOOK_SECRET` to a random string (16+ characters)
3. Create a BookStack API token with read access for the embedder (`BSMCP_EMBED_TOKEN_ID` / `BSMCP_EMBED_TOKEN_SECRET`)
4. Start the embedder container — it downloads the ONNX model (~1.3GB) on first run
5. Use the `reembed` tool (via Claude) to trigger initial embedding of all pages
6. Configure a webhook in BookStack for automatic re-embedding on page changes:

#### BookStack Webhook Configuration

Go to **Settings > Webhooks > Create Webhook** in your BookStack instance:

| Field | Value |
|-------|-------|
| **Name** | MCP Semantic Search |
| **Endpoint URL** | `https://your-mcp-host/webhooks/bookstack` |
| **Active** | Yes |

**Events to select:**
- Page Create
- Page Update
- Page Delete

**Custom header** (required for verification):
```
X-Webhook-Secret: YOUR_WEBHOOK_SECRET
```

The `YOUR_WEBHOOK_SECRET` value must match `BSMCP_WEBHOOK_SECRET` in your server environment. The server uses constant-time comparison to verify the header.

After saving, any page create/update/delete in BookStack automatically queues a re-embedding job. The embedder picks it up within seconds (configurable via `BSMCP_EMBED_POLL_INTERVAL`).

### Reranker Setup (optional — for the `rerank: true` flag and `mode: "precision"`)

The cross-encoder reranker is off by default. `semantic_search` (default `mode: "standard"`, `rerank: false`) and `search_content` (default `rerank: false`) both work fine without it. To enable the `rerank: true` flag on either tool, or to use `mode: "precision"` on `semantic_search`:

1. Pick a provider and set `BSMCP_RERANK_PROVIDER` on the **embedder** to one of:
   - **`local`** — in-process ONNX cross-encoder via fastembed. No API key. Default model: `BAAI/bge-reranker-v2-m3` (~600 MB, downloads on first run). Reuses `BSMCP_MODEL_PATH` for the cache directory.
   - **`voyage`** — Voyage AI's `/v1/rerank`. Set `BSMCP_RERANK_API_KEY`. Default model: `rerank-2`.
   - **`openai`** — any OpenAI-shape `/v1/rerank` endpoint (Voyage, Jina, Cohere-via-shim, self-hosted). Requires all of `BSMCP_RERANK_API_KEY`, `BSMCP_RERANK_API_URL`, and `BSMCP_RERANK_MODEL` (no upstream default — OpenAI itself has not shipped a rerank API).
2. Restart the embedder. On startup it logs `Reranker: <provider> <model>` and starts answering `POST /rerank`. Without configuration it logs `Reranker: disabled (BSMCP_RERANK_PROVIDER unset or 'none')` and `/rerank` returns 503.
3. Use the reranker from a tool call:
   - **`semantic_search` with `mode: "standard", rerank: true`** — runs the standard pipeline, then the cross-encoder re-orders the top-N. Equivalent to the pre-v0.13.0 `mode: "rerank"`.
   - **`semantic_search` with `mode: "precision"`** — wider candidate pool, cross-encoder is the ranker of record (4-stage cascade; rerank is always on).
   - **`search_content` with `rerank: true`** — BookStack keyword search, then the cross-encoder re-orders the matched results.
   In all three cases the response includes `scoring.rerank` per result and `stats.{rerank_ms, rerank_provider, rerank_model, candidates_reranked}`. If the reranker is disabled, the server surfaces a clear error pointing at `BSMCP_RERANK_PROVIDER` so callers can drop the flag and retry.

Per-provider config blocks are documented under [Reranker Providers](#reranker-providers).

### LibStack backend

Phase 1 of the LibStack migration ([design](https://kb.beesroadhouse.com/link/2507), issue #156). `BookStackClient` sits behind a `Backend` trait in `bsmcp-common`, and `LibStackClient` implements the same trait against LibStack's JSON `/api`. The dispatcher, the semantic-search ACL path, the embedder and the reconciliation worker all go through the trait, so every tool name, every connector and the semantic index keep working when the household moves off BookStack.

```
BSMCP_BACKEND=libstack
BSMCP_LIBSTACK_URL=http://libstack:8080          # where /api is dialed
BSMCP_LIBSTACK_PUBLIC_URL=https://libstack.example.com   # links shown to people (optional)
```

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `BSMCP_BACKEND` | No | `bookstack` | `bookstack` (unchanged behaviour) or `libstack`. |
| `BSMCP_LIBSTACK_URL` | If libstack | - | API host. `BSMCP_BOOKSTACK_URL` is ignored on this backend. |
| `BSMCP_LIBSTACK_PUBLIC_URL` | No | `BSMCP_LIBSTACK_URL` | Browser-reachable host for the `url` fields and the links in tool output. |

**Credentials.** LibStack issues one bearer token per user (`/api/auth/tokens`). The MCP keeps its id/secret credential path (OAuth form, encrypted token store, `Authorization: Bearer id:secret`): paste the LibStack token as the **Token ID** and leave the secret blank, or send it bare as `Authorization: Bearer <token>` from Claude Code. A pair is sent as `id:secret`. The embedder and worker take theirs from `BSMCP_EMBED_TOKEN_ID` / `BSMCP_INDEX_TOKEN_ID` the same way.

**Ids.** LibStack ids are UUIDs; the tool surface and the index are integer-keyed. Every UUID maps to a stable integer (its top 63 bits) and every response also carries the UUID as `libstack_id`. An integer the process has not seen in a listing is resolved by re-listing collections and pages once; an id that is still unknown fails with a message saying so (list first).

**Mapping.**

| BookStack tool surface | LibStack |
|---|---|
| Shelf | Root collection (depth 0) |
| Book | Depth-1 collection. `create_book` needs `shelf_id` here. |
| Chapter | Depth-2 collection. Deeper collections are chapters of the same book, named by their path from depth 2 (`Ops / Runbooks / Backups`). |
| Page | Page. `markdown` is the body, `html` carries LibStack's `rendered_html`, `editor` is always `markdown`; `html` input is refused. |
| `move_page`, `move_chapter`, `move_book_to_shelf`, `update_shelf.books` | `collection_id` / `parent_id` change. A collection has one parent, so `update_shelf.books` re-parents the listed books and leaves the rest. |
| Tags | Page tags (`name` / `value`) |
| Attachments | `/api/pages/{id}/attachments`, bytes at `/blobs/{id}`. `list_attachments` needs `page_id`; link attachments are not available. |
| Images | Attachments whose MIME type is `image/*`. `list_images` needs `uploaded_to`; `upload_image` stores a blob and returns its `/blobs/{id}` URL. |
| Comments | `/api/pages/{id}/comments`, markdown bodies. `list_comments` needs `page_id`. |
| Search | `/api/search` hits mapped onto BookStack's result rows. BookStack operator tokens (`{type:page}`, `[tag=value]`) are stripped; LibStack's own grammar (quoted phrases, `-negation`, `OR`) applies. Fixed 50-hit cap, paged client-side. |
| Users | `/api/auth/me` only — `list_users` returns the caller, `get_user` works for the caller's id. |
| Audit log | `/api/activity` |
| System info | `/api/health` plus the public URL |
| `list_collections`, `get_collection`, `create_collection` | The real tree: parent, depth, path, children, pages. New on this backend. |

A page that lives directly in a root collection reports that collection as its `book_id`; `get_book` accepts any collection id, and the worker's orphan sweep walks such roots as books so their pages are indexed.

**Not available on LibStack** (each returns `… is not available on LibStack`, never a fake success): `get_content_permissions`, `update_content_permissions`, `list_roles`, `get_role`, `list_recycle_bin`, `restore_recycle_bin_item`, `destroy_recycle_bin_item`, `export_page`, `export_chapter`, `export_book`, `create_attachment` (link attachments). The semantic-search ACL prefilter is off on LibStack (no roles); every candidate page is checked with a live `GET /api/pages/{id}`, which LibStack's row-level security answers.

**Semantic search** works unchanged: the embedder lists and reads pages through the backend, and the worker's delta walk uses `/api/pages` sorted by `updated_at` with a client-side cut (the endpoint has no filter yet). BookStack webhooks have no LibStack counterpart; the periodic delta walk is what keeps the index current.

## Embedding Providers

Set via `BSMCP_EMBED_PROVIDER`. Changing provider or model triggers an automatic clean re-index.

### Local (default)

Uses fastembed with ONNX Runtime. No external API needed but requires the heavier embedder container.

| Model Name | Dimensions | Parameters | Notes |
|------------|-----------|------------|-------|
| `BAAI/bge-base-en-v1.5` | 768 | 110M | **Default.** Good balance of speed and quality. |
| `BAAI/bge-large-en-v1.5` | 1024 | 335M | Highest quality, heavier. |
| `BAAI/bge-small-en-v1.5` | 384 | 33M | Fastest, lower quality. |
| `embeddinggemma-300m` | 768 | 300M | Google's lightweight model. |
| `intfloat/multilingual-e5-small` | 384 | n/a (~450 MB ONNX download) | Multilingual, CPU-friendly. Query/passage prefixes and a model-specific default threshold of 0.83 are applied automatically when `threshold` is omitted. Calibrate the threshold on your own content. |

### Ollama

Uses a local or remote Ollama instance. Dimensions auto-detected. No API key needed.

```bash
BSMCP_EMBED_PROVIDER=ollama
BSMCP_EMBED_MODEL=nomic-embed-text        # or any Ollama embedding model
BSMCP_EMBED_API_URL=http://ollama:11434    # default: http://localhost:11434
```

### OpenAI

Uses OpenAI's embedding API or any OpenAI-compatible endpoint.

```bash
BSMCP_EMBED_PROVIDER=openai
BSMCP_EMBED_MODEL=text-embedding-3-small   # default
BSMCP_EMBED_API_KEY=sk-...
BSMCP_EMBED_DIMS=1536                      # must match model output
BSMCP_EMBED_API_URL=https://api.openai.com # or any compatible endpoint
```

### Voyage

Uses Voyage AI's embedding API.

```bash
BSMCP_EMBED_PROVIDER=voyage
BSMCP_EMBED_MODEL=voyage-3-lite            # default
BSMCP_EMBED_API_KEY=pa-...
BSMCP_EMBED_DIMS=512                       # must match model output (voyage-3-lite = 512)
BSMCP_EMBED_API_URL=https://api.voyageai.com  # default
```

## Reranker Providers

Set via `BSMCP_RERANK_PROVIDER` on the **embedder**. Off by default. See [Reranker Setup](#reranker-setup-optional--for-the-rerank-true-flag-and-mode-precision) for the activation walkthrough.

### Local (default model: `BAAI/bge-reranker-v2-m3`)

In-process ONNX cross-encoder via fastembed. No external API needed. Downloads the model to `BSMCP_MODEL_PATH` on first run.

| Model Name | Notes |
|------------|-------|
| `BAAI/bge-reranker-base` | Smaller, faster |
| `BAAI/bge-reranker-v2-m3` | **Default.** Multilingual, strong quality. |
| `jinaai/jina-reranker-v1-turbo-en` | English-only, optimized for latency |
| `jinaai/jina-reranker-v2-base-multilingual` | Multilingual alternative to bge-v2-m3 |

```bash
BSMCP_RERANK_PROVIDER=local
BSMCP_RERANK_MODEL=BAAI/bge-reranker-v2-m3   # default; reuses BSMCP_MODEL_PATH
```

### Voyage

Uses Voyage AI's `/v1/rerank` endpoint.

```bash
BSMCP_RERANK_PROVIDER=voyage
BSMCP_RERANK_MODEL=rerank-2                # default
BSMCP_RERANK_API_KEY=pa-...
BSMCP_RERANK_API_URL=https://api.voyageai.com   # default
```

### OpenAI-compatible (Voyage / Jina / Cohere via shim / self-hosted)

Any endpoint that accepts `{model, query, documents}` and returns `{data:[{index, relevance_score}]}` (or `{results:[...]}`). OpenAI itself has not shipped a rerank API — this provider is for compatible third parties. All three vars required (no upstream default).

```bash
BSMCP_RERANK_PROVIDER=openai
BSMCP_RERANK_MODEL=jina-reranker-v2-base-multilingual
BSMCP_RERANK_API_KEY=...
BSMCP_RERANK_API_URL=https://api.jina.ai
```

