//! Parsers for the Claude Code side of the MCP configuration.
//!
//! Only `command` and `args` of a server are ever read. `env` (which usually
//! holds tokens) is never looked at, so it cannot leak into the model, a
//! dump file or an error message. Error strings carry a position and a
//! category only, never a slice of the file.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::{McpScope, McpServerConfig, json_error, strip_bom};
use crate::model::Tool;

/// The parts of `~/.claude.json` that matter here. The file can be several MB
/// (conversation history per project); every other key is skipped by serde
/// without being materialised.
#[derive(Deserialize)]
struct ClaudeJson {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: Option<Value>,
    #[serde(default)]
    projects: Option<BTreeMap<String, Option<ProjectEntry>>>,
}

#[derive(Deserialize)]
struct ProjectEntry {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: Option<Value>,
}

#[derive(Deserialize)]
struct InstalledPlugins {
    #[serde(default)]
    plugins: Option<BTreeMap<String, Value>>,
}

/// User-level servers (`mcpServers`) followed by the servers of every
/// `projects[<path>]`, which get [`McpScope::Project`]. Entries without a
/// `command` (http/sse transports) are skipped.
pub fn parse_claude_json(text: &str) -> Result<Vec<McpServerConfig>, String> {
    let parsed: ClaudeJson = parse_object(text)?;
    let mut out = Vec::new();
    if let Some(value) = &parsed.mcp_servers {
        collect_servers(value, &McpScope::User, &mut out);
    }
    for (path, entry) in parsed.projects.iter().flatten() {
        if let Some(value) = entry.as_ref().and_then(|e| e.mcp_servers.as_ref()) {
            collect_servers(value, &McpScope::Project(path.clone()), &mut out);
        }
    }
    Ok(out)
}

/// A `.mcp.json`, either wrapped (`{"mcpServers": {...}}`) or a bare
/// `{"name": {...}}` map.
pub fn parse_mcp_json(text: &str, scope: McpScope) -> Result<Vec<McpServerConfig>, String> {
    let root = parse_root_object(text)?;
    let mut out = Vec::new();
    collect_wrapped_or_bare(&root, &scope, &mut out);
    Ok(out)
}

/// The inline `mcpServers` of a plugin's `.claude-plugin/plugin.json`. The
/// string form (a path such as `"./.mcp.json"`) is ignored: that file is read
/// on its own.
pub fn parse_plugin_json(text: &str, scope: McpScope) -> Result<Vec<McpServerConfig>, String> {
    let root = parse_root_object(text)?;
    let mut out = Vec::new();
    if let Some(inline @ Value::Object(_)) = root.get("mcpServers") {
        collect_wrapped_or_bare(inline, &scope, &mut out);
    }
    Ok(out)
}

/// `(plugin name without "@marketplace", installPath)` for every install
/// record in `installed_plugins.json`. A plugin maps to an array of install
/// records (one per scope); a single object is accepted too.
pub fn parse_installed_plugins(text: &str) -> Result<Vec<(String, String)>, String> {
    let parsed: InstalledPlugins = parse_object(text)?;
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, records) in parsed.plugins.iter().flatten() {
        let name = match key.rsplit_once('@') {
            Some((name, _)) if !name.is_empty() => name,
            _ => key.as_str(),
        };
        let records = match records {
            Value::Array(list) => list.iter().collect::<Vec<_>>(),
            single @ Value::Object(_) => vec![single],
            _ => Vec::new(),
        };
        for record in records {
            let Some(path) = record.get("installPath").and_then(Value::as_str) else {
                continue;
            };
            let pair = (name.to_string(), path.to_string());
            if !path.is_empty() && !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    Ok(out)
}

const NOT_AN_OBJECT: &str = "invalid JSON (expected an object at the top level)";

/// Deserialises a struct from a JSON object. The explicit check is needed
/// because serde would also fill a struct from a JSON array, positionally.
fn parse_object<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let text = strip_bom(text);
    if !text.trim_start().starts_with('{') {
        return Err(NOT_AN_OBJECT.to_string());
    }
    serde_json::from_str(text).map_err(|e| json_error(&e))
}

fn parse_root_object(text: &str) -> Result<Value, String> {
    let root: Value = serde_json::from_str(strip_bom(text)).map_err(|e| json_error(&e))?;
    if root.is_object() {
        Ok(root)
    } else {
        Err(NOT_AN_OBJECT.to_string())
    }
}

fn collect_wrapped_or_bare(root: &Value, scope: &McpScope, out: &mut Vec<McpServerConfig>) {
    match root.get("mcpServers") {
        Some(wrapped @ Value::Object(_)) => collect_servers(wrapped, scope, out),
        _ => collect_servers(root, scope, out),
    }
}

/// `servers` is a `{name: {command, args, ...}}` object.
fn collect_servers(servers: &Value, scope: &McpScope, out: &mut Vec<McpServerConfig>) {
    let Value::Object(map) = servers else {
        return;
    };
    for (name, entry) in map {
        let Some(command) = entry
            .get("command")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        let args = match entry.get("args") {
            Some(Value::Array(list)) => list.iter().filter_map(arg_to_string).collect(),
            _ => Vec::new(),
        };
        out.push(McpServerConfig {
            tool: Tool::ClaudeCode,
            name: name.clone(),
            command: command.to_string(),
            args,
            scope: scope.clone(),
        });
    }
}

fn arg_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
