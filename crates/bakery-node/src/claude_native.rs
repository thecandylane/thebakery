//! Built-in Claude native-agent adapter.
//!
//! `claude-code` is Bakery's existing provider-model seam and remains
//! untouched.  This module owns the distinct `claude-native` identity: a
//! released Claude SDK agent translated to ACP by the upstream adapter.
//! Bakery pins the package in its lockfile and validates its installed
//! identity and entry before executing it; no frontend request can supply an
//! argv, package path, or login command.

use crate::state::AppState;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const ACCOUNT_ID: &str = "claude-native";
pub(crate) const ACCOUNT_LABEL: &str = "Claude native agent (subscription)";
pub(crate) const ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
/// The pin, compiled in by build.rs from bakery-client's package.json and
/// pnpm-lock.yaml — their one source of truth (V1). The node still refuses
/// any other installed version: Bakery's Review patch is written against
/// exactly this build of the adapter.
pub(crate) const ADAPTER_VERSION: &str = env!("BAKERY_ADAPTER_VERSION");
pub(crate) const ADAPTER_INTEGRITY: &str = env!("BAKERY_ADAPTER_INTEGRITY");
const PACKAGE_RELATIVE: &str = "@agentclientprotocol/claude-agent-acp";
/// The Claude Agent SDK the adapter drives; it carries the Claude Code engine
/// version it bundles as `claudeCodeVersion` in its manifest.
pub(crate) const SDK_PACKAGE: &str = "@anthropic-ai/claude-agent-sdk";
pub(crate) const MIN_NODE_MAJOR: u64 = 22;
const STATUS_TIMEOUT: Duration = Duration::from_secs(6);
const STATUS_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAdapter {
    pub(crate) node: PathBuf,
    pub(crate) entry: PathBuf,
    /// Read/exec grant for package code and its pinned dependencies.  It is
    /// code-only; the operator HOME and credential stores are not mounted.
    pub(crate) runtime_root: PathBuf,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ClaudeNativeStatus {
    pub(crate) installed: bool,
    pub(crate) connected: bool,
    /// `subscription`, `signed_out`, `api_key`, `external`, `unknown`, or
    /// `unavailable`.  API-key billing is deliberately not `connected`.
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    pub(crate) adapter_version: &'static str,
    pub(crate) adapter_integrity: &'static str,
    pub(crate) metadata_provenance: &'static str,
}

impl ClaudeNativeStatus {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            installed: false,
            connected: false,
            state: "unavailable",
            label: None,
            reason: Some(reason.into()),
            adapter_version: ADAPTER_VERSION,
            adapter_integrity: ADAPTER_INTEGRITY,
            metadata_provenance: "pinned_package_and_native_cli_probe",
        }
    }
}

fn package_at(prefix_or_package: &Path) -> Option<PathBuf> {
    // A source checkout root (bakery-client) has its own package.json, so
    // resolve the pinned dependency beneath node_modules before considering
    // the input itself as a package root.
    let under_node_modules = prefix_or_package
        .join("node_modules")
        .join(PACKAGE_RELATIVE);
    if under_node_modules.join("package.json").is_file() {
        return Some(under_node_modules);
    }
    let directly_under_root = prefix_or_package.join(PACKAGE_RELATIVE);
    if directly_under_root.join("package.json").is_file() {
        return Some(directly_under_root);
    }
    prefix_or_package
        .join("package.json")
        .is_file()
        .then_some(prefix_or_package.to_path_buf())
}

fn package_candidates(state: &AppState) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(root) = std::env::var("BAKERY_CLAUDE_AGENT_ACP_ROOT") {
        if !root.trim().is_empty() {
            roots.push(PathBuf::from(root));
        }
    }
    if let Some(source) = state.node.lock().unwrap().bakery_source_root.clone() {
        roots.push(PathBuf::from(source).join("bakery-client"));
    }
    // Development checkout.  This is a package lookup only; no credential
    // location is derived from the build-time path.
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bakery-client"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("claude-native"));
            roots.push(dir.join("../Resources/claude-native"));
        }
    }
    roots
        .into_iter()
        .filter_map(|root| package_at(&root))
        .collect()
}

fn outer_node_modules(path: &Path) -> Option<PathBuf> {
    // pnpm's real path contains two `node_modules` components.  Grant the
    // outermost one so dependency symlink targets are readable too.
    path.ancestors()
        .filter(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name == "node_modules")
        })
        .last()
        .map(Path::to_path_buf)
}

fn validate_package(package: &Path) -> Result<(PathBuf, PathBuf), String> {
    let manifest_bytes = std::fs::read(package.join("package.json"))
        .map_err(|_| "the pinned Claude ACP adapter package is not installed".to_string())?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| "the installed Claude ACP adapter package manifest is invalid".to_string())?;
    if manifest.get("name").and_then(serde_json::Value::as_str) != Some(ADAPTER_PACKAGE)
        || manifest.get("version").and_then(serde_json::Value::as_str) != Some(ADAPTER_VERSION)
    {
        return Err(format!(
            "the Claude native adapter must be exactly {ADAPTER_PACKAGE}@{ADAPTER_VERSION}; run pnpm install in bakery-client"
        ));
    }
    let entry = package.join("dist/index.js");
    if !entry.is_file() {
        return Err(
            "the pinned Claude ACP adapter is incomplete (dist/index.js is missing)".to_string(),
        );
    }
    let canonical_entry = entry
        .canonicalize()
        .map_err(|_| "the pinned Claude ACP adapter entry cannot be resolved".to_string())?;
    let runtime_root = outer_node_modules(package)
        .unwrap_or_else(|| package.to_path_buf())
        .canonicalize()
        .map_err(|_| "the Claude native adapter runtime root cannot be resolved".to_string())?;
    if !canonical_entry.starts_with(&runtime_root) {
        return Err(
            "the Claude native adapter entry escapes its verified runtime root".to_string(),
        );
    }
    Ok((canonical_entry, runtime_root))
}

