//! Parser for the Codex `config.toml` MCP section.
//!
//! Only `command` and `args` of each `[mcp_servers.<name>]` table are read;
//! nested sub-tables (`.env`, `.tools.<tool>`, ...) are ignored. Error strings
//! carry a position only, because the TOML library's own message quotes the
//! offending source line, which may hold a token.

use super::{McpScope, McpServerConfig, strip_bom};
use crate::model::Tool;

pub fn parse_codex_toml(text: &str) -> Result<Vec<McpServerConfig>, String> {
    let text = strip_bom(text);
    let root: toml::Table = toml::from_str(text).map_err(|e| toml_error(text, &e))?;
    let mut out = Vec::new();
    let Some(toml::Value::Table(servers)) = root.get("mcp_servers") else {
        return Ok(out);
    };
    for (name, entry) in servers {
        let Some(command) = entry
            .get("command")
            .and_then(toml::Value::as_str)
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        let args = match entry.get("args") {
            Some(toml::Value::Array(list)) => list.iter().filter_map(arg_to_string).collect(),
            _ => Vec::new(),
        };
        out.push(McpServerConfig {
            tool: Tool::Codex,
            name: name.clone(),
            command: command.to_string(),
            args,
            scope: McpScope::User,
        });
    }
    Ok(out)
}

fn arg_to_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn toml_error(text: &str, err: &toml::de::Error) -> String {
    match err.span() {
        Some(span) => {
            let before = &text.as_bytes()[..span.start.min(text.len())];
            let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
            let col = before.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
            format!("invalid TOML at line {line} column {col}")
        }
        None => "invalid TOML".to_string(),
    }
}
