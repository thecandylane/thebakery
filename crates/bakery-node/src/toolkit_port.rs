//! The VS Code tool port (the operator's name for it): what the operator's
//! Claude Code setup holds (`~/.claude` and `~/.claude.json`, which Claude
//! Code in VS Code reads), offered as a list to tick into the Toolkit,
//! Bakery-wide or for one project. Read only when the operator asks, and
//! only the files Claude Code documents; a credential file is never opened.
//! No key or secret comes across: an MCP server's environment and header
//! values, a secret-looking argument or address part are left out and named,
//! for the operator to enter again through the Bakery's sign-ins; a hook
//! whose command holds one stays behind; a key in a skill's or an
//! instruction's text is replaced. Skills, MCP servers, hooks, instructions
//! and rules become Toolkit items: a skill with its whole folder, an
//! instruction file with the files it imports, a rule with its frontmatter,
//! so a Claude seat gets each as Claude Code reads it. Plugins, slash
//! commands, subagents and output styles come across whole, for Claude
//! seats, a plugin with its parts recorded.

use crate::state::AppState;
use crate::toolkit::{self, Kind, PluginParts, PluginRecord, SetReq, ToolkitItem};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `~/.claude.json` keeps far more than settings; read up to this much.
const MAX_CLAUDE_JSON_BYTES: u64 = 32 << 20;
/// Files listed in one folder, at most.
const MAX_WALK: usize = 2000;
/// What a key in a skill's or an instruction's text becomes.
pub(crate) const LEFT_OUT: &str = "[key left out]";
/// A skill's frontmatter only Claude Code acts on.
const CLAUDE_ONLY_SKILL_KEYS: &[&str] = &[
    "allowed-tools",
    "disallowed-tools",
    "model",
    "effort",
    "context",
    "agent",
    "hooks",
    "paths",
    "shell",
    "disable-model-invocation",
    "user-invocable",
    "arguments",
    "argument-hint",
    "background",
];
/// What Claude Code fills in a skill's text before Claude reads it.
const PLACEHOLDERS: &[&str] = &["$ARGUMENTS", "${CLAUDE_SKILL_DIR}", "${CLAUDE_SESSION_ID}", "${CLAUDE_PROJECT_DIR}", "${CLAUDE_EFFORT}", "!`"];