/// `node --version` as the runtime reports it, e.g. `v22.12.0`.
pub(crate) fn node_version(node: &Path) -> Result<String, String> {
    let output = Command::new(node)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "Node.js is required for the Claude native adapter".to_string())?;
    if !output.status.success() {
        return Err("Node.js could not start for the Claude native adapter".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn node_major(node: &Path) -> Result<u64, String> {
    node_version(node)?
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(|| "Node.js reported an unreadable version".to_string())
}

pub(crate) fn resolve(state: &AppState) -> Result<ResolvedAdapter, String> {
    let node = crate::state::resolve_executable_path("node").map_err(|_| {
        "Setup needed: Node.js 22+ is required for the Claude native adapter".to_string()
    })?;
    if node_major(&node)? < MIN_NODE_MAJOR {
        return Err("Setup needed: Claude native requires Node.js 22 or newer".to_string());
    }
    let mut last_error = None;
    for package in package_candidates(state) {
        match validate_package(&package) {
            Ok((entry, runtime_root)) => {
                return Ok(ResolvedAdapter {
                    node,
                    entry,
                    runtime_root,
                })
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        format!(
            "Setup needed: install pinned {ADAPTER_PACKAGE}@{ADAPTER_VERSION} with pnpm install in bakery-client"
        )
    }))
}

/// The adapter as installed — the package `resolve` runs when one passes its
/// checks, else the first found — and the SDK Node loads for it. A
/// measurement for the Versions report, never a gate: it reports a wrong
/// version instead of refusing it.
pub(crate) struct InstalledAdapter {
    pub(crate) package: PathBuf,
    pub(crate) version: Option<String>,
    pub(crate) sdk: Option<PathBuf>,
    pub(crate) sdk_version: Option<String>,
    pub(crate) claude_code_version: Option<String>,
}

pub(crate) fn installed_adapter(state: &AppState) -> Option<InstalledAdapter> {
    let candidates = package_candidates(state);
    let package = candidates
        .iter()
        .find(|package| validate_package(package).is_ok())
        .or(candidates.first())?
        .clone();
    let field = |dir: &Path, key: &str| {
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("package.json")).ok()?).ok()?;
        manifest.get(key)?.as_str().map(str::to_string)
    };
    let sdk = package
        .canonicalize()
        .ok()
        .and_then(|dir| dependency_dir(&dir, SDK_PACKAGE));
    Some(InstalledAdapter {
        version: field(&package, "version"),
        sdk_version: sdk.as_deref().and_then(|dir| field(dir, "version")),
        claude_code_version: sdk
            .as_deref()
            .and_then(|dir| field(dir, "claudeCodeVersion")),
        sdk,
        package,
    })
}

/// Node's own lookup of `name` from inside the package at `from`: each
/// ancestor's `node_modules/<name>`, nearest first — which finds pnpm's
/// sibling layout (`.pnpm/<key>/node_modules/<name>`) exactly as Node does.
fn dependency_dir(from: &Path, name: &str) -> Option<PathBuf> {
    from.ancestors()
        .filter(|dir| dir.file_name().is_none_or(|n| n != "node_modules"))
        .map(|dir| dir.join("node_modules").join(name))
        .find(|dir| dir.join("package.json").is_file())
}

/// Bakery's OWN Claude credential directory, deliberately separate from the
/// operator's `~/.claude`.
///
/// The adapter honours `CLAUDE_CONFIG_DIR` and reads its credential from
/// there instead of `$HOME/.claude`. The sign-in terminal and `status()`
/// use this directory. A seat does not. It gets a stand-in copy elsewhere,
/// and this directory stays denied.
///
/// It also keeps Bakery off the host CLI's OAuth grant. Sharing one refresh
/// token would mean whichever side refreshed first silently invalidated the
/// other, logging the operator out of their own terminal `claude`.
pub(crate) fn credential_dir(state: &AppState) -> PathBuf {
    crate::claude_sign_in::vault_dir(state)
}

