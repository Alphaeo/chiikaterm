//! `chiika mcp`: the MCP server Claude Code starts for an agent (stdio, one JSON-RPC message per
//! line). It lists the chiika tools and runs each call through the channel of the chiika that
//! started the agent (`CHIIKA_PORT`, `CHIIKA_TOKEN`, `CHIIKA_SESSION`).

use std::io::{self, BufRead, Write};
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};

use crate::channel;
use crate::coord;

/// A tool call must answer before Claude gives up on it.
const CALL_WAIT: Duration = Duration::from_secs(20);

/// Protocol version answered when the client does not ask for one.
const DEFAULT_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "chiika runs you inside a team of agents and shells. Use `team` to \
see who is there and what they do, `send_to` to hand work or questions to another agent, \
`read_screen` to follow a member's terminal (for example a dev server), and `post` / `board` \
to keep decisions and task status on the team's shared board.";

pub fn run() -> anyhow::Result<()> {
    let var = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));
    let port: u16 = var("CHIIKA_PORT")?.parse()?;
    let token = var("CHIIKA_TOKEN")?;
    let session = var("CHIIKA_SESSION")?;
    let call = |name: &str, args: &Value| -> anyhow::Result<Value> {
        let message = channel::tool_message(name, args);
        let answer = channel::request(port, &token, &session, &message, CALL_WAIT)?;
        Ok(serde_json::from_slice(&answer)?)
    };
    serve(io::stdin().lock(), io::stdout().lock(), call)?;
    Ok(())
}

/// Answers every request read from `input` on `output` until the input ends.
pub fn serve(
    input: impl BufRead,
    mut output: impl Write,
    mut call: impl FnMut(&str, &Value) -> anyhow::Result<Value>,
) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(response) = handle(&message, &mut call) {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The response to one message; `None` for notifications (no `id`).
pub fn handle(
    message: &Value,
    call: &mut impl FnMut(&str, &Value) -> anyhow::Result<Value>,
) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or_default();
    tracing::debug!(method, "mcp request");
    let params = &message["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(DEFAULT_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "chiika", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": coord::tool_list() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = match &params["arguments"] {
                Value::Null => json!({}),
                args => args.clone(),
            };
            call(name, &args).unwrap_or_else(|e| {
                json!({
                    "content": [{ "type": "text", "text": format!("chiika is not reachable: {e:#}") }],
                    "isError": true,
                })
            })
        }
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method {method}") },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_call(_: &str, _: &Value) -> anyhow::Result<Value> {
        anyhow::bail!("no chiika in this test")
    }

    #[test]
    fn initialize_answers_the_asked_version() {
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {} }
        });
        let response = handle(&request, &mut no_call).unwrap();
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(response["result"]["serverInfo"]["name"], "chiika");
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn notifications_get_no_answer_and_unknown_methods_an_error() {
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(handle(&initialized, &mut no_call), None);
        let unknown = json!({ "jsonrpc": "2.0", "id": "a", "method": "resources/list" });
        let response = handle(&unknown, &mut no_call).unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn calls_go_to_chiika_and_failures_are_tool_errors() {
        let request = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "post", "arguments": { "text": "hi" } }
        });
        let mut seen = Vec::new();
        let mut call = |name: &str, args: &Value| {
            seen.push((name.to_string(), args.clone()));
            Ok(json!({ "content": [{ "type": "text", "text": "posted" }], "isError": false }))
        };
        let response = handle(&request, &mut call).unwrap();
        assert_eq!(response["result"]["content"][0]["text"], "posted");
        assert_eq!(seen, [("post".to_string(), json!({ "text": "hi" }))]);
        let failed = handle(&request, &mut no_call).unwrap();
        assert_eq!(failed["result"]["isError"], true);
    }

    #[test]
    fn serve_reads_lines_and_lists_the_tools() {
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\nnot json\n\n";
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, no_call).unwrap();
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 5);
    }
}