/// The operator's home, where Claude Code keeps its setup.
fn operator_home(state: &AppState) -> PathBuf {
    if cfg!(test) {
        return state.base.join("operator-home");
    }
    state.base.parent().map(Path::to_path_buf).unwrap_or_default()
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Found {
    /// What the import names it by.
    pub(crate) key: String,
    /// What it is in Claude Code: `skill`, `mcp_server`, `hook`,
    /// `instructions`, `rule`, `plugin`, `command`, `agent` or `output_style`.
    pub(crate) found: &'static str,
    /// What it becomes in the Toolkit; none when it can't come across yet.
    pub(crate) kind: Option<Kind>,
    pub(crate) name: String,
    pub(crate) description: String,
    /// Where it is, from the home folder.
    pub(crate) source: String,
    /// Why it can't come across, when it can't.
    pub(crate) reason: Option<String>,
    /// The keys and secrets left out, by name only.
    pub(crate) left_out: Vec<String>,
    /// What changes on the way, or won't work on the other side.
    pub(crate) warnings: Vec<String>,
    /// The Toolkit item it would change, in the scope asked about.
    pub(crate) in_toolkit: Option<String>,
    /// A plugin's parts.
    pub(crate) parts: Option<PluginParts>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Port {
    pub(crate) found: Vec<Found>,
}

/// What the import writes for one item.
enum Draft {
    Item(Item),
    Plugin { record: PluginRecord, root: PathBuf },
}

/// The Toolkit item it becomes.
#[derive(Default)]
struct Item {
    description: String,
    text: String,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    event: Option<String>,
    matcher: Option<String>,
    /// A skill's folder, kept whole for Claude seats (`toolkit::skill_dir`).
    folder: Option<PathBuf>,
}

struct Candidate {
    found: Found,
    draft: Option<Draft>,
}

fn found(key: String, what: &'static str, kind: Option<Kind>, name: String, source: String) -> Found {
    Found {
        key,
        found: what,
        kind,
        name,
        description: String::new(),
        source,
        reason: None,
        left_out: Vec::new(),
        warnings: Vec::new(),
        in_toolkit: None,
        parts: None,
    }
}

fn stays(mut found: Found, reason: impl Into<String>) -> Candidate {
    found.reason = Some(reason.into());
    Candidate { found, draft: None }
}

/// Everything the operator's Claude Code setup holds: instructions, skills,
/// MCP servers, hooks, plugins, then what has no Toolkit kind yet.
fn discover(home: &Path) -> Vec<Candidate> {
    let claude = home.join(".claude");
    let settings = json_at(&claude.join("settings.json"));
    let mut out = Vec::new();
    instructions(home, &claude, &mut out);
    skills(home, &claude, &mut out);
    mcp_servers(home, &mut out);
    hooks(home, &settings, &mut out);
    plugins(home, &claude, &settings, &mut out);
    claude_files(home, &claude, &mut out);
    out
}

fn json_at(path: &Path) -> Value {
    crate::control_plane::read(path)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn shown(home: &Path, path: &Path) -> String {
    path.strip_prefix(home)
        .map(|rel| format!("~/{}", rel.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

/// A folder's files, as paths from it: never through a link, never a
/// credential file, at most [`MAX_WALK`].
fn walk(root: &Path) -> Vec<String> {
    fn go(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if out.len() >= MAX_WALK {
                return;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() && depth < 4 {
                go(root, &path, depth + 1, out);
            } else if kind.is_file() && !bakery_exec::is_credential_filename(&path) {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut out = Vec::new();
    go(root, root, 0, &mut out);
    out
}

pub(crate) fn without_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return text;
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        at += line.len();
        if line.trim() == "---" {
            return &rest[at..];
        }
    }
    text
}

/// The top-level keys of a file's frontmatter.
fn frontmatter_keys(text: &str) -> Vec<String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Vec::new();
    }
    lines
        .take_while(|l| l.trim() != "---")
        .filter(|l| !l.starts_with([' ', '\t', '-', '#']))
        .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_string()))
        .filter(|k| !k.is_empty() && !k.contains(' '))
        .collect()
}

fn first_line(text: &str) -> String {
    let line = text.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("");
    redact(&line.chars().take(160).collect::<String>()).0
}

/// A credential's shape: a known key prefix, a signed web token, a private
/// key. ponytail: prefixes, not entropy; a key with no known prefix is
/// caught only by the name it sits under.
fn token_shaped(word: &str) -> bool {
    const PREFIXES: &[(&str, usize)] = &[
        ("sk-", 20),
        ("xai-", 20),
        ("ghp_", 30),
        ("gho_", 30),
        ("ghu_", 30),
        ("ghs_", 30),
        ("ghr_", 30),
        ("github_pat_", 30),
        ("glpat-", 20),
        ("xoxb-", 20),
        ("xoxp-", 20),
        ("xoxa-", 20),
        ("xapp-", 20),
        ("AKIA", 20),
        ("ASIA", 20),
        ("AIza", 35),
        ("ya29.", 20),
        ("npm_", 30),
        ("pypi-", 30),
        ("hf_", 30),
        ("gsk_", 30),
        ("r8_", 30),
        ("sk_live_", 20),
        ("sk_test_", 20),
        ("rk_live_", 20),
        ("dop_v1_", 30),
        ("shpat_", 30),
        ("lin_api_", 30),
        ("ntn_", 30),
    ];
    let w = word.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '`'));
    let tokenish = |rest: &str| rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    PREFIXES.iter().any(|(prefix, min)| w.starts_with(prefix) && w.len() >= *min && tokenish(&w[prefix.len()..]))
        || (w.starts_with("eyJ") && w.len() > 40 && w.split('.').count() == 3 && tokenish(w))
        || w.contains("-----BEGIN")
}

/// A name that marks its value as a credential: an environment variable's,
/// a flag's, a header's, an address's query.
fn secret_name(name: &str) -> bool {
    let n = name.trim_start_matches('-').trim_end_matches(':').to_ascii_uppercase().replace('-', "_");
    crate::machine_access::looks_like_secret(&n)
        || n.contains("APIKEY")
        || n.contains("AUTHORIZATION")
        || n.contains("COOKIE")
        || ["KEY", "AUTH", "PASS", "PWD", "SIG", "SIGNATURE", "BEARER", "BASIC"].contains(&n.as_str())
        || n.ends_with("_KEY")
        || n.ends_with("_AUTH")
        || n.ends_with("_PASS")
}

/// A value written out, not taken from the environment or left blank.
fn literal(value: &str) -> bool {
    let v = value.trim_matches(|c: char| matches!(c, '"' | '\''));
    !v.is_empty() && !v.starts_with('$') && !v.starts_with('<') && !v.starts_with('-')
}

/// The secret-looking words of a command line: a flag or `name=` whose name
/// marks a credential, with a value written out; what follows `Bearer` or a
/// secret-named header; a key's shape. Each left-out word's index, and a
/// label for it that never shows the value.
pub(crate) fn secret_words(words: &[String]) -> (Vec<usize>, Vec<String>) {
    let (mut drop, mut labels) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < words.len() {
        let w = words[i].trim_matches(|c: char| matches!(c, '"' | '\''));
        let next_literal = words.get(i + 1).is_some_and(|v| literal(v));
        if let Some((name, value)) = w.split_once('=') {
            if secret_name(name) && literal(value) {
                drop.push(i);
                labels.push(format!("argument {name}="));
                i += 1;
                continue;
            }
        } else if w.starts_with('-') && secret_name(w) && next_literal {
            drop.extend([i, i + 1]);
            labels.push(format!("argument {w} and its value"));
            i += 2;
            continue;
        } else if (w.ends_with(':') && secret_name(w) || ["bearer", "basic"].iter().any(|s| w.eq_ignore_ascii_case(s))) && next_literal {
            // `Authorization: Bearer <key>`: the scheme's own turn takes the key.
            let scheme = words[i + 1].trim_matches(|c: char| matches!(c, '"' | '\''));
            if w.ends_with(':') && ["bearer", "basic", "token"].iter().any(|s| scheme.eq_ignore_ascii_case(s)) {
                if scheme.eq_ignore_ascii_case("token") {
                    drop.push(i + 2);
                    labels.push(format!("the value after {w} Token"));
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            drop.push(i + 1);
            labels.push(format!("the value after {w}"));
            i += 2;
            continue;
        }
        if token_shaped(w) {
            drop.push(i);
            labels.push("a key-shaped argument".to_string());
        }
        i += 1;
    }
    (drop, labels)
}

/// Prose with its key-shaped values replaced, and how many there were.
pub(crate) fn redact(text: &str) -> (String, usize) {
    fn words(text: &str, count: &mut usize, out: &mut String) {
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String, count: &mut usize| {
            if token_shaped(word) {
                out.push_str(LEFT_OUT);
                *count += 1;
            } else {
                out.push_str(word);
            }
            word.clear();
        };
        for c in text.chars() {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                word.push(c);
            } else {
                flush(&mut word, out, count);
                out.push(c);
            }
        }
        flush(&mut word, out, count);
    }
    let (mut out, mut count, mut rest) = (String::with_capacity(text.len()), 0, text);
    while let Some(start) = rest.find("-----BEGIN") {
        let after = start + "-----BEGIN".len();
        let end = rest[after..]
            .find("-----END")
            .and_then(|e| rest[after + e + 8..].find("-----").map(|f| after + e + 8 + f + 5))
            .unwrap_or(rest.len());
        words(&rest[..start], &mut count, &mut out);
        out.push_str(LEFT_OUT);
        count += 1;
        rest = &rest[end..];
    }
    words(rest, &mut count, &mut out);
    (out, count)
}

/// The `${NAME}`s a value expects from its environment.
fn env_refs(value: &str) -> Vec<String> {
    value
        .split("${")
        .skip(1)
        .filter_map(|rest| rest.split_once('}'))
        .map(|(name, _)| name.split(":-").next().unwrap_or(name).to_string())
        .collect()
}

/// An address without its secrets: a user and password, a secret-named or
/// key-shaped query value. One with a key in its path can't be cleaned.
fn scrub_url(url: &str, left_out: &mut Vec<String>) -> Result<String, String> {
    let (rest, fragment) = url.split_once('#').map_or((url, None), |(r, f)| (r, Some(f)));
    let (base, query) = rest.split_once('?').map_or((rest, None), |(b, q)| (b, Some(q)));
    let (scheme, after) = base.split_once("://").ok_or("its address isn't a URL")?;
    let (authority, path) = after.split_once('/').map_or((after, None), |(a, p)| (a, Some(p)));
    let host = match authority.rsplit_once('@') {
        Some((_, host)) => {
            left_out.push("the user and password in its address".to_string());
            host
        }
        None => authority,
    };
    if path.is_some_and(|p| p.split('/').any(token_shaped)) {
        return Err("its address holds a key in its path; add it again once the key is in the Bakery's sign-ins".to_string());
    }
    let mut keep = |part: &str| -> String {
        part.split('&')
            .filter(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                let secret = (secret_name(name) && literal(value)) || token_shaped(value);
                if secret {
                    left_out.push(format!("{name}= in its address"));
                }
                !secret
            })
            .collect::<Vec<_>>()
            .join("&")
    };
    let mut clean = format!("{scheme}://{host}");
    if let Some(path) = path {
        clean.push('/');
        clean.push_str(path);
    }
    if let Some(query) = query.map(&mut keep).filter(|q| !q.is_empty()) {
        clean.push('?');
        clean.push_str(&query);
    }
    if let Some(fragment) = fragment.map(&mut keep).filter(|f| !f.is_empty()) {
        clean.push('#');
        clean.push_str(&fragment);
    }
    Ok(clean)
}

// ─── Instructions and rules ─────────────────────────────────────────────────

fn instructions(home: &Path, claude: &Path, out: &mut Vec<Candidate>) {
    let mut files = vec![(claude.join("CLAUDE.md"), "instructions", "CLAUDE.md".to_string())];
    let rules = claude.join("rules");
    for rel in walk(&rules).into_iter().filter(|r| r.ends_with(".md")) {
        files.push((rules.join(&rel), "rule", format!("rules/{rel}")));
    }
    for (path, what, name) in files {
        let Some(text) = crate::control_plane::read(&path) else { continue };
        if without_frontmatter(&text).trim().is_empty() {
            continue;
        }
        // A rule keeps its frontmatter: a Claude seat opens it by its paths.
        let own = if what == "rule" { text.trim() } else { without_frontmatter(&text).trim() };
        let (mut body, mut keys) = redact(own);
        let mut f = found(format!("instructions:{name}"), what, Some(Kind::Instructions), name, shown(home, &path));
        f.description = first_line(without_frontmatter(&body));
        // Its imports come with it, as Claude Code loads them beside it.
        let mut unread = Vec::new();
        for (from, imported) in imported(home, &path, &text, &mut unread) {
            let (imported, more) = redact(imported.trim());
            body.push_str(&format!("\n\n[Imported from {from}]\n{imported}"));
            keys += more;
        }
        if !unread.is_empty() {
            f.warnings.push(format!("it imports {}, which can't be read; they stay behind", unread.join(", ")));
        }
        if keys > 0 {
            f.left_out.push(format!("{keys} key-shaped value(s) in its text"));
        }
        if frontmatter_keys(&text).iter().any(|k| k == "paths") {
            f.warnings.push("a Claude seat opens it only for the files its paths match; other runtimes open it every turn".to_string());
        }
        if body.len() > toolkit::INSTRUCTIONS_CHARS {
            f.warnings.push(format!(
                "{} characters: a Claude seat gets it whole, but other runtimes' turns open with at most {} characters of instructions, so it won't fit there",
                body.len(),
                toolkit::INSTRUCTIONS_CHARS
            ));
        }
        out.push(Candidate { found: f, draft: Some(Draft::Item(Item { text: body, ..Item::default() })) });
    }
}

/// The files an instruction file imports (`@path` outside code), as Claude
/// Code loads them: from the importing file's folder, `~/` from home, four
/// hops deep, each once, with where each was. Only files in the operator's
/// home, outside the Bakery's own, never a credential; an import that
/// can't be read is named in `unread`.
fn imported(home: &Path, from: &Path, text: &str, unread: &mut Vec<String>) -> Vec<(String, String)> {
    fn go(home: &Path, from: &Path, text: &str, hop: usize, seen: &mut Vec<PathBuf>, out: &mut Vec<(String, String)>, unread: &mut Vec<String>) {
        if hop > 4 {
            return;
        }
        for token in imports_in(text) {
            let raw = token.trim_start_matches('@').replace("\\ ", " ");
            let path = match raw.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None => from.parent().unwrap_or(home).join(&raw),
            };
            let real = path.canonicalize().ok().filter(|real| real.starts_with(home) && !real.starts_with(home.join(".bakery")));
            let Some((real, content)) = real.and_then(|real| crate::control_plane::read(&real).map(|content| (real, content))) else {
                unread.push(token);
                continue;
            };
            if seen.contains(&real) {
                continue;
            }
            seen.push(real.clone());
            out.push((shown(home, &real), content.clone()));
            go(home, &real, &content, hop + 1, seen, out, unread);
        }
    }
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let mut out = Vec::new();
    go(&home, from, text, 1, &mut Vec::new(), &mut out, unread);
    out
}