/// A claude-native seat's own config directory: the stand-in sign-in, plus a
/// copy of the operator's settings. Never the vault.
fn seat_piece(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn seat_config_dir(state: &AppState, stream_id: &str, seat: &str) -> PathBuf {
    state
        .base
        .join("cache")
        .join("claude-native")
        .join("seat-config")
        .join(seat_piece(stream_id))
        .join(seat_piece(seat))
}

fn copy_path(src: &Path, dest: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_path(&entry.path(), &dest.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // The last spawn left settings.json read-only; a copy can't open it.
    let _ = std::fs::remove_file(dest);
    std::fs::copy(src, dest).map_err(|e| e.to_string())?;
    Ok(())
}

/// Write this thread's stand-in sign-in where the seat will read it, and
/// copy settings across. The real credential file is not copied. `seen_at`
/// is where the seat sees the folder, when not at its own path (a
/// container's mount).
pub(crate) fn prepare_seat_config(
    state: &AppState,
    stream_id: &str,
    seat: &str,
    provider: crate::claude_providers::Provider,
    seen_at: Option<&Path>,
) -> Result<PathBuf, String> {
    let dir = seat_config_dir(state, stream_id, seat);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let source = credential_dir(state);
    if source != dir {
        for src in bakery_exec::claude_settings(&source) {
            let Some(name) = src.file_name() else { continue };
            // A seat's plugins, commands, subagents, output styles, skills,
            // rules and CLAUDE.md are installed below, the toolkit's among
            // them.
            let installed = name == "plugins" || name == "CLAUDE.md" || TOOLKIT_FOLDERS.iter().any(|f| name == *f);
            if src.exists() && !installed {
                copy_path(&src, &dir.join(name))?;
            }
        }
    }
    // Only the subscription's seat gets the OAuth-shaped vault copy: a
    // console or OpenRouter seat authenticates by env (ANTHROPIC_API_KEY /
    // ANTHROPIC_AUTH_TOKEN) alone, and Claude Code prefers a stored OAuth
    // sign-in over either — writing one here would silently put a
    // console/OpenRouter seat back on the subscription whenever the
    // operator also has one signed in.
    let subscription_credential = (provider == crate::claude_providers::Provider::Subscription)
        .then(|| crate::swap::subscription_credential(state, stream_id, ACCOUNT_ID))
        .flatten();
    if let Some(blob) = subscription_credential {
        let file = dir.join(".credentials.json");
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        out.write_all(blob.as_bytes())
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let plugins = install_toolkit_plugins(state, stream_id, &dir, seen_at.unwrap_or(&dir))?;
    install_toolkit_files(state, stream_id, &dir)?;
    write_toolkit_settings(state, stream_id, &dir, &plugins)?;
    Ok(dir)
}

/// The seat folders the toolkit writes Claude Code's own files into.
const TOOLKIT_FOLDERS: [&str; 5] = ["commands", "agents", "output-styles", "skills", "rules"];

/// A seat's slash commands, subagents, output styles, skills, rules and
/// CLAUDE.md, where Claude Code reads a user's own: the vault's, then the
/// toolkit's whole, its own winning a clash of names. Instructions open its
/// CLAUDE.md, each after the vault's; one named like a rule
/// (`rules/<path>.md`) is that rule, its frontmatter with it. Rebuilt every
/// spawn, so a removal or a turn-off reaches the seat. The toolkit's names
/// were checked as file names when they were added.
pub(crate) fn install_toolkit_files(state: &AppState, stream_id: &str, dir: &Path) -> Result<(), String> {
    use crate::toolkit::Kind;
    let vault = credential_dir(state);
    for folder in TOOLKIT_FOLDERS {
        match std::fs::remove_dir_all(dir.join(folder)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{folder}: {e}")),
            _ => {}
        }
        if vault != dir && vault.join(folder).exists() {
            copy_path(&vault.join(folder), &dir.join(folder))?;
        }
    }
    let mut memory = std::fs::read_to_string(vault.join("CLAUDE.md")).unwrap_or_default();
    let write = |path: &Path, text: &str| {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(path, format!("{}\n", text.trim_end())).map_err(|e| format!("{}: {e}", path.display()))
    };
    for item in crate::toolkit::for_stream(state, stream_id) {
        let file = match item.kind {
            Kind::Command => Path::new("commands").join(format!("{}.md", item.name.replace(':', "/"))),
            Kind::Agent => Path::new("agents").join(format!("{}.md", seat_piece(&item.name))),
            Kind::OutputStyle => Path::new("output-styles").join(format!("{}.md", item.name)),
            // A skill whole: the files that came with it, then its SKILL.md,
            // its own frontmatter (or one with its description: Claude Code
            // names it by its folder) over its text now.
            Kind::Skill => {
                let at = dir.join("skills").join(&item.name);
                let store = crate::toolkit::skill_dir(state, &item.id);
                if store.is_dir() {
                    copy_path(&store, &at)?;
                }
                let kept = std::fs::read_to_string(store.join("SKILL.md")).unwrap_or_default();
                let head = &kept[..kept.len() - crate::toolkit_port::without_frontmatter(&kept).len()];
                let head = match head.trim() {
                    "" => format!("---\ndescription: {}\n---", serde_json::Value::String(item.description.clone())),
                    head => head.to_string(),
                };
                write(&at.join("SKILL.md"), &format!("{head}\n\n{}", item.text.trim()))?;
                continue;
            }
            Kind::Instructions => match rule_path(&item.name) {
                Some(rule) => Path::new("rules").join(rule),
                None => {
                    memory.push_str(&format!("\n\n{}", item.text.trim()));
                    continue;
                }
            },
            _ => continue,
        };
        write(&dir.join(file), &item.text)?;
    }
    // The vault's own CLAUDE.md is never written over.
    if vault != dir {
        match memory.trim() {
            "" => match std::fs::remove_file(dir.join("CLAUDE.md")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("CLAUDE.md: {e}")),
                _ => {}
            },
            memory => write(&dir.join("CLAUDE.md"), memory)?,
        }
    }
    Ok(())
}

/// Where an instruction named like a rule (`rules/<path>.md`, each piece a
/// plain name) goes under `rules/`.
fn rule_path(name: &str) -> Option<&str> {
    let rel = name.strip_prefix("rules/")?;
    (rel.ends_with(".md") && rel.split('/').all(crate::toolkit::plain)).then_some(rel)
}

/// The toolkit's plugins, installed whole in a seat's own Claude config the
/// way Claude Code installs one: the files under `plugins/cache`, the
/// marketplace's record and catalog entry, the install record, and an empty
/// data folder (the seat can't write there; Claude Code only makes sure it
/// is there). Rebuilt every spawn, so a removal reaches the seat. `seen_at`
/// is where the seat sees `dir`. The ids to turn on.
pub(crate) fn install_toolkit_plugins(
    state: &AppState,
    stream_id: &str,
    dir: &Path,
    seen_at: &Path,
) -> Result<Vec<String>, String> {
    let root = dir.join("plugins");
    // ponytail: rebuilt every spawn (APFS clones keep the copies cheap);
    // sync by version if plugin folders grow large.
    match std::fs::remove_dir_all(&root) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", root.display())),
        _ => {}
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let now = chrono::Utc::now().to_rfc3339();
    let (mut installed, mut known) = (serde_json::Map::new(), serde_json::Map::new());
    let mut catalogs: std::collections::BTreeMap<String, Vec<serde_json::Value>> = Default::default();
    let mut ids = Vec::new();
    for item in crate::toolkit::for_stream(state, stream_id) {
        let Some(plugin) = item.plugin.filter(|_| item.kind == crate::toolkit::Kind::Plugin) else { continue };
        let Some((name, market)) = plugin.id.rsplit_once('@') else { continue };
        let store = crate::toolkit::plugin_dir(state, &item.id);
        if !store.is_dir() {
            continue;
        }
        let rel = Path::new("cache").join(market).join(name).join(&plugin.version);
        crate::toolkit_port::copy_tree(&store, &root.join(&rel)).map_err(|e| format!("plugin {}: {e}", plugin.id))?;
        let data: String = plugin.id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '-' }).collect();
        std::fs::create_dir_all(root.join("data").join(data)).map_err(|e| e.to_string())?;
        let seen = seen_at.join("plugins");
        installed.insert(
            plugin.id.clone(),
            serde_json::json!([{"scope": "user", "installPath": seen.join(&rel), "version": plugin.version, "installedAt": now}]),
        );
        known.insert(
            market.to_string(),
            serde_json::json!({
                "source": plugin.marketplace_source,
                "installLocation": seen.join("marketplaces").join(market),
                "lastUpdated": now,
                "autoUpdate": false,
            }),
        );
        catalogs.entry(market.to_string()).or_default().push(plugin.entry);
        ids.push(plugin.id);
    }
    let write = |path: PathBuf, value: serde_json::Value| -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    for (market, entries) in catalogs {
        let catalog = serde_json::json!({"name": market, "owner": {"name": "Bakery"}, "plugins": entries});
        write(root.join("marketplaces").join(&market).join(".claude-plugin").join("marketplace.json"), catalog)?;
    }
    write(root.join("installed_plugins.json"), serde_json::json!({"version": 2, "plugins": installed}))?;
    write(root.join("known_marketplaces.json"), serde_json::Value::Object(known))?;
    Ok(ids)
}

