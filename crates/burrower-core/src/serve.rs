// SPDX-License-Identifier: MPL-2.0
// Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk>
//! # BI-1: `burrower serve` — Unix domain socket endpoint
//!
//! Burrower today is a one-shot CLI; every `swarm`, `attempt`, or
//! `ledger` invocation reloads the corpus and discards state. ECHIDNA
//! and `echidnabot` have no shared swarm endpoint to consult during
//! their own runs, so their failure-recovery loops can't ask the
//! Burrower swarm "do you have prior work on this goal?".
//!
//! This module exposes the swarm as a long-lived service over a
//! Unix domain socket. Clients send line-delimited JSON requests and
//! receive line-delimited JSON responses:
//!
//! ```text
//! Request:  {"cmd": "swarm",  "args": {"goal": "...", "index": "...", "top": 5}}
//! Request:  {"cmd": "attempt","args": {"goal": "...", "echidna": "...", ...}}
//! Request:  {"cmd": "ledger", "args": {"path": "...", "limit": 10}}
//! Request:  {"cmd": "ping",   "args": {}}
//! Response: {"ok": true,  "result": <command-specific JSON>}
//! Response: {"ok": false, "error": "<message>"}
//! ```
//!
//! The protocol intentionally mirrors the existing CLI subcommands so
//! the dispatcher is a one-liner per command.
//!
//! Implementation choice: synchronous, thread-per-connection. burrower-
//! core has no async runtime today, and JSON-line requests are short-
//! lived; spawning a std thread per accepted connection is cheaper than
//! pulling in tokio. Concurrent corpus loads are independent — each
//! request brings its own paths.
//!
//! Threading model:
//!   accept loop ──> spawn handler thread per connection
//!   handler reads one JSON line, dispatches, writes one JSON line
//!   socket is removed (best-effort) on Drop of the listener guard
//!
//! See `proof-burrower/docs/ECHIDNA-INTEGRATION.adoc` §BI-1.

use crate::{
    attempt::{run_playbook, ProverConfig, TacticTemplate, Playbook},
    corpus::Corpus,
    goal::parse_goal,
    ledger::Ledger,
    specialist::Swarm,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

/// Cleanup guard — removes the socket file when the listener drops.
pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Request envelope — `cmd` selects the dispatch arm; `args` is the
/// command-specific payload (parsed inside each handler).
#[derive(Debug, Deserialize)]
pub struct Request {
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

/// Response envelope — `ok=true` carries `result`; `ok=false` carries
/// `error`. We keep the shape simple so any client can pattern-match.
#[derive(Debug, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn ok(result: Value) -> Self {
        Self { ok: true, result: Some(result), error: None }
    }
    fn err<E: std::fmt::Display>(e: E) -> Self {
        Self { ok: false, result: None, error: Some(e.to_string()) }
    }
}

/// Bind a Unix listener at `socket_path` and accept connections in a
/// loop. Each connection spawns a handler thread that reads one JSON
/// line, dispatches, and writes one JSON line response.
///
/// Returns when the listener errors fatally (the socket guard cleans
/// up on drop). Designed to be invoked from `burrower serve --socket
/// /tmp/burrower.sock`.
pub fn run(socket_path: PathBuf) -> Result<()> {
    // Best-effort cleanup of any stale socket file.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).with_context(|| {
        format!("failed to bind Unix socket at {}", socket_path.display())
    })?;
    let _guard = SocketGuard { path: socket_path.clone() };

    eprintln!(
        "burrower serve: listening on {} (line-delimited JSON; \
         send {{\"cmd\":\"ping\"}} to verify)",
        socket_path.display()
    );

    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_connection(stream) {
                        eprintln!("burrower serve: handler error: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("burrower serve: accept failed: {e}");
                // Don't bail; keep the listener alive on transient errors.
                continue;
            }
        }
    }
    Ok(())
}

