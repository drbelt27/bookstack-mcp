//! `bsmcp-server doctor [--json]`: read-only diagnostics, run inside the
//! container. Never prints secret values (only presence / length class).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bsmcp_common::backend::{BackendConfig, BackendKind};
use bsmcp_common::config::DbBackendType;
use bsmcp_common::db::{DbBackend, IndexDb, SemanticDb};
use bsmcp_common::net_error;
use reqwest::Client;
use serde_json::{json, Value};

type Env = HashMap<String, String>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Checked,
    Hypothesis,
}

#[derive(Debug)]
pub struct Check {
    pub id: String,
    pub status: Status,
    pub kind: Kind,
    pub cause: String,
    pub impact: String,
    pub fix: String,
    pub commands: Vec<String>,
}

impl Check {
    fn new(id: &str, status: Status, cause: impl Into<String>) -> Self {
        Check {
            id: id.into(),
            status,
            kind: Kind::Checked,
            cause: cause.into(),
            impact: String::new(),
            fix: String::new(),
            commands: vec![],
        }
    }
    fn hypothesis(mut self) -> Self {
        self.kind = Kind::Hypothesis;
        self
    }
    fn impact(mut self, s: &str) -> Self {
        self.impact = s.into();
        self
    }
    fn fix(mut self, s: &str) -> Self {
        self.fix = s.into();
        self
    }
    fn cmd(mut self, s: impl Into<String>) -> Self {
        self.commands.push(s.into());
        self
    }
}

const NOT_CHECKABLE: &[&str] = &[
    "iptables / nftables rules on the host",
    "host firewall (ufw, firewalld, cloud security groups)",
    "reverse proxy configuration (nginx, Caddy, Traefik)",
    "Docker daemon state, networks and published ports",
];
const HOST_NOTE: &str = "scripts/install.sh covers the host-side checks.";

// ---------- pure helpers ----------

/// 1 if any FAIL, else 0.
pub fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|c| c.status == Status::Fail))
}

fn count(checks: &[Check], s: Status) -> usize {
    checks.iter().filter(|c| c.status == s).count()
}

pub fn to_json(checks: &[Check]) -> Value {
    json!({
        "version": 1,
        "summary": {
            "pass": count(checks, Status::Pass),
            "warn": count(checks, Status::Warn),
            "fail": count(checks, Status::Fail),
            "skip": count(checks, Status::Skip),
        },
        "exit_code": exit_code(checks),
        "not_checkable_from_container": NOT_CHECKABLE,
        "note": HOST_NOTE,
        "checks": checks.iter().map(|c| json!({
            "id": c.id,
            "status": c.status.as_str(),
            "kind": if c.kind == Kind::Checked { "checked" } else { "hypothesis" },
            "cause": c.cause,
            "impact": c.impact,
            "fix": c.fix,
            "commands": c.commands,
        })).collect::<Vec<_>>(),
    })
}

pub fn to_table(checks: &[Check]) -> String {
    let w = checks.iter().map(|c| c.id.len()).max().unwrap_or(2).max(2);
    let mut out = format!("{:<w$}  {:<6} {:<10} CAUSE\n", "ID", "STATUS", "KIND");
    for c in checks {
        let k = if c.kind == Kind::Checked {
            "checked"
        } else {
            "hypothesis"
        };
        out += &format!(
            "{:<w$}  {:<6} {:<10} {}\n",
            c.id,
            c.status.as_str(),
            k,
            c.cause
        );
    }
    for c in checks
        .iter()
        .filter(|c| matches!(c.status, Status::Warn | Status::Fail))
    {
        out += &format!("\n[{}] {}\n", c.status.as_str(), c.id);
        if !c.impact.is_empty() {
            out += &format!("  impact: {}\n", c.impact);
        }
        if !c.fix.is_empty() {
            out += &format!("  fix:    {}\n", c.fix);
        }
        for cmd in &c.commands {
            out += &format!("  $ {cmd}\n");
        }
    }
    out += &format!(
        "\nSummary: {} pass, {} warn, {} fail, {} skip\n",
        count(checks, Status::Pass),
        count(checks, Status::Warn),
        count(checks, Status::Fail),
        count(checks, Status::Skip)
    );
    out += "\nNOT checkable from inside the container:\n";
    for n in NOT_CHECKABLE {
        out += &format!("  - {n}\n");
    }
    out += &format!("{HOST_NOTE}\n");
    out
}