/// The toolkit's part of the per-seat settings.json (user scope for
/// CLAUDE_CONFIG_DIR): its hooks, and its plugins turned on. The Bakery
/// owns this file, so both keys are replaced whole on each call. The file
/// is set read-only (0o444) afterward.
fn write_toolkit_settings(
    state: &AppState,
    stream_id: &str,
    config_dir: &std::path::Path,
    plugins: &[String],
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let hooks: Vec<_> = crate::toolkit::for_stream(state, stream_id)
        .into_iter()
        .filter(|i| i.kind == crate::toolkit::Kind::Hook && i.event.is_some())
        .collect();
    let settings_file = config_dir.join("settings.json");
    let _ = std::fs::set_permissions(&settings_file, std::fs::Permissions::from_mode(0o644));
    let mut settings: serde_json::Value = if settings_file.exists() {
        let text = std::fs::read_to_string(&settings_file).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if !settings.is_object() {
        settings = serde_json::json!({});
    }
    match crate::toolkit::claude_hooks_json(&hooks) {
        Some(hooks_json) => { settings["hooks"] = hooks_json; }
        None => { settings.as_object_mut().unwrap().remove("hooks"); }
    }
    if plugins.is_empty() {
        settings.as_object_mut().unwrap().remove("enabledPlugins");
    } else {
        settings["enabledPlugins"] = plugins.iter().map(|id| (id.clone(), serde_json::Value::Bool(true))).collect();
    }
    // A seat's plugins are the toolkit's alone (the operator's answer:
    // nothing arrives or updates on its own): no claude.ai sync, and no
    // marketplace for it to fetch.
    settings["syncClaudeAiPlugins"] = serde_json::Value::Bool(false);
    settings.as_object_mut().unwrap().remove("extraKnownMarketplaces");
    crate::toolkit::mark_native_hooks(stream_id);
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&settings_file, text.as_bytes())
        .map_err(|e| format!("{}: {e}", settings_file.display()))?;
    std::fs::set_permissions(&settings_file, std::fs::Permissions::from_mode(0o444))
        .map_err(|e| format!("{}: {e}", settings_file.display()))?;
    Ok(())
}

/// Where the `security` stub lives. Outside [`credential_dir`]: that directory
/// is writable by the native process so it can refresh, and a stub sitting
/// inside it could be replaced.
fn security_shim_dir(state: &AppState) -> PathBuf {
    state.base.join("cache").join("claude-native").join("bin")
}

/// Claude Code's macOS credential service for one config directory.
///
/// The engine names the Keychain item `Claude Code-credentials-` plus the
/// first 8 hex characters of SHA-256 over the NFC form of `CLAUDE_CONFIG_DIR`.
/// Bakery's cache path is ASCII, so the string we export is already NFC.
/// The operator's own `~/.claude` item has no hash suffix; this name never
/// matches it.
fn keychain_service(config_dir: &str) -> String {
    let digest = Sha256::digest(config_dir.as_bytes());
    let mut hex = String::with_capacity(8);
    for byte in digest.iter().take(4) {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("Claude Code-credentials-{hex}")
}

fn keychain_account() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let ok = !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        user
    } else {
        "claude-code-user".to_string()
    }
}

fn credential_file(config_dir: &Path) -> PathBuf {
    config_dir.join(".credentials.json")
}

fn keychain_retired_marker(config_dir: &Path) -> PathBuf {
    config_dir.join("keychain-retired")
}

const CREDENTIAL_RECORD_LIMIT: u64 = 64 * 1024;
const KEYCHAIN_READ_TIMEOUT: Duration = Duration::from_secs(8);

/// Exit status `security` uses for "item not found". Claude treats 44 (and
/// 36) as an empty keychain and then reads the plaintext file. Any other
/// failure is a strict read error, and the file is ignored.
const SECURITY_ITEM_NOT_FOUND: i32 = 44;

fn file_is_credential_record(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_file() || meta.len() == 0 || meta.len() > CREDENTIAL_RECORD_LIMIT {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .is_some_and(|value| value.is_object())
}

fn write_credential_file(config_dir: &Path, bytes: &[u8]) -> Result<(), String> {
    let dest = credential_file(config_dir);
    let tmp = config_dir.join(".credentials.json.partial");
    let write_tmp = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_tmp {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "Bakery could not store the Claude subscription credential: {error}"
        ));
    }
    if let Err(error) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "Bakery could not store the Claude subscription credential: {error}"
        ));
    }
    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
    // This is the login Keychain-copy into the vault: forget any cached
    // access token so the door re-reads the fresh record (a no-op for a
    // seat's own config, which the token cache never keys).
    crate::claude_sign_in::clear_cache(&dest);
    Ok(())
}

fn retire_marker(config_dir: &Path) -> Result<(), String> {
    std::fs::write(keychain_retired_marker(config_dir), b"1").map_err(|error| {
        format!("Bakery could not record that the Claude Keychain item was retired: {error}")
    })
}

#[derive(Debug)]
enum KeychainRead {
    /// A JSON object. The bytes are the credential record; never log them.
    Record(Vec<u8>),
    /// Confirmed missing, or unreadable in a way that will not succeed on retry.
    Stop,
}