fn handle_connection(stream: UnixStream) -> Result<()> {
    // Each request/response is a single JSON line so simple clients
    // (echo / nc / curl-unix-socket) can drive the protocol. Multi-
    // request streams are out of scope for v1.
    let reader_stream = stream.try_clone().context("clone stream")?;
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).context("read request line")?;
    if n == 0 {
        return Ok(());
    }

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => dispatch(req),
        Err(e) => Response::err(format!("malformed request: {e}")),
    };

    let mut writer = stream;
    let body = serde_json::to_string(&response).context("serialise response")?;
    writeln!(writer, "{body}").context("write response")?;
    writer.flush().ok();
    Ok(())
}

fn dispatch(req: Request) -> Response {
    match req.cmd.as_str() {
        "ping" => Response::ok(json!({"pong": true})),
        "swarm" => match handle_swarm(req.args) {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(e),
        },
        "attempt" => match handle_attempt(req.args) {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(e),
        },
        "ledger" => match handle_ledger_recent(req.args) {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(e),
        },
        other => Response::err(format!(
            "unknown cmd: {other} (expected: swarm | attempt | ledger | ping)"
        )),
    }
}

#[derive(Debug, Deserialize)]
struct SwarmArgs {
    goal: String,
    index: PathBuf,
    #[serde(default = "default_top")]
    top: usize,
    #[serde(default)]
    ledger: Option<PathBuf>,
}
fn default_top() -> usize { 5 }

fn handle_swarm(args: Value) -> Result<Value> {
    let a: SwarmArgs = serde_json::from_value(args).map_err(|e| anyhow!("swarm args: {e}"))?;
    let corpus = Corpus::load(&a.index).with_context(|| {
        format!("load index from {}", a.index.display())
    })?;
    let parsed = parse_goal(&a.goal);
    let swarm = Swarm::new();
    let ledger_handle = a.ledger.as_ref().map(|p| Ledger::open(p)).transpose()?;
    let readings = swarm.route_with_ledger(&parsed, &corpus, a.top, ledger_handle.as_ref());
    let synthesis = swarm.synthesise(&readings);
    Ok(json!({
        "synthesis": synthesis,
        "readings":  readings,
    }))
}

#[derive(Debug, Deserialize)]
struct AttemptArgs {
    goal: String,
    echidna: PathBuf,
    ledger: PathBuf,
    #[serde(default = "default_timeout")]
    timeout_secs: u32,
    #[serde(default)]
    project_root: Option<PathBuf>,
    #[serde(default)]
    sandbox: Option<String>,
}
fn default_timeout() -> u32 { 60 }

fn handle_attempt(args: Value) -> Result<Value> {
    let a: AttemptArgs = serde_json::from_value(args).map_err(|e| anyhow!("attempt args: {e}"))?;
    let parsed = parse_goal(&a.goal);
    let swarm = Swarm::new();
    let l = Ledger::open(&a.ledger)?;
    let prover = ProverConfig {
        echidna_path: a.echidna,
        timeout_secs: a.timeout_secs,
        workdir: None,
        project_root: a.project_root,
        sandbox: a.sandbox.unwrap_or_else(|| "none".to_string()),
    };
    let attempts = swarm.attempt_all(&parsed, &prover, Some(&l));
    Ok(serde_json::to_value(&attempts)?)
}

#[derive(Debug, Deserialize)]
struct LedgerArgs {
    path: PathBuf,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize { 10 }

fn handle_ledger_recent(args: Value) -> Result<Value> {
    let a: LedgerArgs = serde_json::from_value(args).map_err(|e| anyhow!("ledger args: {e}"))?;
    let l = Ledger::open(&a.path)?;
    let recs = l.recent(a.limit)?;
    Ok(serde_json::to_value(&recs)?)
}

// Suppress "unused" warnings when the swarm-attempt path isn't compiled
// in (e.g. minimal embeddings). Marker — these imports are real once
// the dispatch tree is wired below.
#[allow(dead_code)]
fn _force_use_of_imports() {
    let _ = TacticTemplate { name: String::new(), script: String::new(), description: String::new() };
    let _ = Playbook { specialist: String::new(), tactics: vec![] };
    let _: Option<Arc<()>> = None;
    let _ = run_playbook;
}