/// A Claude Code instruction file's `@path` imports, outside code.
fn imports_in(text: &str) -> Vec<String> {
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for word in line.split_whitespace() {
            if let Some(path) = word.strip_prefix('@') {
                if !path.contains('`') && (path.contains('/') || path.ends_with(".md")) {
                    out.push(word.to_string());
                }
            }
        }
    }
    out
}

// ─── Skills ─────────────────────────────────────────────────────────────────

fn skills(home: &Path, claude: &Path, out: &mut Vec<Candidate>) {
    let Ok(entries) = std::fs::read_dir(claude.join("skills")) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let folder = entry.file_name().to_string_lossy().into_owned();
        // claude.ai's own skills come with the account, not the setup.
        if folder.starts_with('.') || matches!(folder.as_str(), "synced" | "anthropic-skills") {
            continue;
        }
        // A skill folder may be a link; its SKILL.md is read through it.
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join(".claude-plugin").join("plugin.json").is_file() {
            let f = found(format!("plugin:{folder}@skills-dir"), "plugin", None, format!("{folder}@skills-dir"), shown(home, &path));
            out.push(stays(f, "a plugin kept in your skills folder; only installed plugins come across yet"));
            continue;
        }
        let Some(text) = crate::control_plane::read(&path.join("SKILL.md")) else { continue };
        let (name, description) = crate::control_plane::frontmatter(&text);
        let name = name.unwrap_or_else(|| folder.clone());
        let mut f = found(format!("skill:{folder}"), "skill", Some(Kind::Skill), name.clone(), shown(home, &path.join("SKILL.md")));
        let (body, keys) = redact(without_frontmatter(&text).trim());
        f.description = description.map(|d| redact(&d).0).unwrap_or_else(|| first_line(&body));
        if keys > 0 {
            f.left_out.push(format!("{keys} key-shaped value(s) in its text"));
        }
        // The files beside it come too, each with its keys left out.
        let beside: usize = walk(&path)
            .into_iter()
            .filter(|rel| rel != "SKILL.md")
            .filter_map(|rel| crate::control_plane::read(&path.join(rel)))
            .map(|text| redact(&text).1)
            .sum();
        if beside > 0 {
            f.left_out.push(format!("{beside} key-shaped value(s) in the files beside it"));
        }
        let claude_only: Vec<String> =
            frontmatter_keys(&text).into_iter().filter(|k| CLAUDE_ONLY_SKILL_KEYS.contains(&k.as_str())).collect();
        if !claude_only.is_empty() {
            f.warnings.push(format!("Claude Code settings only a Claude seat acts on: {}", claude_only.join(", ")));
        }
        if PLACEHOLDERS.iter().any(|p| body.contains(p)) {
            f.warnings.push("it uses Claude Code placeholders ($ARGUMENTS, ${CLAUDE_SKILL_DIR}, !`command`) that other runtimes read as plain text".to_string());
        }
        if !toolkit::callable(&name) {
            out.push(stays(f, format!("{name:?} can't be a name agents call: letters, digits, - and _ only")));
            continue;
        }
        let item = Item { description: f.description.clone(), text: body, folder: Some(path), ..Item::default() };
        out.push(Candidate { found: f, draft: Some(Draft::Item(item)) });
    }
}

// ─── MCP servers ────────────────────────────────────────────────────────────

/// What `~/.claude.json` says of MCP servers: the rest of it (the account
/// it's signed in to among it) is skipped unread.
#[derive(Deserialize, Default)]
struct ClaudeJson {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: serde_json::Map<String, Value>,
    #[serde(default)]
    projects: BTreeMap<String, ProjectJson>,
}

#[derive(Deserialize, Default)]
struct ProjectJson {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: serde_json::Map<String, Value>,
}

fn mcp_servers(home: &Path, out: &mut Vec<Candidate>) {
    let path = home.join(".claude.json");
    let small = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= MAX_CLAUDE_JSON_BYTES);
    let config: ClaudeJson = small
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let source = shown(home, &path);
    for (name, server) in &config.mcp_servers {
        out.push(mcp_server(home, name, server, format!("mcp:{name}"), source.clone()));
    }
    for (project, entry) in &config.projects {
        for (name, server) in &entry.mcp_servers {
            out.push(mcp_server(home, name, server, format!("mcp:{name}:{project}"), format!("{source}, for {project}")));
        }
    }
}

fn mcp_server(home: &Path, name: &str, server: &Value, key: String, source: String) -> Candidate {
    let mut f = found(key, "mcp_server", Some(Kind::Mcp), name.to_string(), source);
    let names = |k: &str| server.get(k).and_then(Value::as_object).map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    f.left_out.extend(names("env").into_iter().map(|n| format!("env {n}")));
    f.left_out.extend(names("headers").into_iter().map(|n| format!("header {n}")));
    if server.get("headersHelper").is_some() {
        f.left_out.push("headersHelper, the command that makes its headers".to_string());
    }
    if server.get("oauth").is_some() {
        f.left_out.push("its sign-in (oauth) settings".to_string());
    }
    let text = |k: &str| server.get(k).and_then(Value::as_str).map(str::to_string);
    let transport = text("type").unwrap_or_else(|| if server.get("url").is_some() { "http" } else { "stdio" }.to_string());
    let mut item = Item::default();
    match transport.as_str() {
        "stdio" => {
            let Some(command) = text("command") else { return stays(f, "it has no command") };
            if command.split_whitespace().any(token_shaped) {
                return stays(f, "its command holds a secret-looking value");
            }
            let args: Vec<String> =
                server["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect();
            let (drop, labels) = secret_words(&args);
            f.left_out.extend(labels);
            item.args = args.into_iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, a)| a).collect();
            if command.starts_with('~') || Path::new(&command).starts_with(home) {
                f.warnings.push("it runs a program from your home folder, which a room may not be allowed to run".to_string());
            }
            f.description = format!("{command} {}", item.args.join(" ")).trim().chars().take(200).collect();
            item.command = Some(command);
        }
        "http" | "streamable-http" | "sse" | "ws" => {
            let Some(url) = text("url") else { return stays(f, "it has no address") };
            match scrub_url(&url, &mut f.left_out) {
                Ok(clean) => item.url = Some(clean),
                Err(reason) => return stays(f, reason),
            }
            if matches!(transport.as_str(), "sse" | "ws") {
                f.warnings.push(format!("it speaks {transport}; the Toolkit hands an address over as http"));
            }
            f.description = item.url.clone().unwrap_or_default();
        }
        other => return stays(f, format!("a {other} server; the Toolkit runs a command or an address")),
    }
    let expected: Vec<String> = item.args.iter().chain(item.url.iter()).flat_map(|v| env_refs(v)).collect();
    if !expected.is_empty() {
        f.warnings.push(format!("it expects {} from its environment, which a runtime won't have", expected.join(", ")));
    }
    if !toolkit::callable(name) {
        return stays(f, format!("{name:?} can't be a name agents call: letters, digits, - and _ only"));
    }
    Candidate { found: f, draft: Some(Draft::Item(item)) }
}

// ─── Hooks ──────────────────────────────────────────────────────────────────