enum SecurityFinished {
    Output(std::process::Output),
    /// A Keychain dialog that nobody can answer from this process. Give up
    /// rather than blocking every status poll, and let Provider Accounts
    /// sign in straight into the plaintext file.
    TimedOut,
}

fn run_security(
    security_bin: &Path,
    args: &[&str],
    home: &Path,
) -> Result<SecurityFinished, String> {
    let mut child = Command::new(security_bin)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("USER", keychain_account())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Bakery could not read the Claude Keychain item: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Bakery could not read the Claude Keychain item".to_string())?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() <= CREDENTIAL_RECORD_LIMIT as usize {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(_) => break,
            }
        }
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < KEYCHAIN_READ_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Ok(SecurityFinished::TimedOut);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = reader.join();
                return Err(format!(
                    "Bakery could not read the Claude Keychain item: {error}"
                ));
            }
        }
    };
    let stdout = reader.join().unwrap_or_default();
    Ok(SecurityFinished::Output(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    }))
}

fn read_keychain_record(
    config_dir: &str,
    security_bin: &Path,
    home: &Path,
) -> Result<KeychainRead, String> {
    if !security_bin.is_file() {
        return Ok(KeychainRead::Stop);
    }
    let service = keychain_service(config_dir);
    let account = keychain_account();
    let output = match run_security(
        security_bin,
        &[
            "find-generic-password",
            "-a",
            &account,
            "-w",
            "-s",
            &service,
        ],
        home,
    )? {
        SecurityFinished::TimedOut => return Ok(KeychainRead::Stop),
        SecurityFinished::Output(output) => output,
    };
    // Any miss, refusal, or non-record must not block Provider Accounts.
    // Sign-in writes the plaintext file itself; a hard error here would
    // make that sign-in impossible.
    if !output.status.success() || output.stdout.len() > CREDENTIAL_RECORD_LIMIT as usize {
        return Ok(KeychainRead::Stop);
    }
    let Ok(text) = std::str::from_utf8(&output.stdout) else {
        return Ok(KeychainRead::Stop);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(KeychainRead::Stop);
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) if value.is_object() => Ok(KeychainRead::Record(trimmed.as_bytes().to_vec())),
        _ => Ok(KeychainRead::Stop),
    }
}

fn delete_keychain_item(config_dir: &str, security_bin: &Path, home: &Path) {
    if !security_bin.is_file() {
        return;
    }
    let service = keychain_service(config_dir);
    let account = keychain_account();
    let _ = run_security(
        security_bin,
        &["delete-generic-password", "-a", &account, "-s", &service],
        home,
    );
}

/// Copy a Keychain-only subscription into `.credentials.json`, once.
///
/// Sign-in used to run unsandboxed, so Claude wrote the macOS Keychain and
/// deleted the plaintext file. The turn is sandboxed and cannot reach
/// `securityd`, so `session/prompt` failed with `Authentication required`
/// while Provider Accounts still said connected. After this, login, status,
/// and the turn all use the file. The Keychain item for Bakery's own config
/// directory is deleted when the file is in place; the operator's `~/.claude`
/// item is a different service name and is not touched.
fn ensure_plaintext_credential(
    config_dir: &Path,
    security_bin: &Path,
    home: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|error| {
        format!("Bakery's Claude credential directory could not be created: {error}")
    })?;
    if keychain_retired_marker(config_dir).is_file() {
        return Ok(());
    }
    let dir_key = config_dir.to_string_lossy().into_owned();
    if file_is_credential_record(&credential_file(config_dir)) {
        delete_keychain_item(&dir_key, security_bin, home);
        return retire_marker(config_dir);
    }
    match read_keychain_record(&dir_key, security_bin, home)? {
        KeychainRead::Record(bytes) => {
            write_credential_file(config_dir, &bytes)?;
            delete_keychain_item(&dir_key, security_bin, home);
            retire_marker(config_dir)
        }
        KeychainRead::Stop => retire_marker(config_dir),
    }
}

/// `security` stub. Exit 44 forces Claude's plaintext fallback. The stub is
/// read-only to the seat; `/usr/bin/security` remains on PATH behind it, and
/// the seatbelt still cannot talk to `securityd`.
fn security_shim_script() -> String {
    format!("#!/bin/sh\nexit {SECURITY_ITEM_NOT_FOUND}\n")
}

fn install_security_shim(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| {
        format!("Bakery could not install the Claude credential helper: {error}")
    })?;
    let path = dir.join("security");
    let script = security_shim_script();
    let current = std::fs::read(&path).unwrap_or_default();
    if current != script.as_bytes() {
        let tmp = dir.join("security.new");
        let write_tmp = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o755)
                .open(&tmp)?;
            file.write_all(script.as_bytes())?;
            Ok(())
        })();
        if let Err(error) = write_tmp {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "Bakery could not install the Claude credential helper: {error}"
            ));
        }
        if let Err(error) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "Bakery could not install the Claude credential helper: {error}"
            ));
        }
    }
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    Ok(dir.to_path_buf())
}

pub(crate) struct AuthSurface {
    pub(crate) config_dir: PathBuf,
    /// `shim_dir:/bin:/usr/bin` — the stub is first, so `security` is never
    /// the real Keychain tool for login or the status probe.
    pub(crate) path: String,
    pub(crate) shim_dir: PathBuf,
}

