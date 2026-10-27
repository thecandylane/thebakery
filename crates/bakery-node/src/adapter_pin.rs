//! The Claude native adapter's pin, read from the files pnpm owns — the exact
//! version in bakery-client/package.json and the integrity pnpm locked for it
//! — so nothing keeps a second copy by hand (V1). build.rs compiles both into
//! the node; the node itself only uses [`exact_version`]. std and serde_json
//! only, because build.rs includes this file too.

/// A release, not a range: `MAJOR.MINOR.PATCH` with an optional prerelease or
/// build suffix. A range would let any install pick a release no one reviewed.
pub(crate) fn exact_version(spec: &str) -> bool {
    let core = spec.split(['-', '+']).next().unwrap_or_default();
    core.split('.').count() == 3
        && core
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && spec
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-+.".contains(&b))
}

/// The exact version `package` is pinned to in a package.json.
#[cfg_attr(not(test), allow(dead_code))] // build.rs's
pub(crate) fn pinned_version(package_json: &str, package: &str) -> Option<String> {
    let manifest: serde_json::Value = serde_json::from_str(package_json).ok()?;
    let spec = manifest.get("dependencies")?.get(package)?.as_str()?;
    exact_version(spec).then(|| spec.to_string())
}

/// The integrity pnpm-lock.yaml records for `package@version` — the
/// `resolution: {integrity: …}` under its `packages:` entry. The same key also
/// appears with a value on its own line (`patchedDependencies:`) or with a
/// suffix (`snapshots:`), and neither matches.
#[cfg_attr(not(test), allow(dead_code))] // build.rs's
pub(crate) fn locked_integrity(lock: &str, package: &str, version: &str) -> Option<String> {
    let quoted = format!("'{package}@{version}':");
    let bare = format!("{package}@{version}:");
    let mut lines = lock.lines().skip_while(|line| {
        let entry = line.trim();
        entry != quoted && entry != bare
    });
    let key = lines.next()?;
    let indent = key.len() - key.trim_start().len();
    lines
        .take_while(|line| line.trim().is_empty() || line.len() - line.trim_start().len() > indent)
        .find_map(|line| line.trim().strip_prefix("resolution: {integrity: "))
        .and_then(|rest| rest.split([',', '}']).next())
        .map(|integrity| integrity.trim().to_string())
        .filter(|integrity| !integrity.is_empty())
}