fn env_get<'a>(env: &'a Env, k: &str) -> Option<&'a str> {
    env.get(k).map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn truthy(env: &Env, k: &str) -> bool {
    env_get(env, k).is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// Config check on an injected env map. Reports presence / length class only.
pub fn check_config(env: &Env) -> Check {
    let mut fails: Vec<String> = vec![];
    let mut warns: Vec<String> = vec![];
    match env_get(env, "BSMCP_ENCRYPTION_KEY") {
        None => fails.push("BSMCP_ENCRYPTION_KEY missing".into()),
        Some(k) if k.len() < 32 => fails.push("BSMCP_ENCRYPTION_KEY shorter than 32 chars".into()),
        Some(_) => {}
    }
    let kind = BackendKind::parse(env_get(env, "BSMCP_BACKEND").unwrap_or(""));
    let url_var = match kind {
        BackendKind::BookStack => "BSMCP_BOOKSTACK_URL",
        BackendKind::LibStack => "BSMCP_LIBSTACK_URL",
    };
    for var in [
        url_var,
        "BSMCP_BOOKSTACK_PUBLIC_URL",
        "BSMCP_LIBSTACK_PUBLIC_URL",
    ] {
        match env_get(env, var) {
            None if var == url_var => fails.push(format!("{var} missing")),
            None => {}
            Some(v) => match reqwest::Url::parse(v) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => {}
                _ => fails.push(format!("{var} is not a valid http(s) URL")),
            },
        }
    }
    match env_get(env, "BSMCP_PUBLIC_DOMAIN") {
        None => warns.push(
            "BSMCP_PUBLIC_DOMAIN unset (no clickable links, OAuth URLs derived from Host)".into(),
        ),
        Some(d) if d.contains("://") => fails
            .push("BSMCP_PUBLIC_DOMAIN must be a bare host (the server prepends https://)".into()),
        Some(d) => {
            if reqwest::Url::parse(&format!("https://{d}")).map_or(true, |u| u.host().is_none()) {
                fails.push("BSMCP_PUBLIC_DOMAIN is not a valid host[:port]".into());
            }
        }
    }
    if truthy(env, "BSMCP_SEMANTIC_SEARCH") {
        match env_get(env, "BSMCP_WEBHOOK_SECRET") {
            None => {
                fails.push("BSMCP_WEBHOOK_SECRET missing (required with semantic search)".into())
            }
            Some(s) if s.len() < 16 => {
                fails.push("BSMCP_WEBHOOK_SECRET shorter than 16 chars".into())
            }
            Some(_) => {}
        }
    }
    if DbBackendType::from_env_str(env_get(env, "BSMCP_DB_BACKEND")) == DbBackendType::Postgres
        && env_get(env, "BSMCP_DATABASE_URL").is_none()
    {
        fails.push("BSMCP_DATABASE_URL missing (BSMCP_DB_BACKEND=postgres)".into());
    }
    let fix = "Set the listed variables in .env / docker-compose.yml (see .env.example) and recreate the container.";
    if !fails.is_empty() {
        let all: Vec<String> = fails.into_iter().chain(warns).collect();
        Check::new("config", Status::Fail, all.join("; "))
            .impact("server cannot start or links/OAuth are wrong")
            .fix(fix)
    } else if !warns.is_empty() {
        Check::new("config", Status::Warn, warns.join("; ")).fix(fix)
    } else {
        Check::new(
            "config",
            Status::Pass,
            "required variables present and well-formed (values not shown)",
        )
    }
}

/// Table-driven classification of one token probe. `status` is the HTTP
/// status, or `Err(kind)` for a network failure (net_error kind).
pub fn classify_probe(
    id: &str,
    capability: &str,
    outcome: Result<u16, &str>,
    empty: bool,
) -> Check {
    let cid = format!("token.{id}");
    match outcome {
        Err(k) => Check::new(&cid, Status::Fail, format!("backend unreachable ({k})"))
            .impact("worker/server cannot reach the backend API")
            .fix("See the backend_api check."),
        Ok(200) if empty => Check::new(&cid, Status::Warn, "OK but no visible content for this token")
            .impact("nothing to index for this resource type")
            .fix("Check the token's role can view content, or that content exists."),
        Ok(200) => Check::new(&cid, Status::Pass, "OK"),
        Ok(401) => Check::new(&cid, Status::Fail, "401: token id/secret invalid or revoked")
            .impact("every API call with this token fails")
            .fix("Create a new API token (user must have 'Access system API' permission) and update the env."),
        Ok(403) => Check::new(&cid, Status::Fail, format!("403: token is valid but lacks the capability '{capability}'"))
            .impact("indexing / ACL resolution cannot use this resource")
            .fix(&format!("Grant the token's user a role with '{capability}'.")),
        Ok(404) | Ok(405) => Check::new(&cid, Status::Warn, format!("{}: endpoint not supported by this backend version", outcome.unwrap_or(0)))
            .impact("feature depending on this endpoint is unavailable")
            .fix("Upgrade the backend or ignore if the feature is unused."),
        Ok(s) => Check::new(&cid, Status::Warn, format!("unexpected HTTP status {s}")),
    }
}

