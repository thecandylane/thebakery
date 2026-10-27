//! Operator-owned extra tools, loaded from a directory **outside** the
//! worktree. Executable rows join the same advertised HTTP tool table as
//! `grep`. Rows with `runner: none` load but are not advertised.
//!
//! Unknown ids fail closed. A registered row without a supported runner
//! also fails closed. Broken JSON in the registry is an error, not an
//! empty table.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorTool {
    pub id: String,
    pub summary: String,
    #[serde(default = "default_gate")]
    pub gate_class: String,
    #[serde(default)]
    pub params: Vec<OperatorParam>,
    #[serde(default)]
    pub runner: OperatorRunner,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorParam {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperatorRunner {
    #[default]
    None,
}

fn default_gate() -> String {
    "read".into()
}

pub fn operator_tools_dir() -> PathBuf {
    if let Ok(p) = std::env::var("BAKERY_TOOLS_DIR_FOR_TEST") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".bakery/tools")
}

/// Load `*.json` files from `dir`. Missing dir → empty. Invalid JSON or
/// empty/duplicate ids → error (fail closed, do not skip silently).
pub fn load_operator_tools(dir: &Path) -> Result<Vec<OperatorTool>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut tools = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("operator tools dir {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    names.sort();
    for path in names {
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let tool: OperatorTool = serde_json::from_str(&raw)
            .map_err(|e| format!("operator tool {}: {e}", path.display()))?;
        let id = tool.id.trim().to_string();
        if id.is_empty() {
            return Err(format!("operator tool {} has an empty id", path.display()));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("duplicate operator tool id '{id}'"));
        }
        let mut tool = tool;
        tool.id = id;
        tools.push(tool);
    }
    Ok(tools)
}

pub fn load_from_default_dir() -> Result<Vec<OperatorTool>, String> {
    load_operator_tools(&operator_tools_dir())
}

impl OperatorTool {
    /// Only tools with a runner this node can execute are advertised.
    pub fn is_executable(&self) -> bool {
        !matches!(self.runner, OperatorRunner::None)
    }
}

/// Registered operator ids may be called; they still fail closed until a
/// runner type this node understands is present.
pub fn dispatch_operator_tool(tools: &[OperatorTool], name: &str) -> Result<String, String> {
    let Some(tool) = tools.iter().find(|t| t.id == name) else {
        return Err(format!("unknown tool {name:?}"));
    };
    match tool.runner {
        OperatorRunner::None => Err(format!(
            "operator tool '{}' is registered but has no runner — refused",
            tool.id
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(load_operator_tools(&missing).unwrap().is_empty());
    }

    #[test]
    fn loads_json_rows_and_refuses_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ping.json"),
            r#"{"id":"ping","summary":"ping"}"#,
        )
        .unwrap();
        let tools = load_operator_tools(dir.path()).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].id, "ping");
        assert!(matches!(tools[0].runner, OperatorRunner::None));
        assert!(dispatch_operator_tool(&tools, "ping")
            .unwrap_err()
            .contains("no runner"));
        assert!(dispatch_operator_tool(&tools, "missing")
            .unwrap_err()
            .contains("unknown tool"));

        std::fs::write(
            dir.path().join("ping2.json"),
            r#"{"id":"ping","summary":"dup"}"#,
        )
        .unwrap();
        let err = load_operator_tools(dir.path()).unwrap_err();
        assert!(err.contains("duplicate"), "{err}");
    }
}