fn hooks(home: &Path, settings: &Value, out: &mut Vec<Candidate>) {
    for (event, groups) in settings["hooks"].as_object().into_iter().flatten() {
        for group in groups.as_array().into_iter().flatten() {
            let matcher = group["matcher"].as_str().filter(|m| !m.is_empty() && *m != "*");
            for handler in group["hooks"].as_array().into_iter().flatten() {
                out.push(hook(home, event, matcher, handler));
            }
        }
    }
}

fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c)) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn hook(home: &Path, event: &str, matcher: Option<&str>, handler: &Value) -> Candidate {
    let command = handler["command"]
        .as_str()
        .map(str::to_string)
        .or_else(|| handler["args"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(shell_quote).collect::<Vec<_>>().join(" ")))
        .unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(format!("{event}\n{}\n{handler}", matcher.unwrap_or("*"))));
    let words: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    let (secret, _) = secret_words(&words);
    let shown_command: String = words
        .iter()
        .enumerate()
        .map(|(i, w)| if secret.contains(&i) { LEFT_OUT } else { w.as_str() })
        .collect::<Vec<_>>()
        .join(" ");
    let short: String = shown_command.chars().take(48).collect();
    let name = format!("{event} {}: {short}", matcher.unwrap_or("*"));
    let mut f = found(format!("hook:{}", &digest[..12]), "hook", Some(Kind::Hook), name, "~/.claude/settings.json".to_string());
    f.description = shown_command;
    let event_name = match event {
        "PreToolUse" => "pre_tool",
        "PostToolUse" => "post_tool",
        _ => return stays(f, format!("{event} hooks have no Toolkit event yet; the Toolkit fires before and after a tool")),
    };
    let handler_type = handler["type"].as_str().unwrap_or("command");
    if handler_type != "command" {
        return stays(f, format!("a {handler_type} hook; the Toolkit runs commands"));
    }
    if command.trim().is_empty() {
        return stays(f, "it has no command");
    }
    if let Some(only) = handler["if"].as_str() {
        return stays(
            f,
            format!("Claude Code runs it only for {only}; the Toolkit can't narrow a hook that way yet, and without that it would fire on every matching tool"),
        );
    }
    if handler["shell"].as_str() == Some("powershell") {
        return stays(f, "a PowerShell hook");
    }
    if !secret.is_empty() {
        return stays(f, "its command holds a secret-looking value; write it again without the key");
    }
    if let Some(timeout) = handler["timeout"].as_u64().filter(|t| *t != 600) {
        f.warnings.push(format!("its {timeout} s timeout becomes the Toolkit's 600 s"));
    }
    if handler["async"].as_bool() == Some(true) || handler["asyncRewake"].as_bool() == Some(true) {
        f.warnings.push("Claude Code runs it in the background; the Toolkit waits for it".to_string());
    }
    if handler["onFailure"].as_str() == Some("block") {
        f.warnings.push("Claude Code stops the tool when it fails; the Toolkit lets the tool run".to_string());
    }
    let home_claude = home.join(".claude").display().to_string();
    if ["~/.claude", "$HOME/.claude", "${HOME}/.claude", home_claude.as_str()].iter().any(|p| command.contains(p)) {
        f.warnings.push("it runs a file in ~/.claude, which a room can't read; that file isn't carried".to_string());
    }
    if command.contains("CLAUDE_PROJECT_DIR") {
        f.warnings.push("it uses $CLAUDE_PROJECT_DIR, which only Claude-family runtimes set".to_string());
    }
    if matcher.is_some_and(|m| !m.chars().all(|c| c.is_ascii_alphanumeric() || "_-|, ".contains(c))) {
        f.warnings.push("its matcher is a pattern: native runtimes read it as one; the Bakery's own loop matches exact tool names".to_string());
    }
    let item = Item { text: command, event: Some(event_name.to_string()), matcher: matcher.map(str::to_string), ..Item::default() };
    Candidate { found: f, draft: Some(Draft::Item(item)) }
}

// ─── Plugins ────────────────────────────────────────────────────────────────

use toolkit::plain;

fn plugins(home: &Path, claude: &Path, settings: &Value, out: &mut Vec<Candidate>) {
    let root = claude.join("plugins");
    let installed = json_at(&root.join("installed_plugins.json"));
    let known = json_at(&root.join("known_marketplaces.json"));
    let root = root.canonicalize().unwrap_or(root);
    for (id, installs) in installed["plugins"].as_object().into_iter().flatten() {
        // Version 2 keeps a list of installs per plugin; version 1, one.
        let installs: Vec<&Value> = match installs {
            Value::Array(list) => list.iter().collect(),
            one @ Value::Object(_) => vec![one],
            _ => continue,
        };
        for install in installs {
            out.push(plugin(home, &root, &known, settings, id, install));
        }
    }
    for (id, on) in settings["enabledPlugins"].as_object().into_iter().flatten() {
        if on.as_bool() == Some(true) && installed["plugins"].get(id).is_none() {
            let f = found(format!("plugin:{id}"), "plugin", None, id.clone(), "~/.claude/settings.json".to_string());
            out.push(stays(f, "on in your settings with no install record (a local or claude.ai plugin); only installed plugins come across yet"));
        }
    }
}

/// A plugin source without its secrets: download headers, a user and
/// password in its address.
fn scrub_source(source: &mut Value, left_out: &mut Vec<String>) -> Result<(), String> {
    let Some(fields) = source.as_object_mut() else { return Ok(()) };
    for secret in ["headers", "headersHelper"] {
        if fields.remove(secret).is_some() {
            left_out.push(format!("its download {secret}"));
        }
    }
    if let Some(url) = fields.get("url").and_then(Value::as_str).map(str::to_string) {
        fields.insert("url".to_string(), Value::String(scrub_url(&url, left_out)?));
    }
    Ok(())
}

fn plugin(home: &Path, root: &Path, known: &Value, settings: &Value, id: &str, install: &Value) -> Candidate {
    let project = install["projectPath"].as_str();
    let key = match project {
        Some(p) => format!("plugin:{id}:{p}"),
        None => format!("plugin:{id}"),
    };
    let mut f = found(key, "plugin", Some(Kind::Plugin), id.to_string(), shown(home, &root.join("installed_plugins.json")));
    if let Some(p) = project {
        f.warnings.push(format!("installed for {p} in Claude Code"));
    }
    if settings["enabledPlugins"][id].as_bool() == Some(false) {
        f.warnings.push("off in your Claude Code settings".to_string());
    }
    if settings["pluginConfigs"].get(id).is_some() {
        f.left_out.push("its options (pluginConfigs); set them again".to_string());
    }
    let Some((name, market)) = id.rsplit_once('@').filter(|(n, m)| plain(n) && plain(m)) else {
        return stays(f, "not a <name>@<marketplace> plugin id");
    };
    if install.get("sourceCommand").is_some() || install.get("sourceProducerPath").is_some() {
        return stays(f, "installed by running a command; it doesn't come across yet");
    }
    let dir = install["installPath"].as_str().and_then(|p| Path::new(p).canonicalize().ok());
    let Some(dir) = dir else { return stays(f, "its folder is gone; run /plugin in Claude Code to refresh it") };
    if !dir.starts_with(root.join("cache")) {
        return stays(f, "installed outside ~/.claude/plugins/cache");
    }
    let marketplace = &known[market];
    let source_type = marketplace["source"]["source"].as_str().unwrap_or("unknown");
    if !matches!(source_type, "github" | "git") {
        return stays(f, format!("its marketplace is a {source_type} source; only GitHub and git marketplaces come across yet"));
    }
    let catalog = marketplace["installLocation"]
        .as_str()
        .and_then(|l| Path::new(l).canonicalize().ok())
        .filter(|l| l.starts_with(root.join("marketplaces")))
        .map(|l| json_at(&l.join(".claude-plugin").join("marketplace.json")))
        .unwrap_or(Value::Null);
    let entry = catalog["plugins"].as_array().and_then(|list| list.iter().find(|p| p["name"] == name)).cloned();
    let Some(mut entry) = entry else {
        return stays(f, "its marketplace's catalog isn't on disk; run /plugin in Claude Code to refresh it");
    };
    if entry["source"]["source"].as_str() == Some("command") {
        return stays(f, "installed by running a command; it doesn't come across yet");
    }
    let mut marketplace_source = marketplace["source"].clone();
    for source in [&mut marketplace_source, &mut entry["source"]] {
        if let Err(reason) = scrub_source(source, &mut f.left_out) {
            return stays(f, reason);
        }
    }
    let manifest = json_at(&dir.join(".claude-plugin").join("plugin.json"));
    f.description = manifest["description"].as_str().or(entry["description"].as_str()).map(|d| redact(d).0).unwrap_or_default();
    let version = install["version"].as_str().or(manifest["version"].as_str()).filter(|v| plain(v)).unwrap_or("unknown");
    if manifest["userConfig"].as_object().is_some_and(|o| !o.is_empty()) {
        f.warnings.push("it asks for settings of its own (userConfig); they don't come across, set them again".to_string());
    }
    if ["skills", "commands", "agents"].iter().any(|k| manifest[*k].is_string() || manifest[*k].is_array()) {
        f.warnings.push("it names its own component folders; its parts are listed from the usual ones".to_string());
    }
    let parts = parts_of(&dir, &manifest);
    f.parts = Some(parts.clone());
    let record = PluginRecord { id: id.to_string(), version: version.to_string(), marketplace_source, entry, parts };
    Candidate { found: f, draft: Some(Draft::Plugin { record, root: dir }) }
}