fn operator_home(state: &AppState) -> PathBuf {
    state
        .base
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn prepare_auth_surface_with(state: &AppState, security_bin: &Path) -> Result<AuthSurface, String> {
    let config_dir = credential_dir(state);
    ensure_plaintext_credential(&config_dir, security_bin, &operator_home(state))?;
    let shim_dir = install_security_shim(&security_shim_dir(state))?;
    let path = format!("{}:/bin:/usr/bin", shim_dir.display());
    Ok(AuthSurface {
        config_dir,
        path,
        shim_dir,
    })
}

pub(crate) fn prepare_auth_surface(state: &AppState) -> Result<AuthSurface, String> {
    prepare_auth_surface_with(state, Path::new("/usr/bin/security"))
}

/// Claude Code's MCP tool search, on for a seat behind the door
/// (code.claude.com/docs/en/env-vars: `ENABLE_TOOL_SEARCH`).
pub(crate) const TOOL_SEARCH_ENV: (&str, &str) = ("ENABLE_TOOL_SEARCH", "true");

pub(crate) fn acp_spawn_config(
    state: &AppState,
    stream_id: &str,
    seat: &str,
    provider: crate::claude_providers::Provider,
    trusted: bool,
    mcp: Option<crate::pipe::McpSpawnConfig>,
) -> Result<crate::pipe::AcpSpawnConfig, String> {
    let adapter = resolve(state)?;
    let surface = prepare_auth_surface(state)?;
    let seat_config = prepare_seat_config(state, stream_id, seat, provider, None)?;
    let door = crate::doors::door(state, stream_id)?;
    let mut extra_env = vec![
        (
            "CLAUDE_CODE_TMPDIR".to_string(),
            bakery_exec::PRIVATE_TMPDIR_ENV_VALUE.to_string(),
        ),
        // The seat reads a stand-in sign-in here, never the vault. Settings
        // were copied beside it. `initialize` and `session/new` need no
        // credential, so a missing one only fails at `session/prompt`.
        (
            "CLAUDE_CONFIG_DIR".to_string(),
            seat_config.to_string_lossy().into_owned(),
        ),
        (
            bakery_exec::ACP_PATH_PREFIX_ENV.to_string(),
            surface.shim_dir.to_string_lossy().into_owned(),
        ),
        // Claude Code turns MCP tool search off when its base URL isn't
        // Anthropic's own, as the seat's door isn't. The door passes requests
        // through whole (`anthropic-beta`, `tool_reference` blocks), so it is
        // on, as in VS Code. An operator's env grant still sets its own.
        (TOOL_SEARCH_ENV.0.to_string(), TOOL_SEARCH_ENV.1.to_string()),
    ];
    extra_env.extend(crate::swap::subscription_env(
        state,
        stream_id,
        ACCOUNT_ID,
        provider.id(),
        door,
    ));
    Ok(crate::pipe::AcpSpawnConfig {
        binary: adapter.node,
        argv_suffix: vec![adapter.entry.to_string_lossy().into_owned()],
        config_copy: None,
        extra_env,
        trusted,
        mcp,
        // Its plugins run their own scripts; the folder stays write-denied.
        extra_process_exec_dirs: vec![adapter.runtime_root, surface.shim_dir, seat_config.join("plugins")],
        read_only: bakery_exec::claude_settings(&seat_config),
        // The stand-in directory only. The vault is not in this list.
        credential_dirs: vec![seat_config],
    })
}

fn classify_status(value: &serde_json::Value) -> ClaudeNativeStatus {
    let api_provider = value.get("apiProvider").and_then(serde_json::Value::as_str);
    let api_key_source = value
        .get("apiKeySource")
        .and_then(serde_json::Value::as_str);
    let subscription = value
        .get("subscriptionType")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let logged_in = value.get("loggedIn").and_then(serde_json::Value::as_bool);
    let base = |state, connected, label, reason| ClaudeNativeStatus {
        installed: true,
        connected,
        state,
        label,
        reason,
        adapter_version: ADAPTER_VERSION,
        adapter_integrity: ADAPTER_INTEGRITY,
        metadata_provenance: "pinned_package_and_native_cli_probe",
    };
    if api_provider.is_some_and(|provider| provider != "firstParty") {
        return base(
            "external",
            false,
            Some("External Claude backend".to_string()),
            Some(
                "Claude is using externally managed billing, not a Claude subscription login."
                    .to_string(),
            ),
        );
    }
    if api_key_source.is_some() {
        return base(
            "api_key",
            false,
            Some("Anthropic API key".to_string()),
            Some(
                "Claude is using API-key billing; Bakery will not label it as a subscription."
                    .to_string(),
            ),
        );
    }
    if logged_in == Some(true) && subscription.is_some() {
        let plan = subscription.unwrap();
        let title = plan
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let label = if title.to_ascii_lowercase().starts_with("claude") {
            title
        } else {
            format!("Claude {title}")
        };
        return base("subscription", true, Some(label), None);
    }
    if logged_in == Some(false) {
        return base(
            "signed_out",
            false,
            Some("Not signed in".to_string()),
            Some("Use the Claude subscription sign-in flow.".to_string()),
        );
    }
    base(
        "unknown",
        false,
        None,
        Some("Claude did not report a recognizable subscription identity.".to_string()),
    )
}

/// Ask the adapter's bundled native CLI for status.  Stdout is parsed into a
/// small capability result and then discarded; identity fields such as email
/// or organization never cross this function's boundary.
pub(crate) fn status(state: &AppState) -> ClaudeNativeStatus {
    let adapter = match resolve(state) {
        Ok(adapter) => adapter,
        Err(reason) => return ClaudeNativeStatus::unavailable(reason),
    };
    let host_home = state.base.parent().unwrap_or(Path::new("/"));
    // Probe the same credential file a turn will read. Keychain is invisible
    // on both sides: a successful Keychain read here used to report
    // "Connected" while the sandboxed prompt failed with Authentication required.
    let surface = match prepare_auth_surface(state) {
        Ok(surface) => surface,
        Err(reason) => {
            return ClaudeNativeStatus {
                installed: true,
                connected: false,
                state: "unknown",
                label: None,
                reason: Some(reason),
                adapter_version: ADAPTER_VERSION,
                adapter_integrity: ADAPTER_INTEGRITY,
                metadata_provenance: "pinned_package_and_native_cli_probe",
            };
        }
    };
    let mut child = match Command::new(&adapter.node)
        .arg(&adapter.entry)
        .args(["--cli", "auth", "status", "--json"])
        .env_clear()
        .env("HOME", host_home)
        .env("CLAUDE_CONFIG_DIR", &surface.config_dir)
        .env("PATH", &surface.path)
        .env("USER", std::env::var("USER").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return ClaudeNativeStatus::unavailable(
                "the pinned Claude native adapter could not start",
            )
        }
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < STATUS_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return ClaudeNativeStatus {
                    installed: true,
                    connected: false,
                    state: "unknown",
                    label: None,
                    reason: Some("Claude sign-in status timed out; retry.".to_string()),
                    adapter_version: ADAPTER_VERSION,
                    adapter_integrity: ADAPTER_INTEGRITY,
                    metadata_provenance: "pinned_package_and_native_cli_probe",
                };
            }
            Err(_) => {
                return ClaudeNativeStatus::unavailable("Claude sign-in status could not be read");
            }
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(_) => {
            return ClaudeNativeStatus::unavailable("Claude sign-in status could not be collected")
        }
    };
    if output.stdout.len() > STATUS_MAX_BYTES {
        return ClaudeNativeStatus::unavailable(
            "Claude sign-in status exceeded the safe response limit",
        );
    }
    match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
        Ok(value) if value.is_object() => classify_status(&value),
        _ => ClaudeNativeStatus {
            installed: true,
            connected: false,
            state: "unknown",
            label: None,
            reason: Some("Claude did not return a readable sign-in status.".to_string()),
            adapter_version: ADAPTER_VERSION,
            adapter_integrity: ADAPTER_INTEGRITY,
            metadata_provenance: "pinned_package_and_native_cli_probe",
        },
    }
}