pub fn is_local_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified()
        }
        IpAddr::V6(a) => {
            let s = a.segments()[0];
            a.is_loopback()
                || a.is_unspecified()
                || (s & 0xfe00) == 0xfc00
                || (s & 0xffc0) == 0xfe80
        }
    }
}

/// `(failed, stuck)` open-job counts from `(status, started_at, resolved_status)`.
pub fn job_health(
    jobs: &[(String, Option<i64>, Option<String>)],
    now: i64,
    stuck_after: i64,
) -> (usize, usize) {
    let failed = jobs
        .iter()
        .filter(|(s, _, r)| s == "failed" && r.is_none())
        .count();
    let stuck = jobs
        .iter()
        .filter(|(s, st, _)| s == "running" && st.is_some_and(|t| now - t > stuck_after))
        .count();
    (failed, stuck)
}

// ---------- network ----------

struct Resp {
    status: u16,
    body: String,
}

fn client(extra: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder) -> Client {
    extra(
        Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5)),
    )
    .build()
    .expect("client")
}

/// Err carries the net_error kind.
async fn fetch(
    c: &Client,
    url: &str,
    auth: Option<&str>,
    host: Option<&str>,
) -> Result<Resp, &'static str> {
    let mut r = c.get(url);
    if let Some(a) = auth {
        r = r.header("Authorization", a);
    }
    if let Some(h) = host {
        r = r.header("Host", h);
    }
    match r.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Ok(Resp { status, body })
        }
        Err(e) => Err(net_error::describe(&e).0),
    }
}

struct UrlReport {
    reachable: bool,
    local: bool,
    detail: String,
}

/// Resolve the host and try every address separately (max 4) so the report
/// says which IP family works.
async fn probe_url(url: &str) -> UrlReport {
    let Ok(u) = reqwest::Url::parse(url) else {
        return UrlReport {
            reachable: false,
            local: false,
            detail: "invalid URL".into(),
        };
    };
    let host = u.host_str().unwrap_or("").to_string();
    let port = u.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(a) => a.take(4).collect(),
        Err(e) => {
            return UrlReport {
                reachable: false,
                local: false,
                detail: format!("dns: {e}"),
            }
        }
    };
    let local = addrs.iter().any(|a| is_local_ip(&a.ip()));
    let mut parts = vec![];
    let (mut v4, mut v6) = (false, false);
    for a in &addrs {
        let c = client(|b| b.resolve(&host, *a).timeout(Duration::from_secs(6)));
        let fam = if a.is_ipv4() { "IPv4" } else { "IPv6" };
        match fetch(&c, url, None, None).await {
            Ok(r) => {
                if a.is_ipv4() {
                    v4 = true
                } else {
                    v6 = true
                }
                parts.push(format!("{} ({fam}): HTTP {}", a.ip(), r.status));
            }
            Err(k) => parts.push(format!("{} ({fam}): {k}", a.ip())),
        }
    }
    let fams = match (v4, v6) {
        (true, true) => "IPv4+IPv6 work",
        (true, false) => "only IPv4 works",
        (false, true) => "only IPv6 works",
        _ => "no address works",
    };
    UrlReport {
        reachable: v4 || v6,
        local,
        detail: format!("{fams}; {}", parts.join(", ")),
    }
}

async fn check_backend(env: &Env, cfg: &BackendConfig) -> Vec<Check> {
    let _ = env;
    let api = probe_url(&cfg.api_url).await;
    let api_check = if api.reachable {
        Check::new(
            "backend_api",
            Status::Pass,
            format!("{} -> {}", cfg.api_url, api.detail),
        )
    } else {
        Check::new("backend_api", Status::Fail, format!("{} unreachable: {}", cfg.api_url, api.detail))
            .impact("all content tools, indexing and token validation fail")
            .fix("Verify the URL, that the backend is running and reachable from this container's network.")
            .cmd(format!("docker compose exec bsmcp-server getent hosts {}", host_of(&cfg.api_url)))
    };
    let public_check = if cfg.public_url == cfg.api_url {
        Check::new(
            "backend_public",
            Status::Skip,
            "public URL equals the API URL",
        )
    } else {
        let p = probe_url(&cfg.public_url).await;
        if p.reachable {
            Check::new(
                "backend_public",
                Status::Pass,
                format!("{} -> {}", cfg.public_url, p.detail),
            )
        } else if api.reachable || p.local {
            Check::new(
                "backend_public",
                Status::Warn,
                format!(
                    "{} unreachable from the container ({}) while {}: likely NAT hairpin / split-DNS problem",
                    cfg.public_url,
                    p.detail,
                    if api.reachable { "the internal URL works" } else { "the host resolves to a local address" }
                ),
            )
            .hypothesis()
            .impact("only matters if something in the container must call the public URL; emitted links are unaffected")
            .fix("Point internal traffic at the internal URL, or add split-DNS / extra_hosts so the public name resolves inside Docker.")
        } else {
            Check::new(
                "backend_public",
                Status::Warn,
                format!("{} unreachable: {}", cfg.public_url, p.detail),
            )
        }
    };
    vec![api_check, public_check]
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default()
}

