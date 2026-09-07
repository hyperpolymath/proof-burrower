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
    attempt::ProverConfig, corpus::Corpus, goal::parse_goal, ledger::Ledger, specialist::Swarm,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;

/// Cleanup guard — removes the socket file when the listener drops.
pub struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        // A caller may have removed or replaced the path while we ran.
        // Never unlink a regular file, symlink or another listener's socket.
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
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
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    fn err<E: std::fmt::Display>(e: E) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(e.to_string()),
        }
    }
}

/// Bind a new socket without replacing an occupied path.
fn bind(socket_path: PathBuf) -> Result<(UnixListener, SocketGuard)> {
    // Refuse occupied paths, including live or stale sockets. An operator
    // must explicitly remove a stale socket; startup never removes user data.
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind Unix socket at {}", socket_path.display()))?;
    let metadata = std::fs::symlink_metadata(&socket_path)?;
    Ok((
        listener,
        SocketGuard {
            path: socket_path,
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    ))
}

/// Serve one JSON-line request per connection on a Unix socket.
///
/// Binding fails if the path already exists. Operators must explicitly
/// remove stale sockets before restarting. Each accepted connection runs
/// in its own thread; transient accept errors are logged and retried.
pub fn run(socket_path: PathBuf) -> Result<()> {
    let (listener, _guard) = bind(socket_path.clone())?;

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
fn default_top() -> usize {
    5
}

fn handle_swarm(args: Value) -> Result<Value> {
    let a: SwarmArgs = serde_json::from_value(args).map_err(|e| anyhow!("swarm args: {e}"))?;
    let corpus =
        Corpus::load(&a.index).with_context(|| format!("load index from {}", a.index.display()))?;
    let parsed = parse_goal(&a.goal);
    let swarm = Swarm::new();
    let ledger_handle = a.ledger.as_ref().map(Ledger::open).transpose()?;
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
fn default_timeout() -> u32 {
    60
}

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
fn default_limit() -> usize {
    10
}

fn handle_ledger_recent(args: Value) -> Result<Value> {
    let a: LedgerArgs = serde_json::from_value(args).map_err(|e| anyhow!("ledger args: {e}"))?;
    let l = Ledger::open(&a.path)?;
    let recs = l.recent(a.limit)?;
    Ok(serde_json::to_value(&recs)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::time::Duration;

    #[test]
    fn listener_never_unlinks_occupied_or_replaced_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.sock");
        std::fs::write(&path, "existing user data").unwrap();
        assert!(run(path.clone()).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "existing user data"
        );
        std::fs::remove_file(&path).unwrap();
        let (listener, guard) = bind(path.clone()).unwrap();
        assert!(bind(path.clone()).is_err());
        let client = UnixStream::connect(&path).unwrap();
        let (stream, _) = listener.accept().unwrap();
        drop((client, stream, listener, guard));
        assert!(!path.exists());
        let (listener, guard) = bind(path.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "replacement data").unwrap();
        drop((listener, guard));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement data");
    }

    fn exchange(request: &str) -> Value {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handler = thread::spawn(move || handle_connection(server));
        writeln!(client, "{request}").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        handler.join().unwrap().unwrap();
        serde_json::from_str(&response).unwrap()
    }

    #[test]
    fn socket_envelopes_reject_malformed_unknown_and_incomplete_requests() {
        assert_eq!(
            exchange(r#"{"cmd":"ping"}"#),
            json!({"ok":true,"result":{"pong":true}})
        );
        for request in [
            "not json",
            r#"{"cmd":"unknown"}"#,
            r#"{"cmd":"swarm"}"#,
            r#"{"cmd":"attempt"}"#,
            r#"{"cmd":"ledger"}"#,
        ] {
            let response = exchange(request);
            assert_eq!(response["ok"], false, "{response}");
            assert!(response["error"].as_str().unwrap().len() > 5);
            assert!(response.get("result").is_none());
        }
        let (client, server) = UnixStream::pair().unwrap();
        drop(client);
        handle_connection(server).unwrap();
    }

    #[test]
    fn socket_swarm_and_ledger_use_requested_storage_and_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("corpus.json");
        Corpus::default().save(&index).unwrap();
        let ledger = dir.path().join("readings.jsonl");
        let goal = "lemma ordered: \"finite walks ∧ tropical_add x y ≤ x\"";
        let response = exchange(
            &json!({"cmd":"swarm","args":{"goal":goal,"index":index,"ledger":ledger}}).to_string(),
        );
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["result"]["readings"].as_array().unwrap().len(), 3);
        assert!(response["result"]["synthesis"]["summary"].is_string());
        let records = Ledger::open(&ledger).unwrap().read_all().unwrap();
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|r| r.goal_excerpt == goal));
        let recent =
            exchange(&json!({"cmd":"ledger","args":{"path":ledger,"limit":1}}).to_string());
        assert_eq!(recent["ok"], true);
        assert_eq!(recent["result"].as_array().unwrap().len(), 1);
        assert_eq!(recent["result"][0]["id"], records[2].id);
        let defaults = exchange(&json!({"cmd":"ledger","args":{"path":ledger}}).to_string());
        assert_eq!(defaults["result"].as_array().unwrap().len(), 3);
        let missing = exchange(
            &json!({"cmd":"swarm","args":{"goal":goal,"index":dir.path().join("missing")}})
                .to_string(),
        );
        assert_eq!(missing["ok"], false);
    }

    #[test]
    fn socket_attempt_keeps_missing_prover_as_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("attempts.jsonl");
        let response = exchange(&json!({"cmd":"attempt","args":{"goal":"tropical_add x y ≤ x","echidna":dir.path().join("missing-prover"),"ledger":ledger}}).to_string());
        assert_eq!(response["ok"], true, "{response}");
        let attempts = response["result"].as_array().unwrap();
        assert!(!attempts.is_empty());
        assert!(attempts
            .iter()
            .all(|a| a["result"].get("Skipped").is_some()));
        assert_eq!(
            Ledger::open(&ledger).unwrap().read_all().unwrap().len(),
            attempts.len()
        );
    }
}
