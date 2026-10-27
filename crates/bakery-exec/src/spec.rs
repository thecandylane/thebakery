use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spec {
    pub tools: BTreeMap<String, String>,
}

impl Spec {
    pub fn from_mise_toml(text: &str) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            tools: BTreeMap<String, toml::Value>,
        }
        let raw: Raw = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut tools = BTreeMap::new();
        for (name, value) in raw.tools {
            let version = match value {
                toml::Value::String(s) => s,
                other => {
                    return Err(format!(
                        "tool {name}: expected a version string, got {other:?}"
                    ))
                }
            };
            tools.insert(name, version);
        }
        Ok(Spec { tools })
    }

    pub fn load(workspace: &Path) -> Result<Self, String> {
        let path = workspace.join("mise.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Self::from_mise_toml(&text)
    }

    pub fn rust_version(&self) -> Option<&str> {
        self.tools.get("rust").map(String::as_str)
    }

    /// Tool names this backend does not (yet) provision. Only `"rust"` is
    /// supported today — everything else pinned in `mise.toml` is a reason
    /// `provision`/`provision_project_tools` must fail closed, never a tool
    /// silently skipped.
    pub fn unsupported_tools(&self) -> Vec<&str> {
        self.tools
            .keys()
            .map(String::as_str)
            .filter(|&name| name != "rust")
            .collect()
    }

    /// Canonical form for digesting: `BTreeMap` already serializes with sorted
    /// keys, so this JSON string is the same for any two equal specs.
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).expect("Spec always serializes")
    }

    pub fn digest(&self) -> String {
        blake3::hash(self.canonical_json().as_bytes())
            .to_hex()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rust_version() {
        let spec = Spec::from_mise_toml("[tools]\nrust = \"1.98.0\"\n").unwrap();
        assert_eq!(spec.rust_version(), Some("1.98.0"));
    }

    #[test]
    fn digest_is_stable_regardless_of_key_order() {
        let a = Spec::from_mise_toml("[tools]\nrust = \"1.98.0\"\nnode = \"22\"\n").unwrap();
        let b = Spec::from_mise_toml("[tools]\nnode = \"22\"\nrust = \"1.98.0\"\n").unwrap();
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn digest_changes_with_version() {
        let a = Spec::from_mise_toml("[tools]\nrust = \"1.98.0\"\n").unwrap();
        let b = Spec::from_mise_toml("[tools]\nrust = \"1.99.0\"\n").unwrap();
        assert_ne!(a.digest(), b.digest());
    }
}