/// What a plugin holds, from Claude Code's usual places in it.
fn parts_of(dir: &Path, manifest: &Value) -> PluginParts {
    let named = |sub: &str, by_frontmatter: bool| -> Vec<String> {
        walk(&dir.join(sub))
            .into_iter()
            .filter_map(|rel| {
                let stem = rel.strip_suffix(".md")?.replace('/', ":");
                let name = by_frontmatter
                    .then(|| crate::control_plane::read(&dir.join(sub).join(&rel)))
                    .flatten()
                    .and_then(|t| crate::control_plane::frontmatter(&t).0);
                Some(name.unwrap_or(stem))
            })
            .collect()
    };
    let skills = walk(&dir.join("skills"))
        .into_iter()
        .filter_map(|rel| rel.strip_suffix("/SKILL.md").map(str::to_string))
        .map(|folder| {
            crate::control_plane::read(&dir.join("skills").join(&folder).join("SKILL.md"))
                .and_then(|t| crate::control_plane::frontmatter(&t).0)
                .unwrap_or(folder)
        })
        .collect();
    let keys = |v: &Value| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    let mut hooks = keys(&json_at(&dir.join("hooks").join("hooks.json"))["hooks"]);
    hooks.extend(keys(&manifest["hooks"]["hooks"]));
    hooks.sort();
    hooks.dedup();
    let mut mcp_servers = keys(&json_at(&dir.join(".mcp.json"))["mcpServers"]);
    mcp_servers.extend(keys(&manifest["mcpServers"]));
    mcp_servers.sort();
    mcp_servers.dedup();
    PluginParts { skills, commands: named("commands", false), agents: named("agents", true), hooks, mcp_servers }
}

/// A plugin's files, copied: a link only when it stays inside the plugin,
/// never a credential file.
/// A folder's files (`walk`'s: never a link or a credential file) into
/// `dst`, each text file with its keys left out.
fn copy_redacted(src: &Path, dst: &Path) -> std::io::Result<()> {
    for rel in walk(src) {
        let to = dst.join(&rel);
        std::fs::create_dir_all(to.parent().unwrap_or(dst))?;
        match String::from_utf8(std::fs::read(src.join(&rel))?) {
            Ok(text) => std::fs::write(&to, redact(&text).0)?,
            Err(binary) => std::fs::write(&to, binary.into_bytes())?,
        }
        // A skill's scripts keep their mode, so the ones it runs still run.
        std::fs::set_permissions(&to, std::fs::metadata(src.join(&rel))?.permissions())?;
    }
    Ok(())
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fn go(root: &Path, src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let (from, to) = (entry.path(), dst.join(entry.file_name()));
            let kind = entry.file_type()?;
            if kind.is_dir() {
                go(root, &from, &to)?;
            } else if kind.is_file() {
                if !bakery_exec::is_hidden_credential(&from) {
                    std::fs::copy(&from, &to)?;
                }
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&from)?;
                let inside = target.is_relative()
                    && from.parent().and_then(|p| p.join(&target).canonicalize().ok()).is_some_and(|t| t.starts_with(root));
                if inside {
                    std::os::unix::fs::symlink(&target, &to)?;
                }
            }
        }
        Ok(())
    }
    let root = src.canonicalize()?;
    go(&root, &root, dst)
}

// ─── Slash commands, subagents and output styles ────────────────────────────

/// Claude Code's own files, carried whole into Claude seats (the operator's
/// answer): a slash command is named by its path, a subagent by its
/// frontmatter, an output style by its frontmatter or file.
fn claude_files(home: &Path, claude: &Path, out: &mut Vec<Candidate>) {
    for (dir, what, kind) in [("commands", "command", Kind::Command), ("agents", "agent", Kind::Agent), ("output-styles", "output_style", Kind::OutputStyle)] {
        for rel in walk(&claude.join(dir)).into_iter().filter(|r| r.ends_with(".md")) {
            let path = claude.join(dir).join(&rel);
            let Some(text) = crate::control_plane::read(&path) else { continue };
            let (named, description) = crate::control_plane::frontmatter(&text);
            let stem = rel.trim_end_matches(".md").replace('/', ":");
            let name = match kind {
                Kind::Command => stem.clone(),
                _ => named.unwrap_or_else(|| stem.clone()),
            };
            let mut f = found(format!("{what}:{stem}"), what, Some(kind), name.clone(), shown(home, &path));
            let (text, keys) = redact(text.trim());
            f.description = description.map(|d| redact(&d).0).unwrap_or_else(|| first_line(without_frontmatter(&text)));
            if keys > 0 {
                f.left_out.push(format!("{keys} key-shaped value(s) in its text"));
            }
            // A subagent's own MCP servers can carry their keys in its frontmatter.
            let own_keys = |line: &str| matches!(line.trim_start().split(':').next(), Some("env" | "headers" | "headersHelper"));
            if kind == Kind::Agent && text.lines().skip(1).take_while(|l| l.trim() != "---").any(own_keys) {
                out.push(stays(f, "its frontmatter gives an MCP server env or headers, which can hold keys; write it again without them"));
                continue;
            }
            let fits = match kind {
                Kind::Command => name.split(':').all(toolkit::plain),
                Kind::OutputStyle => toolkit::plain(&name),
                _ => !name.is_empty(),
            };
            if !fits {
                out.push(stays(f, format!("{name:?} can't be a file name in a seat: letters, digits, -, _ and . only")));
                continue;
            }
            let item = Item { description: f.description.clone(), text, ..Item::default() };
            out.push(Candidate { found: f, draft: Some(Draft::Item(item)) });
        }
    }
}

// ─── The list and the import ────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PortQuery {
    /// The scope it would come into: that project's, or Bakery-wide when
    /// absent.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
}

fn in_toolkit<'a>(items: &'a [ToolkitItem], kind: Kind, name: &str, project_id: Option<&str>) -> Option<&'a ToolkitItem> {
    items.iter().find(|i| i.kind == kind && i.name == name && i.project_id.as_deref() == project_id)
}

