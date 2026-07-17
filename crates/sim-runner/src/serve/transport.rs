//! Transport layer for the JSON-RPC server.
//!
//! Supports two transport modes:
//! - **TCP**: one thread per connection, reads newline-delimited JSON
//! - **stdio**: reads from stdin, writes to stdout
//!
//! All connections share one [`Server`] so stop / status from a sibling
//! client can observe sessions owned by another connection's run.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::TcpStream;
use std::sync::Arc;

use serde_json::Value;

use super::error_codes;
use super::{dispatch, rpc_error, Server, PROTOCOL_VERSION};

/// Handle a single TCP connection: read requests, dispatch, write responses.
pub fn handle_tcp(server: Arc<Server>, stream: TcpStream) {
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
                if let Err(e) = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                ) {
                    eprintln!("error: write error on connection: {}", e);
                    break;
                }
                if let Err(e) = writer.flush() {
                    eprintln!("error: flush error on connection: {}", e);
                    break;
                }
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
            if let Err(e) = writeln!(
                writer,
                "{}",
                serde_json::to_string(&err).unwrap_or_default()
            ) {
                eprintln!("error: write error on connection: {}", e);
                break;
            }
            if let Err(e) = writer.flush() {
                eprintln!("error: flush error on connection: {}", e);
                break;
            }
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
                if let Err(e) = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                ) {
                    eprintln!("error: write error on connection: {}", e);
                    break;
                }
                if let Err(e) = writer.flush() {
                    eprintln!("error: flush error on connection: {}", e);
                    break;
                }
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
                if let Err(e) = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                ) {
                    eprintln!("error: write error on stdout: {}", e);
                    break;
                }
                if let Err(e) = writer.flush() {
                    eprintln!("error: flush error on stdout: {}", e);
                    break;
                }
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
            if let Err(e) = writeln!(
                writer,
                "{}",
                serde_json::to_string(&err).unwrap_or_default()
            ) {
                eprintln!("error: write error on stdout: {}", e);
                break;
            }
            if let Err(e) = writer.flush() {
                eprintln!("error: flush error on stdout: {}", e);
                break;
            }
            continue;
        }

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
                if let Err(e) = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&err).unwrap_or_default()
                ) {
                    eprintln!("error: write error on stdout: {}", e);
                    break;
                }
                if let Err(e) = writer.flush() {
                    eprintln!("error: flush error on stdout: {}", e);
                    break;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::time::Duration;

    /// Ignored by default: requires interactive stdin.
    #[test]
    #[ignore]
    fn test_stdio_integration() {
        let server = Server::new(Duration::from_secs(300));
        let _ = &server;
        let mut cursor = Cursor::new(Vec::new());
        let _ = writeln!(
            cursor,
            r#"{{"jsonrpc":"2.0","id":1,"method":"server.version","params":{{}}}}"#
        );
    }
}