async fn check_internet(env: &Env) -> Check {
    if truthy(env, "BSMCP_DOCTOR_OFFLINE") {
        return Check::new("internet_egress", Status::Skip, "BSMCP_DOCTOR_OFFLINE=1");
    }
    match fetch(&client(|b| b), "https://example.com", None, None).await {
        Ok(r) if r.status < 500 => Check::new("internet_egress", Status::Pass, format!("HTTPS GET example.com -> {}", r.status)),
        Ok(r) => Check::new("internet_egress", Status::Warn, format!("example.com answered {}", r.status)),
        Err(k) => Check::new("internet_egress", Status::Warn, format!("HTTPS egress failed ({k})"))
            .impact("only matters for cloud embedding/rerank providers and OAuth discovery by external clients")
            .fix("Ignore for fully local setups; otherwise check DNS/proxy/firewall (host side: scripts/install.sh).")
            .cmd("docker compose exec bsmcp-server getent hosts example.com"),
    }
}

async fn check_self(env: &Env) -> Vec<Check> {
    let port = env_get(env, "BSMCP_PORT").unwrap_or("8080");
    let base = format!("http://127.0.0.1:{port}");
    let domain = env_get(env, "BSMCP_PUBLIC_DOMAIN");
    let c = client(|b| b);
    let mut out = vec![];
    match fetch(&c, &format!("{base}/health"), None, None).await {
        Ok(r) if r.status == 200 => {
            out.push(Check::new("mcp_health", Status::Pass, "/health -> 200"))
        }
        Ok(r) => out.push(Check::new(
            "mcp_health",
            Status::Fail,
            format!("/health -> {}", r.status),
        )),
        Err(k) => {
            return vec![Check::new(
                "mcp_health",
                Status::Fail,
                format!("own /health unreachable ({k}) on {base}"),
            )
            .impact("is the server process running? doctor probes it on 127.0.0.1")
            .fix("Run doctor inside the running bsmcp-server container; check BSMCP_PORT.")]
        }
    }
    for (id, path, key) in [
        (
            "mcp_oauth_metadata",
            "/.well-known/oauth-authorization-server",
            "issuer",
        ),
        (
            "mcp_resource_metadata",
            "/.well-known/oauth-protected-resource",
            "resource",
        ),
    ] {
        let r = fetch(&c, &format!("{base}{path}"), None, domain).await;
        out.push(match r {
            Err(k) => Check::new(id, Status::Fail, format!("{path} unreachable ({k})")),
            Ok(r) if r.status != 200 => Check::new(id, Status::Fail, format!("{path} -> {}", r.status)),
            Ok(r) => {
                let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
                let got = v[key].as_str().unwrap_or("");
                match domain {
                    None => Check::new(id, Status::Warn, format!("{path} -> 200, but BSMCP_PUBLIC_DOMAIN unset: cannot verify URLs")),
                    Some(d) if got == format!("https://{}", d.trim_end_matches('/')) => {
                        Check::new(id, Status::Pass, format!("{path} -> 200, {key} matches BSMCP_PUBLIC_DOMAIN"))
                    }
                    Some(_) => Check::new(id, Status::Fail, format!("{path} -> 200 but {key} does not match https://BSMCP_PUBLIC_DOMAIN (got '{got}')"))
                        .impact("OAuth clients (claude.ai) will be sent to the wrong URLs")
                        .fix("Set BSMCP_PUBLIC_DOMAIN to the bare public hostname (no scheme)."),
                }
            }
        });
    }
    out
}

// ---------- database & semantic ----------

type Dbs = (
    Arc<dyn DbBackend>,
    Option<Arc<dyn SemanticDb>>,
    Arc<dyn IndexDb>,
);

