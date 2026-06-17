//! Transport layer for the JSON-RPC server.
//!
//! Supports two transport modes:
//! - **TCP**: one thread per connection, reads newline-delimited JSON
//! - **stdio**: reads from stdin, writes to stdout

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::TcpStream;

use serde_json::Value;

use super::error_codes;
use super::{dispatch, rpc_error, Server, PROTOCOL_VERSION};

/// Handle a single TCP connection: read requests, dispatch, write responses.
pub fn handle_tcp(server: Server, stream: TcpStream) {
    let reader = BufReader::new(stream.try_clone().expect("failed to clone TCP stream"));
    let mut writer = BufWriter::new(stream);

    for line in reader.lines() {
        if server.is_shutdown() {
            break;
        }

        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error: read error on connection: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Send parse error.
                let err = rpc_error(
                    &serde_json::Value::Null,
                    error_codes::PARSE_ERROR,
                    &format!("parse error: {}", e),
                    None,
                );
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                );
                let _ = writer.flush();
                continue;
            }
        };

        // Validate JSON-RPC version.
        if request.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
            let err = rpc_error(
                &request.get("id").cloned().unwrap_or(Value::Null),
                error_codes::INVALID_REQUEST,
                "invalid JSON-RPC version",
                None,
            );
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&err).unwrap_or_default()
            );
            let _ = writer.flush();
            continue;
        }

        // Validate protocol version.
        if let Some(pv) = request.get("protocol_version").and_then(|v| v.as_u64()) {
            if pv > PROTOCOL_VERSION {
                let err = rpc_error(
                    &request.get("id").cloned().unwrap_or(Value::Null),
                    error_codes::UNSUPPORTED_PROTOCOL_VERSION,
                    &format!(
                        "unsupported protocol version {} (server supports up to {})",
                        pv, PROTOCOL_VERSION
                    ),
                    None,
                );
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                );
                let _ = writer.flush();
                continue;
            }
        }

        if let Some(response) = dispatch(&server, &request, &mut writer) {
            if let Err(e) = writeln!(
                writer,
                "{}",
                serde_json::to_string(&response).unwrap_or_default()
            ) {
                eprintln!("error: write error on connection: {}", e);
                break;
            }
            if let Err(e) = writer.flush() {
                eprintln!("error: flush error on connection: {}", e);
                break;
            }
        }
    }
}

/// Handle stdio transport: read from stdin, write to stdout.
pub fn handle_stdio(server: &Server) {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    let reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    for line in reader.lines() {
        if server.is_shutdown() {
            break;
        }

        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error: read error on stdin: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = rpc_error(
                    &Value::Null,
                    error_codes::PARSE_ERROR,
                    &format!("parse error: {}", e),
                    None,
                );
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                );
                let _ = writer.flush();
                continue;
            }
        };

        if request.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
            let err = rpc_error(
                &request.get("id").cloned().unwrap_or(Value::Null),
                error_codes::INVALID_REQUEST,
                "invalid JSON-RPC version",
                None,
            );
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&err).unwrap_or_default()
            );
            let _ = writer.flush();
            continue;
        }

        // Validate protocol version.
        if let Some(pv) = request.get("protocol_version").and_then(|v| v.as_u64()) {
            if pv > PROTOCOL_VERSION {
                let err = rpc_error(
                    &request.get("id").cloned().unwrap_or(Value::Null),
                    error_codes::UNSUPPORTED_PROTOCOL_VERSION,
                    &format!(
                        "unsupported protocol version {} (server supports up to {})",
                        pv, PROTOCOL_VERSION
                    ),
                    None,
                );
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                );
                let _ = writer.flush();
                continue;
            }
        }

        if let Some(response) = dispatch(server, &request, &mut writer) {
            if let Err(e) = writeln!(
                writer,
                "{}",
                serde_json::to_string(&response).unwrap_or_default()
            ) {
                eprintln!("error: write error on stdout: {}", e);
                break;
            }
            if let Err(e) = writer.flush() {
                eprintln!("error: flush error on stdout: {}", e);
                break;
            }
        }
    }
}

// ── Stdio integration test ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    /// Integration test: spawn `costar serve --stdio` and pipe JSON-RPC requests.
    ///
    /// This test requires the `sim-runner` binary to be built first (`cargo build`).
    #[test]
    fn test_stdio_integration() {
        // Find the binary.
        let exe = std::env::current_dir().ok().and_then(|d| {
            let p = d.join("target/debug/sim-runner");
            if p.exists() {
                Some(p)
            } else {
                d.join("../../target/debug/sim-runner")
                    .exists()
                    .then(|| d.join("../../target/debug/sim-runner"))
            }
        });

        let exe = match exe {
            Some(p) if p.exists() => p,
            _ => {
                eprintln!(
                    "skipping stdio integration test: sim-runner binary not found (run `cargo build` first)"
                );
                return;
            }
        };

        let mut child = Command::new(&exe)
            .arg("serve")
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn costar serve --stdio");

        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut buf_reader = std::io::BufReader::new(stdout);

        // Send session.create.
        stdin
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session.create\",\"params\":{}}\n",
            )
            .unwrap();
        stdin.flush().unwrap();

        let mut response = String::new();
        buf_reader.read_line(&mut response).unwrap();
        let resp: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(resp["id"], serde_json::json!(1));
        let session_id = resp["result"]["session_id"].as_u64().unwrap();
        assert!(session_id > 0);

        // Send session.destroy (stdin is still available).
        let req_line = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.destroy\",\"params\":{{\"session_id\":{}}}}}\n",
            session_id
        );
        stdin.write_all(req_line.as_bytes()).unwrap();
        stdin.flush().unwrap();

        response.clear();
        buf_reader.read_line(&mut response).unwrap();
        let resp: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(resp["result"]["destroyed"], serde_json::json!(true));

        // Send server.shutdown.
        stdin
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"server.shutdown\",\"params\":{}}\n",
            )
            .unwrap();
        stdin.flush().unwrap();
        drop(stdin);

        response.clear();
        buf_reader.read_line(&mut response).unwrap();

        child.wait().unwrap();
    }
}