pub(crate) fn login_program_and_args(state: &AppState) -> Result<(PathBuf, Vec<String>), String> {
    let adapter = resolve(state)?;
    Ok((
        adapter.node,
        vec![
            adapter.entry.to_string_lossy().into_owned(),
            "--cli".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--claudeai".to_string(),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seat's settings turn on the toolkit's plugins and nothing else, and
    /// leave nothing to bring plugins in on its own: no claude.ai sync, no
    /// marketplace to fetch (the operator's answer).
    #[test]
    fn a_seats_settings_turn_on_the_toolkits_plugins_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let seat = dir.path().join("seat");
        std::fs::create_dir_all(&seat).unwrap();
        std::fs::write(
            seat.join("settings.json"),
            r#"{"theme":"dark","enabledPlugins":{"old@mkt":true},"extraKnownMarketplaces":{"m":{"source":{"source":"github","repo":"a/b"}}}}"#,
        )
        .unwrap();
        write_toolkit_settings(&state, "s:any", &seat, &["fmt@mkt".to_string()]).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(seat.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"], serde_json::json!({"fmt@mkt": true}));
        assert_eq!(settings["syncClaudeAiPlugins"], false);
        assert!(settings.get("extraKnownMarketplaces").is_none());
        assert_eq!(settings["theme"], "dark");
    }

    /// A seat's settings.json is left read-only; its next spawn still copies
    /// the vault's over it.
    #[test]
    fn a_seats_read_only_settings_are_replaced_on_its_next_spawn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (vault, seat) = (dir.path().join("vault.json"), dir.path().join("seat.json"));
        std::fs::write(&vault, "{\"theme\":\"dark\"}").unwrap();
        std::fs::write(&seat, "{}").unwrap();
        std::fs::set_permissions(&seat, std::fs::Permissions::from_mode(0o444)).unwrap();
        copy_path(&vault, &seat).unwrap();
        assert_eq!(std::fs::read_to_string(&seat).unwrap(), "{\"theme\":\"dark\"}");
    }

    /// pnpm keeps a package's dependencies beside it under
    /// `.pnpm/<key>/node_modules`; the SDK must be found where Node would load
    /// it from — the nearest `node_modules` up from the adapter itself.
    #[test]
    fn the_sdk_is_found_where_node_would_load_it_for_the_adapter() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("node_modules/.pnpm/acp@1/node_modules");
        let adapter = store.join(PACKAGE_RELATIVE);
        let sdk = store.join(SDK_PACKAGE);
        let decoy = root.path().join("node_modules").join(SDK_PACKAGE);
        for dir in [&adapter, &sdk, &decoy] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("package.json"), "{}").unwrap();
        }
        assert_eq!(dependency_dir(&adapter, SDK_PACKAGE), Some(sdk));
        assert_eq!(dependency_dir(root.path(), "nope"), None);
    }

    /// The bug this whole module got wrong once: `status()` probed the
    /// operator's real `$HOME` while a turn ran against an empty per-seat
    /// HOME, so Provider Accounts reported "Connected / Claude Max" while
    /// every `session/prompt` failed with a bare `Authentication required`.
    /// Both surfaces must name the SAME directory or the panel can lie again.
    #[test]
    fn the_signed_in_surface_and_the_turn_surface_read_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(
            dir.path(),
            std::sync::Arc::new(crate::testutil::StubModel),
        );

        let credentials = credential_dir(&state);
        assert!(
            credentials.starts_with(state.base.as_path()),
            "Bakery's credential dir must live under ~/.bakery, got {}",
            credentials.display()
        );

        // Never the operator's own store: sharing one OAuth grant would let a
        // Bakery refresh invalidate the host CLI's token, and vice versa.
        let host_home = state.base.parent().unwrap();
        assert!(
            !credentials.starts_with(host_home.join(".claude")),
            "Bakery must not borrow the operator's ~/.claude"
        );

        let Ok(config) = acp_spawn_config(&state, "wt:turn", "orchestrator", crate::claude_providers::Provider::Subscription, true, None) else {
            // The pinned adapter is not installed in this environment; the
            // directory invariants above are still the ones that regress.
            return;
        };

        let configured = config
            .extra_env
            .iter()
            .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
            .map(|(_, v)| v.clone())
            .expect("a native turn must be told where its stand-in sign-in lives");
        assert_ne!(
            std::path::Path::new(&configured),
            credentials.as_path(),
            "the turn must not be pointed at the vault status() probes"
        );
        assert!(
            !config.credential_dirs.contains(&credentials),
            "the sandbox must not grant the vault"
        );
        assert!(
            config.credential_dirs.iter().all(|dir| dir == Path::new(&configured)),
            "the only credential directory is the stand-in config"
        );
        let token = config
            .extra_env
            .iter()
            .find(|(key, _)| key == "CLAUDE_CODE_OAUTH_TOKEN")
            .map(|(_, value)| value.clone())
            .expect("the seat holds the minted stand-in");
        assert!(token.starts_with("bakery-stand-in-claude-"), "{token}");
        assert!(!token.contains("real-"));
        assert!(
            config.extra_env.iter().any(|(key, value)| key == "ENABLE_TOOL_SEARCH" && value == "true"),
            "MCP tool search stays on behind the door"
        );
        for kept in ["CLAUDE.md", "rules", "skills", "commands", "agents"] {
            assert!(config.read_only.iter().any(|p| p.ends_with(kept)), "{kept} must be read-only to the seat");
        }
        let shim = config
            .extra_env
            .iter()
            .find(|(key, _)| key == bakery_exec::ACP_PATH_PREFIX_ENV)
            .map(|(_, value)| value.clone())
            .expect(
                "a native turn must shadow security so Keychain cannot hide the credential file",
            );
        assert!(
            config
                .extra_process_exec_dirs
                .iter()
                .any(|dir| dir == Path::new(&shim)),
            "the seatbelt must be allowed to execute the security stub"
        );
        assert!(
            !Path::new(&shim).starts_with(&credentials),
            "the stub must live outside the credential directory the model can write"
        );
    }

    #[test]
    fn keychain_service_name_is_claude_codes_sha256_prefix() {
        // SHA-256("abc") begins ba7816bf. Claude uses the first 8 hex chars.
        assert_eq!(keychain_service("abc"), "Claude Code-credentials-ba7816bf");
        assert_ne!(
            keychain_service("/Users/operator/.bakery/cache/claude-native/config"),
            "Claude Code-credentials",
            "Bakery's item must not be the operator's ~/.claude item, which has no hash suffix"
        );
    }

    #[test]
    fn security_shim_exits_item_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let shim_dir = install_security_shim(dir.path()).unwrap();
        let status = Command::new(shim_dir.join("security"))
            .arg("find-generic-password")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(SECURITY_ITEM_NOT_FOUND));
    }

    fn fake_security(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("security");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn keychain_record_is_copied_to_the_credential_file_and_not_echoed() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let log = dir.path().join("argv.log");
        let secret = "test-only-token";
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = find-generic-password ]; then\n  printf '%s' '{{ \"claudeAiOauth\": {{ \"accessToken\": \"{secret}\" }} }}'\n  exit 0\nfi\nexit 0\n",
            log.display()
        );
        let bin = fake_security(dir.path(), &script);
        ensure_plaintext_credential(&config, &bin, dir.path()).unwrap();

        let stored = std::fs::read_to_string(credential_file(&config)).unwrap();
        assert!(stored.contains(secret));
        assert!(stored.contains("claudeAiOauth"));
        let mode = std::fs::metadata(credential_file(&config))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(argv.contains(&keychain_service(&config.to_string_lossy())));
        assert!(argv.contains("delete-generic-password"));
        assert!(keychain_retired_marker(&config).is_file());

        let leaked = format!("{secret}-not-json");
        let again = read_keychain_record(
            &config.to_string_lossy(),
            &fake_security(
                dir.path(),
                &format!("#!/bin/sh\nprintf '%s' '{leaked}'\nexit 0\n"),
            ),
            dir.path(),
        )
        .unwrap();
        assert!(matches!(again, KeychainRead::Stop));
        let shown = format!("{again:?}");
        assert!(
            !shown.contains(&leaked),
            "credential bytes leaked into the result: {shown}"
        );
    }

    #[test]
    fn a_retired_keychain_is_not_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let log = dir.path().join("argv.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 44\n",
            log.display()
        );
        let bin = fake_security(dir.path(), &script);
        ensure_plaintext_credential(&config, &bin, dir.path()).unwrap();
        assert!(!credential_file(&config).exists());
        assert!(keychain_retired_marker(&config).is_file());

        let missing = dir.path().join("does-not-exist");
        ensure_plaintext_credential(&config, &missing, dir.path()).unwrap();
        let calls = std::fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(calls, 1, "a second prepare must not spawn security");
    }

    #[test]
    fn the_path_prefix_is_not_a_plug_env_key() {
        let mut env = std::collections::HashMap::new();
        env.insert(
            bakery_exec::ACP_PATH_PREFIX_ENV.to_string(),
            "/tmp/shim".to_string(),
        );
        let err = crate::state::validate_acp_plug_env(&env).unwrap_err();
        assert!(err.contains(bakery_exec::ACP_PATH_PREFIX_ENV));
    }

    #[test]
    fn login_and_turns_share_the_security_stub() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(
            dir.path(),
            std::sync::Arc::new(crate::testutil::StubModel),
        );
        let surface = prepare_auth_surface_with(&state, Path::new("/no/such/security")).unwrap();
        assert!(surface
            .path
            .starts_with(&format!("{}:", surface.shim_dir.display())));
        assert_eq!(surface.config_dir, credential_dir(&state));
        assert!(!surface.shim_dir.starts_with(&surface.config_dir));
        let status = Command::new(surface.shim_dir.join("security"))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(SECURITY_ITEM_NOT_FOUND));
    }

    #[test]
    fn status_never_calls_an_api_key_a_subscription() {
        let status = classify_status(&serde_json::json!({
            "loggedIn": true,
            "subscriptionType": "max",
            "apiKeySource": "env",
            "email": "must-not-cross@example.invalid"
        }));
        assert_eq!(status.state, "api_key");
        assert!(!status.connected);
        let wire = serde_json::to_string(&status).unwrap();
        assert!(!wire.contains("must-not-cross"));
    }

    #[test]
    fn subscription_status_exposes_plan_but_not_identity() {
        let status = classify_status(&serde_json::json!({
            "loggedIn": true,
            "subscriptionType": "max",
            "email": "must-not-cross@example.invalid",
            "orgName": "also-private"
        }));
        assert_eq!(status.state, "subscription");
        assert!(status.connected);
        assert_eq!(status.label.as_deref(), Some("Claude Max"));
        let wire = serde_json::to_string(&status).unwrap();
        assert!(!wire.contains("must-not-cross"));
        assert!(!wire.contains("also-private"));
    }
}