#[allow(clippy::result_large_err)] // one-shot CLI, Check is the report item
async fn open_db(env: &Env) -> Result<Dbs, Check> {
    let key = env_get(env, "BSMCP_ENCRYPTION_KEY")
        .unwrap_or("")
        .to_string();
    let bad = |m: String| {
        Check::new("database", Status::Fail, m)
            .impact("tokens, sessions and semantic index unavailable")
    };
    match DbBackendType::from_env_str(env_get(env, "BSMCP_DB_BACKEND")) {
        DbBackendType::Sqlite => {
            let path =
                PathBuf::from(env_get(env, "BSMCP_DB_PATH").unwrap_or("/data/bookstack-mcp.db"));
            if !path.exists() {
                return Err(
                    bad(format!("SQLite file {} does not exist", path.display()))
                        .fix("Check the /data volume mount and BSMCP_DB_PATH."),
                );
            }
            let dir = path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let probe = dir.join(".doctor-write-test");
            if std::fs::write(&probe, b"")
                .and_then(|_| std::fs::remove_file(&probe))
                .is_err()
            {
                return Err(bad(format!(
                    "directory {} is not writable (SQLite WAL needs it)",
                    dir.display()
                ))
                .fix("Fix volume ownership/permissions for the user running the container."));
            }
            let p = path.clone();
            let db = std::panic::catch_unwind(move || bsmcp_db_sqlite::SqliteDb::open(&p, &key))
                .map_err(
                    |_| bad("SQLite open failed (corrupt file or wrong permissions)".into()),
                )?;
            let db = Arc::new(db);
            Ok((db.clone() as _, Some(db.clone() as _), db as _))
        }
        DbBackendType::Postgres => {
            let url = env_get(env, "BSMCP_DATABASE_URL").unwrap_or("");
            // error text is not echoed: sqlx errors can include the connection URL
            let db = bsmcp_db_postgres::PostgresDb::new(url, &key)
                .await
                .map_err(|_| {
                    bad("PostgreSQL connection failed".into()).fix(
                        "Check BSMCP_DATABASE_URL, that postgres is up and credentials are right.",
                    )
                })?;
            let db = Arc::new(db);
            Ok((db.clone() as _, Some(db.clone() as _), db as _))
        }
    }
}

async fn check_database(env: &Env, dbs: &Result<Dbs, Check>) -> Check {
    let _ = env;
    match dbs {
        Err(c) => Check::new(&c.id, c.status, c.cause.clone())
            .impact(&c.impact)
            .fix(&c.fix),
        Ok((db, _, _)) => match db.get_global_settings().await {
            Ok(_) => Check::new(
                "database",
                Status::Pass,
                "opened and queried (global settings read)",
            )
            .cmd("(free disk space: not checked, no std API; use `df -h /data` in the container)"),
            Err(e) => Check::new(
                "database",
                Status::Fail,
                format!("query failed: {}", e.chars().take(120).collect::<String>()),
            )
            .impact("tokens, sessions and settings unavailable"),
        },
    }
}