/// The patch pnpm applies to `package@version`: its path, relative to
/// pnpm-workspace.yaml, from that file's `patchedDependencies:`.
#[cfg_attr(not(test), allow(dead_code))] // build.rs's
pub(crate) fn patch_path(workspace: &str, package: &str, version: &str) -> Option<String> {
    let key = format!("{package}@{version}");
    let keys = [format!("'{key}':"), format!("\"{key}\":"), format!("{key}:")];
    workspace
        .lines()
        .skip_while(|line| line.trim_end() != "patchedDependencies:")
        .skip(1)
        .take_while(|line| line.trim().is_empty() || line.starts_with([' ', '\t']))
        .find_map(|line| {
            let entry = line.trim();
            keys.iter().find_map(|k| entry.strip_prefix(k.as_str()))
        })
        .map(|path| path.trim().trim_matches(['\'', '"']).to_string())
        .filter(|path| !path.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";

    #[test]
    fn the_patch_is_the_one_pnpm_applies_to_that_release() {
        let workspace = "allowBuilds:\n  esbuild: true\n\npatchedDependencies:\n  '@agentclientprotocol/claude-agent-acp@0.89.1': patches/acp@0.89.1.patch\n  other@1.0.0: patches/other.patch\n";
        assert_eq!(patch_path(workspace, PACKAGE, "0.89.1").as_deref(), Some("patches/acp@0.89.1.patch"));
        assert_eq!(patch_path(workspace, "other", "1.0.0").as_deref(), Some("patches/other.patch"));
        assert_eq!(patch_path(workspace, PACKAGE, "0.84.0"), None, "another release has no patch");
        assert_eq!(patch_path("allowBuilds:\n  esbuild: true\n", PACKAGE, "0.89.1"), None);
    }

    #[test]
    fn only_an_exact_release_is_a_pin() {
        for exact in ["0.84.0", "10.2.33", "1.0.0-beta.1", "1.0.0+build.7"] {
            assert!(exact_version(exact), "{exact}");
        }
        for range in [
            "", "^0.84.0", "~0.84.0", ">=0.84.0", "0.84", "0.84.x", "1.x", "*", "latest",
            "1.0.0 - 2.0.0", "1.0.0 || 2.0.0", "workspace:*", "npm:other@1.0.0", "0.84.0; rm",
        ] {
            assert!(!exact_version(range), "{range:?}");
        }
    }

    #[test]
    fn the_pin_comes_from_the_dependency_entry_and_must_be_exact() {
        let manifest = |spec: &str| {
            format!(r#"{{"name":"x","dependencies":{{"{PACKAGE}":"{spec}","react":"^19.3.0"}}}}"#)
        };
        assert_eq!(pinned_version(&manifest("0.85.0"), PACKAGE).as_deref(), Some("0.85.0"));
        assert_eq!(pinned_version(&manifest("^0.85.0"), PACKAGE), None);
        assert_eq!(pinned_version(r#"{"dependencies":{}}"#, PACKAGE), None);
        assert_eq!(pinned_version("not json", PACKAGE), None);
    }

    /// The shape of pnpm-lock.yaml v9, trimmed: the package's key appears
    /// three times and only the `packages:` entry carries the integrity.
    #[test]
    fn the_integrity_is_read_from_the_packages_entry_only() {
        let lock = "\
lockfileVersion: '9.0'

patchedDependencies:
  '@agentclientprotocol/claude-agent-acp@0.84.0': b4d0acb4

packages:

  '@agentclientprotocol/claude-agent-acp@0.84.0':
    resolution: {integrity: sha512-old==}
    engines: {node: '>=22'}

  '@agentclientprotocol/claude-agent-acp@0.85.0':
    engines: {node: '>=22'}
    resolution: {integrity: sha512-new==, tarball: https://example.test/x.tgz}

  '@agentclientprotocol/sdk@1.5.1':
    resolution: {integrity: sha512-sdk==}

  diff@9.0.0:
    resolution: {integrity: sha512-diff==}

snapshots:

  '@agentclientprotocol/claude-agent-acp@0.84.0(patch_hash=b4d0acb4)':
    dependencies:
      diff: 9.0.0
";
        assert_eq!(locked_integrity(lock, PACKAGE, "0.84.0").as_deref(), Some("sha512-old=="));
        assert_eq!(locked_integrity(lock, PACKAGE, "0.85.0").as_deref(), Some("sha512-new=="));
        assert_eq!(locked_integrity(lock, "diff", "9.0.0").as_deref(), Some("sha512-diff=="));
        assert_eq!(locked_integrity(lock, PACKAGE, "0.86.0"), None, "not locked");
        // An entry without its own resolution never borrows the next one's.
        let bare = "packages:\n\n  'p@1.0.0':\n    engines: {}\n\n  'q@1.0.0':\n    resolution: {integrity: sha512-q==}\n";
        assert_eq!(locked_integrity(bare, "p", "1.0.0"), None);
    }

    /// The real files, as build.rs reads them: the pin the node compiled in
    /// is exactly what they say.
    #[test]
    fn the_compiled_pin_matches_bakery_client() {
        let client = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bakery-client");
        let read = |name: &str| std::fs::read_to_string(client.join(name)).unwrap();
        let version = pinned_version(&read("package.json"), PACKAGE).unwrap();
        assert_eq!(version, crate::claude_native::ADAPTER_VERSION);
        assert_eq!(
            locked_integrity(&read("pnpm-lock.yaml"), PACKAGE, &version).as_deref(),
            Some(crate::claude_native::ADAPTER_INTEGRITY)
        );
    }
}
