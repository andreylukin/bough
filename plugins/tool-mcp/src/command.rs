//! Invariant: `/mcp call` validates its JSON against the TOOL'S OWN input schema before the call,
//! so a malformed argument is `CommandError::BadArgs` naming the usage and never a foreign server's
//! error message. The output cites the call's cite, like any other pull (§6).

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandScope, CommandSpec, CommandsHandle,
    Invocation, OutputRender,
};
use bough_plugin_mcp::{McpHandle, McpToolRef, ServerName};

/// The usage line a bad invocation quotes.
pub const USAGE: &str = "/mcp call <server> <tool> <json> | /mcp list [server]";

/// PURE: the JSON a `/mcp call` line asks for, or the reason it is not one.
///
/// Split out of the command body so the parse rules are testable without a registry: the
/// arguments are a mode word and then, for `call`, a server, a tool and ONE JSON blob — the rest
/// of the line rejoined, so an unquoted `{"a": 1}` still arrives whole.
pub fn parse_call(args: &[String]) -> Result<(McpToolRef, serde_json::Value), CommandError> {
    let bad = |detail: String| CommandError::BadArgs {
        usage: USAGE.to_string(),
        detail,
    };
    if args.len() < 3 {
        return Err(bad("`call` needs a server, a tool and a JSON object".into()));
    }
    let json = args[2..].join(" ");
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| bad(format!("the arguments are not JSON: {e}")))?;
    if !value.is_object() {
        return Err(bad("the arguments must be a JSON object".into()));
    }
    Ok((
        McpToolRef {
            server: ServerName::new(&args[0]),
            tool: args[1].clone(),
        },
        value,
    ))
}

/// PURE: the first schema complaint about `args`, if any.
pub fn schema_complaint(schema: &serde_json::Value, args: &serde_json::Value) -> Option<String> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        // A server's own schema failing to compile is not the typist's fault: let the call
        // through rather than making a foreign bug look like a bad argument.
        Err(_) => return None,
    };
    let first = validator.iter_errors(args).next().map(|e| {
        let at = e.instance_path.to_string();
        if at.is_empty() {
            e.to_string()
        } else {
            format!("argument `{at}`: {e}")
        }
    });
    first
}

struct McpCommand {
    mcp: McpHandle,
}

#[async_trait::async_trait]
impl Command for McpCommand {
    async fn run(
        &self,
        inv: Invocation,
        _cx: CommandCx,
    ) -> Result<bough_plugin_commands::CommandOutput, CommandError> {
        let bad = |detail: String| CommandError::BadArgs {
            usage: USAGE.to_string(),
            detail,
        };
        match inv.args.first().map(String::as_str) {
            Some("list") => {
                let server = inv.args.get(1).map(ServerName::new);
                let tools = self
                    .mcp
                    .tools(server.as_ref())
                    .await
                    .map_err(|e| CommandError::Failed(e.to_string()))?;
                let text = if tools.is_empty() {
                    "no MCP tools".to_string()
                } else {
                    tools
                        .iter()
                        .map(|t| format!("{}__{}: {}", t.server, t.tool, t.description))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(bough_plugin_commands::CommandOutput {
                    text,
                    render: OutputRender::KeyValue,
                    cites: Vec::new(),
                })
            }
            Some("call") => {
                let (r, args) = parse_call(&inv.args[1..])?;
                let known = self
                    .mcp
                    .tools(Some(&r.server))
                    .await
                    .map_err(|e| CommandError::Failed(e.to_string()))?;
                let Some(info) = known.iter().find(|t| t.tool == r.tool) else {
                    return Err(bad(format!(
                        "server `{}` has no tool `{}`",
                        r.server, r.tool
                    )));
                };
                // The TOOL'S OWN schema decides, before the call leaves the process.
                if let Some(detail) = schema_complaint(&info.input_schema, &args) {
                    return Err(bad(detail));
                }
                let out = self
                    .mcp
                    .call(&r, args)
                    .await
                    .map_err(|e| CommandError::Failed(e.to_string()))?;
                Ok(bough_plugin_commands::CommandOutput {
                    text: format!(
                        "server: {}\ntool: {}\nis_error: {}\n{}",
                        r.server, r.tool, out.is_error, out.content
                    ),
                    render: OutputRender::KeyValue,
                    cites: out.cites,
                })
            }
            _ => Err(bad("expected `call` or `list`".into())),
        }
    }
}

/// `/mcp call <server> <tool> <json>` and `/mcp list [server]`.
pub async fn register(
    ctx: &Context,
    commands: &CommandsHandle,
    mcp: &McpHandle,
) -> Result<EffectHandle, PluginError> {
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("mcp"),
                summary: "Call and list MCP tools.".into(),
                usage: USAGE.into(),
                // The line's own shape is checked here, not by the registry: `call`'s third
                // argument is a JSON blob that may contain spaces, which no positional schema
                // describes.
                args: schemars::Schema::try_from(serde_json::json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                }))
                .expect("an array schema is legal"),
                scope: CommandScope::Global,
                run: Arc::new(McpCommand { mcp: mcp.clone() }),
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn a_call_line_parses_into_a_ref_and_a_json_object() {
        let (r, args) = parse_call(&words("fixture echo {\"text\": \"hi\"}")).unwrap();
        assert_eq!(r.server, ServerName::new("fixture"));
        assert_eq!(r.tool, "echo");
        assert_eq!(args, serde_json::json!({ "text": "hi" }));
    }

    #[test]
    fn malformed_json_is_bad_args_naming_the_usage() {
        let err = parse_call(&words("fixture echo {oops")).unwrap_err();
        match err {
            CommandError::BadArgs { usage, detail } => {
                assert_eq!(usage, USAGE);
                assert!(detail.contains("not JSON"), "{detail}");
            }
            other => panic!("expected BadArgs, got {other:?}"),
        }
        assert!(matches!(
            parse_call(&words("fixture echo")).unwrap_err(),
            CommandError::BadArgs { .. }
        ));
        assert!(matches!(
            parse_call(&words("fixture echo [1,2]")).unwrap_err(),
            CommandError::BadArgs { .. }
        ));
    }

    #[test]
    fn the_tools_own_schema_is_what_rejects_an_argument() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        });
        assert_eq!(
            schema_complaint(&schema, &serde_json::json!({ "text": "hi" })),
            None
        );
        assert!(schema_complaint(&schema, &serde_json::json!({})).is_some());
        assert!(schema_complaint(&schema, &serde_json::json!({ "text": 1 })).is_some());
    }
}