async fn check_semantic(env: &Env, dbs: &Result<Dbs, Check>) -> Vec<Check> {
    if !truthy(env, "BSMCP_SEMANTIC_SEARCH") {
        return vec![Check::new(
            "semantic",
            Status::Skip,
            "BSMCP_SEMANTIC_SEARCH is off",
        )];
    }
    let base = env_get(env, "BSMCP_EMBEDDER_URL")
        .unwrap_or("http://bsmcp-embedder:8081")
        .trim_end_matches('/')
        .to_string();
    let c = client(|b| b);
    let mut out = vec![];
    match fetch(&c, &format!("{base}/health"), None, None).await {
        Err(k) => out.push(
            Check::new(
                "embedder_health",
                Status::Fail,
                format!("embedder unreachable ({k})"),
            )
            .impact("semantic_search and reembed fail")
            .fix("Start the embedder service and check BSMCP_EMBEDDER_URL / compose network.")
            .cmd("docker compose logs --tail 50 bsmcp-embedder"),
        ),
        Ok(r) if r.status != 200 => out.push(Check::new(
            "embedder_health",
            Status::Fail,
            format!("/health -> {}", r.status),
        )),
        Ok(r) => {
            let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            let rr = v["reranker"]["model"].as_str().unwrap_or("none");
            out.push(Check::new(
                "embedder_health",
                Status::Pass,
                format!(
                    "model={} dims={} reranker={}",
                    v["model"].as_str().unwrap_or("?"),
                    v["dimensions"],
                    rr
                ),
            ));
            match fetch(&c, &format!("{base}/ready"), None, None).await {
                Ok(r) if r.status == 200 => {
                    out.push(Check::new("embedder_ready", Status::Pass, "/ready -> 200"))
                }
                Ok(r) if r.status == 404 => out.push(Check::new(
                    "embedder_ready",
                    Status::Skip,
                    "older embedder without /ready (404)",
                )),
                Ok(r) => out.push(
                    Check::new(
                        "embedder_ready",
                        Status::Warn,
                        format!(
                            "/ready -> {} (embedder not ready, e.g. model still loading)",
                            r.status
                        ),
                    )
                    .fix("Wait and retry; check the embedder logs.")
                    .cmd("docker compose logs --tail 50 bsmcp-embedder"),
                ),
                Err(k) => out.push(Check::new(
                    "embedder_ready",
                    Status::Warn,
                    format!("/ready unreachable ({k})"),
                )),
            }
        }
    }
    let Ok((_, Some(sdb), idb)) = dbs else {
        out.push(Check::new(
            "index_jobs",
            Status::Skip,
            "database unavailable",
        ));
        return out;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut jobs: Vec<(String, Option<i64>, Option<String>)> = vec![];
    let mut last: Option<i64> = None;
    if let Ok(js) = sdb.list_jobs(20).await {
        for j in js {
            last = last.max(j.finished_at);
            jobs.push((j.status, j.started_at, j.resolved_status));
        }
    }
    if let Ok(js) = idb.list_index_jobs(20).await {
        for j in js {
            last = last.max(j.finished_at);
            jobs.push((j.status, j.started_at, j.resolved_status));
        }
    }
    let (failed, stuck) = job_health(&jobs, now, 2 * 3600);
    let last_s = last.map_or("never".to_string(), |t| {
        format!("{}h ago", (now - t).max(0) / 3600)
    });
    out.push(if failed + stuck > 0 {
        Check::new("index_jobs", Status::Warn, format!("{failed} failed (unresolved), {stuck} running >2h; last finished: {last_s}"))
            .impact("index may be stale or incomplete")
            .fix("Inspect /status and the worker logs; a failed job is retried automatically, a stuck one is failed by the housekeeper.")
            .cmd("docker compose logs --tail 100 bsmcp-worker")
    } else {
        Check::new("index_jobs", Status::Pass, format!("no failed or stuck jobs; last finished: {last_s}"))
    });
    out
}

// ---------- token ----------

async fn check_tokens(env: &Env, cfg: &BackendConfig) -> Vec<Check> {
    let pick = |a: &str, b: &str| env_get(env, a).zip(env_get(env, b));
    let Some((id, secret)) = pick("BSMCP_INDEX_TOKEN_ID", "BSMCP_INDEX_TOKEN_SECRET")
        .or_else(|| pick("BSMCP_EMBED_TOKEN_ID", "BSMCP_EMBED_TOKEN_SECRET"))
    else {
        return vec![Check::new(
            "token",
            Status::Skip,
            "the worker's token is not visible to the server; run doctor with BSMCP_INDEX_TOKEN_ID/SECRET (or BSMCP_EMBED_TOKEN_ID/SECRET) set to validate it",
        )
        .cmd("docker compose exec -e BSMCP_INDEX_TOKEN_ID=... -e BSMCP_INDEX_TOKEN_SECRET=... bsmcp-server bsmcp-server doctor")];
    };
    let auth = format!("Token {id}:{secret}");
    let c = client(|b| b);
    let get = |path: String| {
        let url = format!("{}{path}", cfg.api_url);
        let (c, auth) = (c.clone(), auth.clone());
        async move { fetch(&c, &url, Some(&auth), None).await }
    };
    let mut out = vec![];
    let mut first_ids: HashMap<&str, Option<i64>> = HashMap::new();
    let view = "view access to this content type (role permission)";
    for (key, path) in [
        ("pages", "pages"),
        ("books", "books"),
        ("chapters", "chapters"),
        ("shelves", "shelves"),
        ("roles", "roles"),
    ] {
        let cap = if key == "roles" {
            "manage roles & role permissions"
        } else {
            view
        };
        let r = get(format!("/api/{path}?count=1")).await;
        let (outcome, empty, first) = summarize(&r);
        first_ids.insert(key, first);
        out.push(classify_probe(key, cap, outcome, empty));
    }
    let role_cap = "manage roles & role permissions";
    out.push(match first_ids["roles"] {
        Some(rid) => {
            let r = get(format!("/api/roles/{rid}")).await;
            classify_probe(
                "roles_detail",
                role_cap,
                r.as_ref().map(|x| x.status).map_err(|k| *k),
                false,
            )
        }
        None => Check::new(
            "token.roles_detail",
            Status::Skip,
            "no role id available from the previous probe",
        ),
    });
    out.push(match first_ids["books"] {
        Some(bid) => {
            let r = get(format!("/api/content-permissions/book/{bid}")).await;
            classify_probe(
                "content_permissions",
                "manage permissions on content",
                r.as_ref().map(|x| x.status).map_err(|k| *k),
                false,
            )
        }
        None => Check::new(
            "token.content_permissions",
            Status::Skip,
            "no visible book to probe",
        ),
    });
    out.push(
        Check::new("token.note", Status::Skip, "BookStack requires admin-level role permissions even for the read-only roles API: the token needs 'manage roles & role permissions' and 'manage permissions on content'. Nothing beyond what the worker uses is probed."),
    );
    out
}

/// (status-or-neterr, list is empty, first `data[0].id`)
fn summarize(r: &Result<Resp, &'static str>) -> (Result<u16, &'static str>, bool, Option<i64>) {
    match r {
        Err(k) => (Err(k), false, None),
        Ok(r) => {
            let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            let first = v["data"][0]["id"].as_i64();
            let empty = r.status == 200 && v["data"].as_array().is_some_and(|a| a.is_empty());
            (Ok(r.status), empty, first)
        }
    }
}

// ---------- entry ----------

pub async fn run(args: &[String]) -> i32 {
    let json_out = args.iter().any(|a| a == "--json");
    let env: Env = std::env::vars().collect();
    let mut checks = vec![check_config(&env)];
    let dbs = open_db(&env).await;
    checks.push(check_database(&env, &dbs).await);
    let cfg = BackendConfig::from_env().ok();
    match &cfg {
        Some(cfg) => {
            checks.extend(check_backend(&env, cfg).await);
            checks.push(check_internet(&env).await);
            checks.extend(check_self(&env).await);
            checks.extend(check_semantic(&env, &dbs).await);
            checks.extend(check_tokens(&env, cfg).await);
        }
        None => {
            checks.push(check_internet(&env).await);
            checks.extend(check_self(&env).await);
            checks.extend(check_semantic(&env, &dbs).await);
            checks.push(Check::new(
                "backend_api",
                Status::Skip,
                "backend URL missing, see config",
            ));
        }
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&to_json(&checks)).unwrap_or_default()
        );
    } else {
        print!("{}", to_table(&checks));
    }
    exit_code(&checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    const KEY: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn exit_code_only_fail() {
        let ok = vec![
            Check::new("a", Status::Warn, ""),
            Check::new("b", Status::Skip, ""),
        ];
        assert_eq!(exit_code(&ok), 0);
        let bad = vec![
            Check::new("a", Status::Pass, ""),
            Check::new("b", Status::Fail, ""),
        ];
        assert_eq!(exit_code(&bad), 1);
    }

    #[test]
    fn json_shape_is_stable() {
        let v = to_json(&[Check::new("x", Status::Warn, "c")
            .hypothesis()
            .fix("f")
            .cmd("k")]);
        assert_eq!(v["version"], 1);
        assert_eq!(v["summary"]["warn"], 1);
        assert_eq!(v["exit_code"], 0);
        let c = &v["checks"][0];
        for k in ["id", "status", "kind", "cause", "impact", "fix", "commands"] {
            assert!(c.get(k).is_some(), "{k}");
        }
        assert_eq!(c["status"], "WARN");
        assert_eq!(c["kind"], "hypothesis");
        assert!(v["not_checkable_from_container"].as_array().unwrap().len() >= 4);
    }

    #[test]
    fn table_mentions_host_limits() {
        let t = to_table(&[Check::new("x", Status::Pass, "ok")]);
        assert!(t.contains("NOT checkable") && t.contains("iptables") && t.contains("install.sh"));
    }

    #[test]
    fn config_checks_and_no_secret_leak() {
        let good = env(&[
            ("BSMCP_ENCRYPTION_KEY", KEY),
            ("BSMCP_BOOKSTACK_URL", "http://bs:80"),
            ("BSMCP_PUBLIC_DOMAIN", "mcp.example.com"),
        ]);
        assert_eq!(check_config(&good).status, Status::Pass);
        let short = env(&[
            ("BSMCP_ENCRYPTION_KEY", "short-secret"),
            ("BSMCP_BOOKSTACK_URL", "http://bs"),
        ]);
        let c = check_config(&short);
        assert_eq!(c.status, Status::Fail);
        assert!(!c.cause.contains("short-secret"));
        let mut scheme = good.clone();
        scheme.insert(
            "BSMCP_PUBLIC_DOMAIN".into(),
            "https://mcp.example.com".into(),
        );
        assert_eq!(check_config(&scheme).status, Status::Fail);
        let mut sem = good.clone();
        sem.insert("BSMCP_SEMANTIC_SEARCH".into(), "true".into());
        sem.insert("BSMCP_WEBHOOK_SECRET".into(), "tooshort".into());
        let c = check_config(&sem);
        assert!(c.cause.contains("WEBHOOK_SECRET shorter") && !c.cause.contains("tooshort"));
        let mut nourl = good;
        nourl.remove("BSMCP_BOOKSTACK_URL");
        assert_eq!(check_config(&nourl).status, Status::Fail);
        let mut libs = env(&[("BSMCP_ENCRYPTION_KEY", KEY), ("BSMCP_BACKEND", "libstack")]);
        assert!(check_config(&libs).cause.contains("BSMCP_LIBSTACK_URL"));
        libs.insert("BSMCP_LIBSTACK_URL".into(), "not a url".into());
        assert_eq!(check_config(&libs).status, Status::Fail);
    }

    #[test]
    fn probe_classification_table() {
        let cases: &[(Result<u16, &str>, bool, Status, &str)] = &[
            (Ok(200), false, Status::Pass, "OK"),
            (Ok(200), true, Status::Warn, "no visible content"),
            (Ok(401), false, Status::Fail, "invalid"),
            (Ok(403), false, Status::Fail, "lacks the capability 'cap'"),
            (Ok(404), false, Status::Warn, "not supported"),
            (Ok(405), false, Status::Warn, "not supported"),
            (Ok(500), false, Status::Warn, "unexpected"),
            (Err("dns"), false, Status::Fail, "unreachable (dns)"),
        ];
        for (o, e, st, text) in cases {
            let c = classify_probe("p", "cap", *o, *e);
            assert_eq!(c.status, *st, "{o:?}");
            assert!(c.cause.contains(text), "{} / {text}", c.cause);
        }
    }

    #[test]
    fn local_ips_and_job_health() {
        assert!(is_local_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_local_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_local_ip(&"fd00::1".parse().unwrap()));
        assert!(!is_local_ip(&"8.8.8.8".parse().unwrap()));
        let jobs = vec![
            ("failed".to_string(), None, None),
            ("failed".to_string(), None, Some("retried".to_string())),
            ("running".to_string(), Some(0), None),
            ("running".to_string(), Some(9_000), None),
        ];
        assert_eq!(job_health(&jobs, 10_000, 3_600), (1, 1));
    }

    /// Mock BookStack on 127.0.0.1: 401 for a bad token, 403 for roles, 200 else.
    #[tokio::test]
    async fn token_probes_against_mock() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let (code, body) = if !req.contains("Token good:tok") {
                        ("401 Unauthorized", "{}")
                    } else if req.starts_with("GET /api/roles") {
                        ("403 Forbidden", "{}")
                    } else if req.starts_with("GET /api/shelves") {
                        ("200 OK", r#"{"data":[]}"#)
                    } else {
                        ("200 OK", r#"{"data":[{"id":7}]}"#)
                    };
                    let resp = format!("HTTP/1.1 {code}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        let cfg = BackendConfig::new(
            BackendKind::BookStack,
            &format!("http://127.0.0.1:{port}"),
            None,
        );
        let good = env(&[
            ("BSMCP_INDEX_TOKEN_ID", "good"),
            ("BSMCP_INDEX_TOKEN_SECRET", "tok"),
        ]);
        let out = check_tokens(&good, &cfg).await;
        let st = |id: &str| out.iter().find(|c| c.id == id).unwrap().status;
        assert_eq!(st("token.pages"), Status::Pass);
        assert_eq!(st("token.shelves"), Status::Warn);
        assert_eq!(st("token.roles"), Status::Fail);
        assert_eq!(st("token.content_permissions"), Status::Pass);
        let bad = env(&[
            ("BSMCP_EMBED_TOKEN_ID", "bad"),
            ("BSMCP_EMBED_TOKEN_SECRET", "x"),
        ]);
        let out = check_tokens(&bad, &cfg).await;
        assert!(out
            .iter()
            .find(|c| c.id == "token.pages")
            .unwrap()
            .cause
            .contains("401"));
        assert_eq!(check_tokens(&env(&[]), &cfg).await[0].status, Status::Skip);
        let r = probe_url(&cfg.api_url).await;
        assert!(r.reachable && r.detail.contains("IPv4"));
    }
}
