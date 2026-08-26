//! Invariant: framing is LINE-DELIMITED JSON-RPC 2.0 on the child's stdin/stdout — the MCP stdio
//! transport — and NOTHING here knows what a tool is. Parsing is pure and total: an unreadable line
//! is a [`Incoming::Junk`], never a panic and never a dropped connection.

/// One message read off a child's stdout.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A response to a request this side sent.
    Response {
        id: u64,
        result: Result<serde_json::Value, String>,
    },
    /// A server-initiated notification: a method, no id.
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// Anything else. Reported, never fatal.
    Junk(String),
}

/// PURE: one line of the child's stdout → an [`Incoming`].
pub fn parse_line(line: &str) -> Incoming {
    let line = line.trim();
    if line.is_empty() {
        return Incoming::Junk("empty line".into());
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return Incoming::Junk(format!("not JSON: {}", truncate(line)));
    };
    let id = v.get("id").and_then(|i| i.as_u64());
    match (id, v.get("method").and_then(|m| m.as_str())) {
        (Some(id), _) if v.get("result").is_some() || v.get("error").is_some() => {
            let result = match v.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unspecified server error")
                    .to_string()),
                None => Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null)),
            };
            Incoming::Response { id, result }
        }
        (None, Some(method)) => Incoming::Notification {
            method: method.to_string(),
            params: v.get("params").cloned().unwrap_or(serde_json::Value::Null),
        },
        // A request FROM the server (sampling, roots). Not answered here, and not junk either: it
        // is reported so a silent no-answer is visible.
        (Some(_), Some(method)) => Incoming::Junk(format!("unanswered server request `{method}`")),
        _ => Incoming::Junk(format!(
            "neither response nor notification: {}",
            truncate(line)
        )),
    }
}

/// PURE: the request line this side writes.
pub fn request(id: u64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
        .to_string()
}

/// PURE: a notification line this side writes.
pub fn notification(method: &str, params: serde_json::Value) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

fn truncate(s: &str) -> String {
    if s.len() <= 120 {
        s.to_string()
    } else {
        format!("{}…", &s[..120])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_a_response() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#),
            Incoming::Response {
                id: 1,
                result: Ok(serde_json::json!({"tools": []}))
            }
        );
    }

    #[test]
    fn an_error_is_a_response_carrying_its_message() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-1,"message":"nope"}}"#),
            Incoming::Response {
                id: 2,
                result: Err("nope".into())
            }
        );
    }

    #[test]
    fn a_method_with_no_id_is_a_notification() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","method":"bough/actions","params":{"actions":[]}}"#),
            Incoming::Notification {
                method: "bough/actions".into(),
                params: serde_json::json!({"actions": []})
            }
        );
    }

    #[test]
    fn junk_is_reported_and_never_fatal() {
        assert!(matches!(parse_line("garbage"), Incoming::Junk(_)));
        assert!(matches!(parse_line(""), Incoming::Junk(_)));
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","id":3,"method":"sampling/createMessage"}"#),
            Incoming::Junk(_)
        ));
    }

    #[test]
    fn the_lines_this_side_writes_round_trip() {
        let r = request(7, "tools/list", serde_json::json!({}));
        let v: serde_json::Value = serde_json::from_str(&r).expect("valid JSON");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "tools/list");
        let n = notification("notifications/initialized", serde_json::Value::Null);
        assert!(serde_json::from_str::<serde_json::Value>(&n).expect("valid")["id"].is_null());
    }
}