/// What the operator's Claude Code setup holds, read now.
pub(crate) fn list(state: &AppState, project_id: Option<&str>) -> Port {
    let items = toolkit::items(state);
    let found = discover(&operator_home(state))
        .into_iter()
        .map(|c| {
            let mut f = c.found;
            f.in_toolkit = f.kind.and_then(|k| in_toolkit(&items, k, &f.name, project_id)).map(|i| i.id.clone());
            f
        })
        .collect();
    Port { found }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ImportReq {
    /// What to bring, by the keys the list gave.
    pub(crate) keys: Vec<String>,
    /// Into that project; Bakery-wide when absent.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Imported {
    pub(crate) key: String,
    /// The Toolkit item it became, or changed.
    pub(crate) id: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ImportResult {
    pub(crate) imported: Vec<Imported>,
}

/// Brings the ticked items in, each on its own: one already in the Toolkit
/// in that scope is changed.
pub(crate) fn import(state: &AppState, req: &ImportReq) -> ImportResult {
    let mut candidates = discover(&operator_home(state));
    let imported = req
        .keys
        .iter()
        .map(|key| {
            let result = candidates
                .iter_mut()
                .find(|c| &c.found.key == key)
                .ok_or_else(|| "it isn't in your Claude Code setup any more".to_string())
                .and_then(|c| bring(state, c, req.project_id.as_deref()));
            match result {
                Ok(id) => Imported { key: key.clone(), id: Some(id), error: None },
                Err(e) => Imported { key: key.clone(), id: None, error: Some(e) },
            }
        })
        .collect();
    ImportResult { imported }
}

fn bring(state: &AppState, candidate: &mut Candidate, project_id: Option<&str>) -> Result<String, String> {
    let f = &candidate.found;
    let (Some(kind), Some(draft)) = (f.kind, candidate.draft.take()) else {
        return Err(f.reason.clone().unwrap_or_else(|| "it can't come across yet".to_string()));
    };
    let had = in_toolkit(&toolkit::items(state), kind, &f.name, project_id).map(|i| i.id.clone());
    let mut req = SetReq {
        id: had.clone(),
        kind,
        name: f.name.clone(),
        description: String::new(),
        project_id: project_id.map(str::to_string),
        enabled: None,
        text: String::new(),
        command: None,
        args: Vec::new(),
        url: None,
        event: None,
        matcher: None,
        params: None,
    };
    match draft {
        Draft::Item(item) => {
            req.description = item.description;
            req.text = item.text;
            req.command = item.command;
            req.args = item.args;
            req.url = item.url;
            req.event = item.event;
            req.matcher = item.matcher;
            let id = toolkit::set(state, &req)?.item.id;
            if let Some(folder) = item.folder {
                let store = toolkit::skill_dir(state, &id);
                let _ = std::fs::remove_dir_all(&store);
                copy_redacted(&folder, &store).map_err(|e| format!("its files didn't copy: {e}"))?;
            }
            Ok(id)
        }
        Draft::Plugin { record, root } => {
            req.description = f.description.clone();
            let id = toolkit::set_with(state, &req, Some(record))?.item.id;
            let dir = toolkit::plugin_dir(state, &id);
            let _ = std::fs::remove_dir_all(&dir);
            if let Err(e) = copy_tree(&root, &dir) {
                if had.is_none() {
                    let _ = toolkit::remove(state, &id);
                }
                return Err(format!("its files didn't copy: {e}"));
            }
            Ok(id)
        }
    }
}

pub(crate) async fn list_route(State(st): State<AppState>, Query(q): Query<PortQuery>) -> Result<Json<Port>, (StatusCode, String)> {
    toolkit::operator_only()?;
    toolkit::blocking(move || Ok(list(&st, q.project_id.as_deref()))).await
}

pub(crate) async fn import_route(State(st): State<AppState>, Json(req): Json<ImportReq>) -> Result<Json<ImportResult>, (StatusCode, String)> {
    toolkit::operator_only()?;
    toolkit::blocking(move || Ok(import(&st, &req))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;
    use std::sync::Arc;

    fn put(home: &Path, rel: &str, text: &str) {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn setup() -> (tempfile::TempDir, AppState, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let home = operator_home(&state);
        (dir, state, home)
    }

    fn by_key<'a>(port: &'a Port, key: &str) -> &'a Found {
        port.found.iter().find(|f| f.key == key).unwrap_or_else(|| panic!("no {key} in {:#?}", port.found))
    }

    /// Every secret in the fixture, none of which may leave the setup.
    const SECRETS: &[&str] = &[
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "hunter2hunter2",
        "sk-proj-abcdefghijklmnopqrstuvwxyz",
        "s3cr3t-query-value",
        "bearer-value-123",
        "env-value-xyz",
        "header-value-xyz",
        "pa55word",
    ];

    #[test]
    fn keys_and_secrets_stay_behind() {
        let (_dir, state, home) = setup();
        put(
            &home,
            ".claude.json",
            r#"{
              "oauthAccount": {"emailAddress": "op@example.com"},
              "primaryApiKey": "sk-proj-abcdefghijklmnopqrstuvwxyz",
              "mcpServers": {
                "github": {"command": "npx", "args": ["-y", "@example/github-mcp", "--api-key", "hunter2hunter2", "--token=s3cr3t-query-value", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"],
                           "env": {"GITHUB_TOKEN": "env-value-xyz", "LOG_LEVEL": "debug"}},
                "notes": {"type": "http", "url": "https://user:pa55word@mcp.example.com/mcp?api_key=s3cr3t-query-value&region=eu",
                          "headers": {"Authorization": "Bearer header-value-xyz"}},
                "zap": {"type": "sse", "url": "https://mcp.example.com/ghp_abcdefghijklmnopqrstuvwxyz0123456789/sse"}
              },
              "projects": {"/Users/op/app": {"history": [{"display": "hunter2hunter2"}], "mcpServers": {"fetch": {"command": "uvx", "args": ["mcp-server-fetch"]}}}}
            }"#,
        );
        put(
            &home,
            ".claude/settings.json",
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "curl -H 'Authorization: Bearer bearer-value-123' https://audit.example.com"}]}]}}"#,
        );
        put(&home, ".claude/skills/deploy/SKILL.md", "---\nname: deploy\ndescription: Ship it\n---\nUse GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789 to push.\n");
        let port = list(&state, None);
        let listed = serde_json::to_string(&port).unwrap();
        for secret in SECRETS {
            assert!(!listed.contains(secret), "{secret} leaked into the list");
        }

        let github = by_key(&port, "mcp:github");
        assert_eq!(github.kind, Some(Kind::Mcp));
        assert!(github.reason.is_none());
        for name in ["env GITHUB_TOKEN", "env LOG_LEVEL", "argument --api-key and its value", "argument --token=", "a key-shaped argument"] {
            assert!(github.left_out.iter().any(|l| l == name), "{name} not named in {:?}", github.left_out);
        }
        let notes = by_key(&port, "mcp:notes");
        assert!(notes.left_out.iter().any(|l| l == "header Authorization"));
        assert!(notes.left_out.iter().any(|l| l.contains("user and password")));
        assert_eq!(notes.description, "https://mcp.example.com/mcp?region=eu");
        assert!(by_key(&port, "mcp:zap").reason.as_deref().is_some_and(|r| r.contains("key in its path")));
        assert_eq!(by_key(&port, "mcp:fetch:/Users/op/app").kind, Some(Kind::Mcp));
        let hook = port.found.iter().find(|f| f.found == "hook").unwrap();
        assert!(hook.reason.as_deref().is_some_and(|r| r.contains("secret-looking")));

        let result = import(
            &state,
            &ImportReq { keys: vec!["mcp:github".into(), "mcp:notes".into(), "skill:deploy".into(), hook.key.clone()], project_id: None },
        );
        assert!(result.imported[3].error.is_some(), "the hook with a key stays behind");
        let stored = serde_json::to_string(&toolkit::items(&state)).unwrap();
        for secret in SECRETS {
            assert!(!stored.contains(secret), "{secret} reached the Toolkit");
        }
        let items = toolkit::items(&state);
        let github = items.iter().find(|i| i.name == "github").unwrap();
        assert_eq!(github.command.as_deref(), Some("npx"));
        assert_eq!(github.args, vec!["-y", "@example/github-mcp"]);
        let skill = items.iter().find(|i| i.name == "deploy").unwrap();
        assert!(skill.text.contains(LEFT_OUT) && skill.description == "Ship it");
    }

    #[test]
    fn skills_instructions_and_hooks_come_across_and_a_second_port_changes_them() {
        let (_dir, state, home) = setup();
        put(&home, ".claude/CLAUDE.md", "# House rules\nAlways run tests.\nSee @~/notes/style.md and @guide.md\n");
        put(&home, ".claude/guide.md", "Prefer small diffs.\n");
        put(&home, ".claude/rules/rust.md", "---\npaths:\n  - \"**/*.rs\"\n---\nUse anyhow.\n");
        put(&home, ".claude/skills/pdf/SKILL.md", "---\nname: pdf\ndescription: Read PDFs\nallowed-tools: Read Grep\n---\nRun ${CLAUDE_SKILL_DIR}/scripts/x.py\n");
        put(&home, ".claude/skills/pdf/scripts/x.py", "print(1)\n");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.join(".claude/skills/pdf/scripts/x.py"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        put(&home, ".claude/skills/linked/.claude-plugin/plugin.json", r#"{"name": "linked"}"#);
        put(&home, ".claude/commands/review.md", "Review the diff.\n");
        put(&home, ".claude/agents/helper.md", "---\nname: helper\ndescription: Helps\n---\nYou help.\n");
        put(
            &home,
            ".claude/settings.json",
            r#"{"hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "~/.claude/hooks/guard.sh", "timeout": 30}]},
                               {"matcher": "Bash", "hooks": [{"type": "command", "command": "deny.sh", "if": "Bash(rm *)"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]
            }}"#,
        );
        let port = list(&state, None);
        let claude_md = by_key(&port, "instructions:CLAUDE.md");
        assert!(claude_md.warnings.iter().any(|w| w.contains("@~/notes/style.md")));
        assert!(by_key(&port, "instructions:rules/rust.md").warnings.iter().any(|w| w.contains("paths")));
        let pdf = by_key(&port, "skill:pdf");
        assert!(pdf.warnings.iter().any(|w| w.contains("allowed-tools")));
        assert!(by_key(&port, "plugin:linked@skills-dir").reason.is_some());
        assert_eq!(by_key(&port, "command:review").kind, Some(Kind::Command));
        assert_eq!(by_key(&port, "agent:helper").name, "helper");
        let hooks: Vec<&Found> = port.found.iter().filter(|f| f.found == "hook").collect();
        let guard = hooks.iter().find(|h| h.description.contains("guard.sh")).unwrap();
        assert!(guard.reason.is_none());
        assert!(guard.warnings.iter().any(|w| w.contains("30 s")) && guard.warnings.iter().any(|w| w.contains("~/.claude")));
        assert!(hooks.iter().find(|h| h.description == "deny.sh").unwrap().reason.as_deref().unwrap().contains("Bash(rm *)"));
        assert!(hooks.iter().find(|h| h.name.starts_with("Stop")).unwrap().reason.is_some());

        let keys = vec!["instructions:CLAUDE.md".to_string(), "skill:pdf".to_string(), guard.key.clone(), "instructions:rules/rust.md".to_string()];
        let first = import(&state, &ImportReq { keys: keys.clone(), project_id: None });
        assert!(first.imported.iter().all(|i| i.error.is_none()), "{:?}", first.imported);
        let hook = toolkit::items(&state).into_iter().find(|i| i.kind == Kind::Hook).unwrap();
        assert_eq!((hook.event.as_deref(), hook.matcher.as_deref(), hook.text.as_str()), (Some("pre_tool"), Some("Bash"), "~/.claude/hooks/guard.sh"));

        // A Claude seat gets each as Claude Code reads it: CLAUDE.md with what
        // it imports, the rule with its paths, the skill's whole folder.
        let seat = _dir.path().join("seat-config");
        crate::claude_native::install_toolkit_files(&state, "s:any", &seat).unwrap();
        let memory = std::fs::read_to_string(seat.join("CLAUDE.md")).unwrap();
        assert!(memory.contains("Always run tests.") && memory.contains("Prefer small diffs."), "{memory}");
        let rule = std::fs::read_to_string(seat.join("rules/rust.md")).unwrap();
        assert!(rule.starts_with("---\npaths:") && rule.contains("Use anyhow."), "{rule}");
        let skill = std::fs::read_to_string(seat.join("skills/pdf/SKILL.md")).unwrap();
        assert!(skill.starts_with("---\nname: pdf\ndescription: Read PDFs\nallowed-tools: Read Grep\n---"), "{skill}");
        assert!(skill.contains("Run ${CLAUDE_SKILL_DIR}/scripts/x.py"), "{skill}");
        assert_eq!(std::fs::read_to_string(seat.join("skills/pdf/scripts/x.py")).unwrap(), "print(1)\n");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(seat.join("skills/pdf/scripts/x.py")).unwrap().permissions().mode() & 0o111, 0o111, "a script still runs");

        put(&home, ".claude/skills/pdf/SKILL.md", "---\nname: pdf\ndescription: Read PDFs, better\n---\nNew text.\n");
        assert_eq!(list(&state, None).found.iter().find(|f| f.key == "skill:pdf").unwrap().in_toolkit.as_deref(), first.imported[1].id.as_deref());
        let second = import(&state, &ImportReq { keys, project_id: None });
        assert_eq!(second.imported[1].id, first.imported[1].id, "the same item, changed");
        let items = toolkit::items(&state);
        assert_eq!(items.len(), 4);
        assert_eq!(items.iter().find(|i| i.name == "pdf").unwrap().text, "New text.");
    }

    /// The operator's answer: standalone slash commands, subagents and output
    /// styles go whole into Claude seats, beside the vault's own; turning one
    /// off takes it out of the seat at its next spawn.
    #[test]
    fn slash_commands_subagents_and_output_styles_go_whole_into_claude_seats() {
        let (dir, state, home) = setup();
        let command = "---\ndescription: Make a component\n---\nMake $ARGUMENTS.";
        put(&home, ".claude/commands/frontend/component.md", &format!("{command}\n"));
        put(&home, ".claude/agents/helper.md", "---\nname: helper\ndescription: Helps\n---\nYou help.\n");
        put(&home, ".claude/agents/keyed.md", "---\nname: keyed\nmcpServers:\n  - name: gh\n    env:\n      GITHUB_TOKEN: abc\n---\nUse gh.\n");
        put(&home, ".claude/output-styles/terse.md", "---\nname: terse\ndescription: Short answers\n---\nBe brief.\n");
        let port = list(&state, None);
        assert_eq!(by_key(&port, "command:frontend:component").name, "frontend:component");
        assert!(by_key(&port, "agent:keyed").reason.as_deref().unwrap().contains("env or headers"));
        let keys = ["command:frontend:component", "agent:helper", "output_style:terse"].map(String::from).to_vec();
        let result = import(&state, &ImportReq { keys, project_id: None });
        assert!(result.imported.iter().all(|i| i.error.is_none()), "{:?}", result.imported);

        let vault = crate::claude_native::credential_dir(&state);
        std::fs::create_dir_all(vault.join("commands")).unwrap();
        std::fs::write(vault.join("commands/vault-only.md"), "From the vault.\n").unwrap();
        let seat = dir.path().join("seat-config");
        crate::claude_native::install_toolkit_files(&state, "s:any", &seat).unwrap();
        assert_eq!(std::fs::read_to_string(seat.join("commands/frontend/component.md")).unwrap(), format!("{command}\n"));
        assert!(std::fs::read_to_string(seat.join("agents/helper.md")).unwrap().contains("You help."));
        assert!(seat.join("output-styles/terse.md").is_file());
        assert!(seat.join("commands/vault-only.md").is_file());

        let id = result.imported[0].id.clone().unwrap();
        toolkit::enable(&state, &toolkit::EnableReq { id, enabled: false }).unwrap();
        crate::claude_native::install_toolkit_files(&state, "s:any", &seat).unwrap();
        assert!(!seat.join("commands/frontend/component.md").exists(), "turned off, gone from the seat");
        assert!(seat.join("commands/vault-only.md").is_file());
    }

    /// The operator keeps control of a ported plugin: off leaves the seat
    /// without it, on brings it back, and porting again after Claude Code
    /// updated it brings the newer version to the same Toolkit item.
    #[test]
    fn a_ported_plugin_goes_off_on_and_to_a_newer_version_only_when_the_operator_says() {
        let (dir, state, home) = setup();
        installed_plugin(&home);
        let key = vec!["plugin:formatter@example-mkt".to_string()];
        let id = import(&state, &ImportReq { keys: key.clone(), project_id: None }).imported[0].id.clone().unwrap();
        let seat = dir.path().join("seat-config");
        let install = || crate::claude_native::install_toolkit_plugins(&state, "s:any", &seat, &seat).unwrap();

        toolkit::enable(&state, &toolkit::EnableReq { id: id.clone(), enabled: false }).unwrap();
        assert!(install().is_empty(), "off: not in the seat");
        toolkit::enable(&state, &toolkit::EnableReq { id: id.clone(), enabled: true }).unwrap();
        assert_eq!(install(), vec!["formatter@example-mkt"]);

        // Claude Code updates it on the operator's Mac; the seat keeps 1.2.0
        // until the operator ports it again.
        let root = home.join(".claude/plugins");
        put(&home, ".claude/plugins/cache/example-mkt/formatter/1.3.0/.claude-plugin/plugin.json", r#"{"name": "formatter", "version": "1.3.0"}"#);
        put(&home, ".claude/plugins/cache/example-mkt/formatter/1.3.0/skills/fmt2/SKILL.md", "---\nname: fmt2\n---\nNewer.\n");
        put(&home, ".claude/plugins/installed_plugins.json", &json!({"version": 2, "plugins": {
            "formatter@example-mkt": [{"scope": "user", "installPath": root.join("cache/example-mkt/formatter/1.3.0"), "version": "1.3.0"}]
        }}).to_string());
        install();
        assert!(seat.join("plugins/cache/example-mkt/formatter/1.2.0").is_dir());
        let again = import(&state, &ImportReq { keys: key, project_id: None });
        assert_eq!(again.imported[0].id.as_deref(), Some(id.as_str()), "the same item, changed");
        let item = toolkit::items(&state).into_iter().find(|i| i.id == id).unwrap();
        assert_eq!(item.plugin.as_ref().unwrap().version, "1.3.0");
        assert!(item.plugin.unwrap().parts.skills.contains(&"fmt2".to_string()));
        install();
        assert!(seat.join("plugins/cache/example-mkt/formatter/1.3.0/skills/fmt2/SKILL.md").is_file());
        assert!(!seat.join("plugins/cache/example-mkt/formatter/1.2.0").exists());
        let known: Value = serde_json::from_str(&std::fs::read_to_string(seat.join("plugins/known_marketplaces.json")).unwrap()).unwrap();
        assert_eq!(known["example-mkt"]["autoUpdate"], false, "nothing updates on its own");
    }

    /// A synthetic plugin as Claude Code installs one, with a credential
    /// file and a link out of it that must not travel.
    fn installed_plugin(home: &Path) {
        let cache = ".claude/plugins/cache/example-mkt/formatter/1.2.0";
        put(home, &format!("{cache}/.claude-plugin/plugin.json"), r#"{"name": "formatter", "version": "1.2.0", "description": "Formats code"}"#);
        put(home, &format!("{cache}/skills/fmt/SKILL.md"), "---\nname: fmt\ndescription: Format\n---\nFormat it.\n");
        put(home, &format!("{cache}/commands/format.md"), "Format the file.\n");
        put(home, &format!("{cache}/agents/styler.md"), "---\nname: styler\ndescription: Styles\n---\nStyle.\n");
        put(home, &format!("{cache}/hooks/hooks.json"), r#"{"hooks": {"PostToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/bin/fmt"}]}]}}"#);
        put(home, &format!("{cache}/.mcp.json"), r#"{"mcpServers": {"fmt-server": {"command": "node", "args": ["server.js"]}}}"#);
        put(home, &format!("{cache}/bin/fmt"), "#!/bin/sh\n");
        put(home, &format!("{cache}/.env"), "TOKEN=hunter2hunter2\n");
        put(home, ".claude/secret.txt", "outside\n");
        std::os::unix::fs::symlink("../../../../../secret.txt", home.join(cache).join("out-link")).unwrap();
        std::os::unix::fs::symlink("bin/fmt", home.join(cache).join("in-link")).unwrap();
        put(home, ".claude/plugins/marketplaces/example-mkt/.claude-plugin/marketplace.json",
            r#"{"name": "example-mkt", "owner": {"name": "Example"}, "plugins": [{"name": "formatter", "source": "./plugins/formatter", "description": "Formats"}]}"#);
        let root = home.join(".claude/plugins");
        put(home, ".claude/plugins/known_marketplaces.json", &json!({
            "example-mkt": {"source": {"source": "github", "repo": "example/mkt"}, "installLocation": root.join("marketplaces/example-mkt"), "lastUpdated": "2026-10-01T00:00:00Z"},
            "web-mkt": {"source": {"source": "url", "url": "https://example.com/marketplace.json"}, "installLocation": root.join("marketplaces/web-mkt"), "lastUpdated": "2026-10-01T00:00:00Z"}
        }).to_string());
        put(home, ".claude/plugins/installed_plugins.json", &json!({"version": 2, "plugins": {
            "formatter@example-mkt": [{"scope": "user", "installPath": root.join("cache/example-mkt/formatter/1.2.0"), "version": "1.2.0", "installedAt": "2026-10-01T00:00:00Z"}],
            "web@web-mkt": [{"scope": "user", "installPath": root.join("cache/web-mkt/web/1.0.0"), "version": "1.0.0"}]
        }}).to_string());
        put(home, ".claude/plugins/cache/web-mkt/web/1.0.0/.claude-plugin/plugin.json", r#"{"name": "web"}"#);
        put(home, ".claude/settings.json", r#"{"enabledPlugins": {"formatter@example-mkt": true, "local@dev-mkt": true}, "pluginConfigs": {"formatter@example-mkt": {"options": {"style": "tabs"}}}}"#);
    }

    #[test]
    fn a_plugin_comes_across_whole_and_installs_in_a_claude_seat() {
        let (dir, state, home) = setup();
        installed_plugin(&home);
        let port = list(&state, None);
        let formatter = by_key(&port, "plugin:formatter@example-mkt");
        assert!(formatter.reason.is_none(), "{:?}", formatter.reason);
        let parts = formatter.parts.as_ref().unwrap();
        assert_eq!(parts.skills, vec!["fmt"]);
        assert_eq!(parts.commands, vec!["format"]);
        assert_eq!(parts.agents, vec!["styler"]);
        assert_eq!(parts.hooks, vec!["PostToolUse"]);
        assert_eq!(parts.mcp_servers, vec!["fmt-server"]);
        assert!(formatter.left_out.iter().any(|l| l.contains("pluginConfigs")));
        assert!(by_key(&port, "plugin:web@web-mkt").reason.as_deref().unwrap().contains("url source"));
        assert!(by_key(&port, "plugin:local@dev-mkt").reason.is_some());

        let result = import(&state, &ImportReq { keys: vec!["plugin:formatter@example-mkt".into()], project_id: None });
        let id = result.imported[0].id.clone().unwrap_or_else(|| panic!("{:?}", result.imported[0].error));
        let store = toolkit::plugin_dir(&state, &id);
        assert!(store.join("skills/fmt/SKILL.md").is_file());
        assert!(!store.join(".env").exists(), "a credential file never travels");
        assert!(!store.join("out-link").exists(), "a link out of the plugin never travels");
        assert!(std::fs::symlink_metadata(store.join("in-link")).unwrap().file_type().is_symlink());

        // The seat: installed the way Claude Code installs a plugin.
        let seat = dir.path().join("seat-config");
        let seen_at = Path::new("/claude-config");
        let ids = crate::claude_native::install_toolkit_plugins(&state, "s:any", &seat, seen_at).unwrap();
        assert_eq!(ids, vec!["formatter@example-mkt"]);
        let plugins = seat.join("plugins");
        let installed: Value = serde_json::from_str(&std::fs::read_to_string(plugins.join("installed_plugins.json")).unwrap()).unwrap();
        assert_eq!(installed["version"], 2);
        let record = &installed["plugins"]["formatter@example-mkt"][0];
        assert_eq!(record["scope"], "user");
        assert_eq!(record["installPath"], "/claude-config/plugins/cache/example-mkt/formatter/1.2.0");
        assert!(plugins.join("cache/example-mkt/formatter/1.2.0/.claude-plugin/plugin.json").is_file());
        let known: Value = serde_json::from_str(&std::fs::read_to_string(plugins.join("known_marketplaces.json")).unwrap()).unwrap();
        assert_eq!(known["example-mkt"]["installLocation"], "/claude-config/plugins/marketplaces/example-mkt");
        assert_eq!(known["example-mkt"]["autoUpdate"], false);
        assert_eq!(known["example-mkt"]["source"]["repo"], "example/mkt");
        let catalog: Value =
            serde_json::from_str(&std::fs::read_to_string(plugins.join("marketplaces/example-mkt/.claude-plugin/marketplace.json")).unwrap()).unwrap();
        assert_eq!(catalog["plugins"][0]["name"], "formatter");
        assert!(plugins.join("data/formatter-example-mkt").is_dir());

        // Removed from the Toolkit: gone from the seat on its next spawn.
        toolkit::remove(&state, &id).unwrap();
        assert!(!store.exists());
        assert!(crate::claude_native::install_toolkit_plugins(&state, "s:any", &seat, seen_at).unwrap().is_empty());
        assert!(!plugins.join("cache").exists());
    }
}
