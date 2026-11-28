//! Shared `tracing-subscriber` setup for every bsmcp binary.
//!
//! Issue #90 Phase 1: replace 311 `eprintln!` / `println!` sites with
//! structured `tracing` records. Each binary's `main` calls `init(name)`
//! once before any log macro fires. Subsequent `tracing::info!` /
//! `warn!` / `error!` / `debug!` calls route through the subscriber set
//! up here.
//!
//! Environment:
//! - `RUST_LOG`: parsed by `EnvFilter`. Default `info`. Example:
//!   `RUST_LOG=bsmcp_server=debug,info`.
//! - `BSMCP_LOG_FORMAT`: `compact` (default) or `json`. JSON mode emits
//!   one structured record per line — the shape `/metrics` (Phase 2) and
//!   a future log shipper will rely on.
//!
//! Phase 2 (`/metrics` endpoint) and Phase 3 (`#[tracing::instrument]`
//! spans on handlers + OTel export) land in separate PRs; this module
//! is the foundation both build on.

use std::env;

use tracing_subscriber::{fmt, fmt::writer::BoxMakeWriter, prelude::*, EnvFilter};

/// Initialize the global tracing subscriber for a binary.
///
/// `binary_name` is included as a static field on every record so a
/// shared log pipeline can distinguish server / embedder / worker
/// without parsing the formatted message. Safe to call once at startup;
/// subsequent calls are a no-op (the `try_init` path swallows the
/// "already set" error rather than panicking).
pub fn init(binary_name: &'static str) {
    init_with(binary_name, false);
}

/// Like [`init`] but writes records to stderr. Required by STDIO MCP mode,
/// where stdout carries protocol messages only.
pub fn init_stderr(binary_name: &'static str) {
    init_with(binary_name, true);
}

fn init_with(binary_name: &'static str, stderr: bool) {
    let writer = if stderr {
        BoxMakeWriter::new(std::io::stderr)
    } else {
        BoxMakeWriter::new(std::io::stdout)
    };
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json_mode = env::var("BSMCP_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    // Both fmt layers carry the binary identity as a const field so log
    // aggregators can route on `binary=bsmcp-server` etc. without
    // string-matching the message.
    if json_mode {
        let layer = fmt::layer()
            .with_writer(writer)
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_target(true);
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(layer)
            .try_init();
    } else {
        let layer = fmt::layer()
            .with_writer(writer)
            .compact()
            .with_target(false);
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(layer)
            .try_init();
    }

    tracing::info!(binary = binary_name, "startup");
}
