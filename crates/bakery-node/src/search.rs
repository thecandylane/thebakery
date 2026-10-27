//! The operator's own search, for the top bar: file names and text, in this
//! workspace or everywhere the Bakery has a folder, secret files included
//! (the operator: "as long as i am the only one that can see them"). Hidden
//! from every agent, who keep their own tools (`tools::Jail::grep` and
//! `glob`), and its answers go back to the app that asked and nowhere else:
//! never the history, never a phone. A secret file's line comes back with
//! its value masked, so a desktop screenshot never carries it; the operator
//! reveals one line at a time (`POST /search/reveal`).

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A search stops at this many matches, and says so.
const MAX_MATCHES: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    /// The conversation's own folder.
    Workspace,
    /// Every project's folders and every chat's folder.
    Everywhere,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchReq {
    /// Plain text to find in file names and in files.
    pub(crate) q: String,
    pub(crate) scope: Scope,
    /// The conversation whose folder "this workspace" is.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
    /// Match case exactly; otherwise case is ignored.
    #[serde(default)]
    pub(crate) exact_case: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Found {
    /// The folder it is in: a project's root, a chat, the workspace.
    pub(crate) folder: String,
    /// That folder's project or chat, by name.
    pub(crate) name: String,
    /// The file, relative to the folder.
    pub(crate) path: String,
    /// The matching line, from 1; none when the file's name matched.
    pub(crate) line: Option<usize>,
    /// That line, or the stretch of it around the match.
    pub(crate) text: Option<String>,
    /// A secret file's line: `text` shows its key, its value masked;
    /// `POST /search/reveal` shows it whole.
    pub(crate) masked: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Searched {
    pub(crate) matches: Vec<Found>,
    /// It stopped at its most matches; a narrower search finds the rest.
    pub(crate) truncated: bool,
}

/// `POST /search`: the operator's alone (hidden from agents, never remote).
pub(crate) async fn search(
    State(st): State<AppState>,
    Json(req): Json<SearchReq>,
) -> Result<Json<Searched>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || run(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) fn run(state: &AppState, req: &SearchReq) -> Result<Searched, String> {
    let q = req.q.trim();
    if q.is_empty() {
        return Err("search for something".to_string());
    }
    let matcher = Matcher::new(q, req.exact_case);
    let mut out = Searched {
        matches: Vec::new(),
        truncated: false,
    };
    for (folder, name) in folders(state, req.scope, req.stream_id.as_deref())? {
        for rel in files(&folder) {
            let path = folder.join(&rel);
            if never_searched(state, &path) {
                continue;
            }
            if !scan(&folder, &name, &rel, &path, &matcher, &mut out.matches) {
                out.truncated = true;
                return Ok(out);
            }
        }
    }
    Ok(out)
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct RevealReq {
    /// The search it came from: its scope and conversation.
    pub(crate) scope: Scope,
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
    /// The match: its folder, its file (relative) and its line, as found.
    pub(crate) folder: String,
    pub(crate) path: String,
    pub(crate) line: usize,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Revealed {
    /// The line, whole: the secret itself.
    pub(crate) text: String,
    /// Always: no desktop screenshot while the app shows it, the same
    /// `secret` every reply carrying one has.
    pub(crate) secret: bool,
}

/// `POST /search/reveal`: one masked line, whole, read again from its secret
/// file, for the operator alone (hidden from agents, never remote, never
/// recorded).
pub(crate) async fn reveal(
    State(st): State<AppState>,
    Json(req): Json<RevealReq>,
) -> Result<Json<Revealed>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || reveal_line(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(|text| Json(Revealed { text, secret: true }))
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// Only a secret file's line, in a folder that search covers, that search
/// itself could have found.
pub(crate) fn reveal_line(state: &AppState, req: &RevealReq) -> Result<String, String> {
    let folder = folders(state, req.scope, req.stream_id.as_deref())?
        .into_iter()
        .map(|(folder, _)| folder)
        .find(|f| f.display().to_string() == req.folder)
        .ok_or("that folder isn't one this search covers")?;
    let path = crate::tools::Jail::new(&folder)?.resolve(&req.path)?;
    if never_searched(state, &path) || !bakery_exec::is_credential_filename(&path) {
        return Err("only a secret file's masked line is revealed".to_string());
    }
    readable(&path)
        .and_then(|text| {
            text.lines()
                .nth(req.line.checked_sub(1)?)
                .map(str::to_string)
        })
        .map(|line| line.trim().to_string())
        .ok_or_else(|| "that line isn't there any more".to_string())
}

/// The folders a scope covers, each once, with its project's or chat's name.
fn folders(
    state: &AppState,
    scope: Scope,
    stream_id: Option<&str>,
) -> Result<Vec<(PathBuf, String)>, String> {
    let mut found = Vec::new();
    match scope {
        Scope::Workspace => {
            let stream_id = stream_id.ok_or("this workspace needs its conversation (stream_id)")?;
            let ctx = crate::state::context_for(state, stream_id)?;
            if ctx.worktree_path.trim().is_empty() {
                return Err("this conversation has no folder to search".to_string());
            }
            found.push((PathBuf::from(&ctx.worktree_path), ctx.name));
        }
        Scope::Everywhere => {
            for project in crate::catalog::known_projects(state)? {
                for root in project.roots {
                    found.push((PathBuf::from(root.path), project.name.clone()));
                }
            }
            let chats = std::fs::read_dir(state.base.join("chats"))
                .into_iter()
                .flatten();
            for chat in chats.flatten().filter(|c| c.path().is_dir()) {
                found.push((chat.path(), chat.file_name().to_string_lossy().into_owned()));
            }
        }
    }
    let mut seen = HashSet::new();
    found.retain(|(folder, _)| folder.is_dir() && seen.insert(folder.canonicalize().ok()));
    Ok(found)
}

/// A folder's files, relative to it. In a Git work tree, what Git doesn't
/// ignore (tracked, or new and not ignored), plus the secret files it
/// ignores (a `.env` almost always is); elsewhere, every file. Either way
/// never under node_modules/, target/ and the other heavy folders the
/// agents' tools skip too.
fn files(folder: &Path) -> Vec<PathBuf> {
    let skipped = |rel: &str| {
        rel.split('/')
            .any(|part| crate::tools::SKIP_DIRS.contains(&part))
    };
    let listed = |extra: &[&str]| {
        let mut args = vec!["ls-files", "-z", "--others", "--exclude-standard"];
        args.extend(extra);
        crate::git::run(folder, &args).ok()
    };
    if let Some(kept) = listed(&["--cached"]) {
        // Ignored folders come folded ("build/"), so only their own files
        // are listed one by one; the secret ones among them are searched.
        let ignored = listed(&["--ignored", "--directory"]).unwrap_or_default();
        let secret = |rel: &&str| bakery_exec::is_credential_filename(Path::new(rel));
        return kept
            .split('\0')
            .chain(ignored.split('\0').filter(secret))
            .filter(|rel| !rel.is_empty() && !rel.ends_with('/') && !skipped(rel))
            .map(PathBuf::from)
            .collect();
    }
    let Ok(jail) = crate::tools::Jail::new(folder) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut deeper = false;
    jail.walk(
        jail.root(),
        0,
        &mut |path, is_dir| {
            if !is_dir {
                if let Ok(rel) = path.strip_prefix(jail.root()) {
                    out.push(rel.to_path_buf());
                }
            }
            true
        },
        &mut deeper,
    );
    out
}

/// Never searched, even for the operator: the Bakery's own machinery (its
/// identity and token, the model proxy's keys, the agents' homes and
/// sign-ins, the history), all but the projects, chats and worktrees it
/// keeps; and the sign-ins and keys outside it that agents never reach
/// either (`bakery_exec::default_deny_paths`).
fn never_searched(state: &AppState, path: &Path) -> bool {
    let base = state
        .base
        .canonicalize()
        .unwrap_or_else(|_| state.base.to_path_buf());
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let kept = ["projects", "chats", "worktrees"]
        .iter()
        .any(|d| path.starts_with(base.join(d)));
    (path.starts_with(&base) && !kept)
        || bakery_exec::default_deny_paths()
            .iter()
            .any(|denied| path.starts_with(denied))
}

/// A file small enough to search, as text; binary and unreadable files
/// aren't.
fn readable(path: &Path) -> Option<String> {
    let small = std::fs::metadata(path)
        .is_ok_and(|m| m.is_file() && m.len() <= crate::tools::MAX_SCAN_BYTES);
    small.then(|| std::fs::read_to_string(path).ok()).flatten()
}

/// One file: its name, then its lines. `false` once the search is full.
fn scan(
    folder: &Path,
    name: &str,
    rel: &Path,
    path: &Path,
    matcher: &Matcher,
    out: &mut Vec<Found>,
) -> bool {
    let secret = bakery_exec::is_credential_filename(path);
    // A key file has no names worth showing: every line is masked whole.
    let key_file = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["pem", "key"].contains(&e.to_ascii_lowercase().as_str()));
    let found = |line: Option<usize>, text: Option<String>| Found {
        folder: folder.display().to_string(),
        name: name.to_string(),
        path: rel.display().to_string(),
        line,
        masked: secret && text.is_some(),
        text,
    };
    if matcher.at(&rel.to_string_lossy()).is_some() {
        out.push(found(None, None));
        if out.len() >= MAX_MATCHES {
            return false;
        }
    }
    let Some(text) = readable(path) else {
        return true;
    };
    for (i, line) in text.lines().enumerate() {
        if let Some(at) = matcher.at(line) {
            let shown = match (secret, key_file) {
                (true, true) => "•••".to_string(),
                (true, false) => masked(line),
                _ => shown(line, at),
            };
            out.push(found(Some(i + 1), Some(shown)));
            if out.len() >= MAX_MATCHES {
                return false;
            }
        }
    }
    true
}

/// Plain text, case ignored unless asked.
struct Matcher {
    needle: String,
    exact: bool,
}

impl Matcher {
    fn new(q: &str, exact: bool) -> Self {
        let needle = if exact {
            q.to_string()
        } else {
            q.to_lowercase()
        };
        Matcher { needle, exact }
    }

    /// Where in `hay` it matches (a byte offset into `hay`), if it does.
    fn at(&self, hay: &str) -> Option<usize> {
        if self.exact {
            return hay.find(&self.needle);
        }
        let lower = hay.to_lowercase();
        let at = lower.find(&self.needle)?;
        // Lowercasing can change a character's length; then the line's start.
        Some(if lower.len() == hay.len() { at } else { 0 })
    }
}

/// A line as results show it: whole when short, else the stretch around its
/// match.
fn shown(line: &str, at: usize) -> String {
    let line = line.trim_end();
    let max = crate::tools::MAX_LINE_CHARS;
    if line.chars().count() <= max {
        return line.trim_start().to_string();
    }
    let mut start = at.saturating_sub(max / 4);
    while !line.is_char_boundary(start) {
        start -= 1;
    }
    let lead = if start > 0 { "…" } else { "" };
    format!(
        "{lead}{}",
        line[start..].chars().take(max).collect::<String>()
    )
}

/// A secret file's line as results show it: its key, and its value masked.
/// A long value keeps its first three characters, which name a key's kind
/// ("sk-…•••"). A line with no name-like key (a key file's base64, which
/// may end in "=") is masked whole.
fn masked(line: &str) -> String {
    let line = line.trim();
    let keyed = line.find(['=', ':']).filter(|&at| {
        let key = line[..at]
            .trim()
            .trim_start_matches("export ")
            .trim_matches(['"', '\'']);
        (1..=40).contains(&key.len())
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    });
    let Some(at) = keyed else {
        return "•••".to_string();
    };
    let (key, value) = line.split_at(at + 1);
    let value = value.trim().trim_matches(['"', '\'']);
    let kind: String = value.chars().take(3).collect();
    if value.chars().count() >= 16 {
        format!("{key}{kind}…•••")
    } else {
        format!("{key}•••")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{make_repo, open_req, run_git, state_with};
    use std::sync::Arc;

    /// Planted for this test alone; never a real key.
    const PLANTED: &str = "sk-w2-planted-0123456789abcdef";

    #[test]
    fn it_finds_names_and_text_where_the_operator_asks_and_skips_what_git_ignores() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let repo = make_repo(&work.path().join("repo"));
        std::fs::write(repo.join(".gitignore"), "build/\n.env\n").unwrap();
        std::fs::write(repo.join("notes.md"), "Find the Needle here\nnothing\n").unwrap();
        std::fs::create_dir_all(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/out.txt"), "needle in output\n").unwrap();
        std::fs::create_dir_all(repo.join("node_modules/x")).unwrap();
        std::fs::write(repo.join("node_modules/x/needle.js"), "needle\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "files"]);
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "find"))
            .unwrap()
            .context;
        crate::catalog::create_project(
            &state,
            "app",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                repo.to_str().unwrap(),
            )),
        )
        .unwrap();
        let ws = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(ws.join("new.txt"), "a NEEDLE too\n").unwrap();
        let ask = |q: &str, scope, exact_case| {
            run(
                &state,
                &SearchReq {
                    q: q.into(),
                    scope,
                    stream_id: Some(ctx.stream_id.clone()),
                    exact_case,
                },
            )
            .unwrap()
        };

        let found = ask("needle", Scope::Workspace, false);
        assert_eq!(hits(&found), ["new.txt:1", "notes.md:1"], "{found:?}");
        assert!(found.matches.iter().all(|m| m
            .text
            .as_deref()
            .is_some_and(|t| t.to_lowercase().contains("needle"))));
        assert!(!found.truncated && found.matches.iter().all(|m| !m.masked));
        assert_eq!(hits(&ask("Needle", Scope::Workspace, true)), ["notes.md:1"]);
        assert_eq!(
            hits(&ask("notes", Scope::Workspace, false)),
            ["notes.md:0"],
            "a name"
        );

        // Everywhere: the project's own folder, not the thread's copy.
        let all = ask("needle", Scope::Everywhere, false);
        assert_eq!(hits(&all), ["notes.md:1"], "{all:?}");
        let blank = SearchReq {
            q: " ".into(),
            scope: Scope::Workspace,
            stream_id: None,
            exact_case: false,
        };
        assert!(run(&state, &blank).is_err());
    }

    /// A secret file is searched even when git ignores it, its value comes
    /// back masked, and only the operator's reveal shows it, one line.
    #[test]
    fn a_secret_files_line_is_found_masked_and_revealed_one_line_at_a_time() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let repo = make_repo(&work.path().join("repo"));
        std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "ignore"]);
        std::fs::write(
            repo.join(".env"),
            format!("OPENAI_API_KEY={PLANTED}\nPORT=3000\n"),
        )
        .unwrap();
        std::fs::write(repo.join("app.js"), "process.env.OPENAI_API_KEY\n").unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        crate::catalog::create_project(
            &state,
            "keys",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                repo.to_str().unwrap(),
            )),
        )
        .unwrap();
        let ask = |q: &str| {
            run(
                &state,
                &SearchReq {
                    q: q.into(),
                    scope: Scope::Everywhere,
                    stream_id: None,
                    exact_case: false,
                },
            )
            .unwrap()
        };

        let found = ask("openai_api_key");
        assert_eq!(hits(&found), [".env:1", "app.js:1"], "{found:?}");
        let secret = found.matches.iter().find(|m| m.path == ".env").unwrap();
        assert!(secret.masked);
        assert_eq!(secret.text.as_deref(), Some("OPENAI_API_KEY=sk-…•••"));
        let by_value = ask("planted-0123");
        assert_eq!(hits(&by_value), [".env:1"], "found by its value too");
        let text =
            serde_json::to_string(&found).unwrap() + &serde_json::to_string(&by_value).unwrap();
        assert!(
            !text.contains(PLANTED),
            "a search never answers with the value"
        );

        let reveal = |path: &str, line: usize| {
            reveal_line(
                &state,
                &RevealReq {
                    scope: Scope::Everywhere,
                    stream_id: None,
                    folder: secret.folder.clone(),
                    path: path.into(),
                    line,
                },
            )
        };
        assert_eq!(
            reveal(".env", 1).unwrap(),
            format!("OPENAI_API_KEY={PLANTED}")
        );
        assert!(reveal("app.js", 1).is_err(), "only a secret file's line");
        assert!(
            reveal("../outside/.env", 1).is_err(),
            "never outside its folder"
        );
        assert!(reveal(".env", 9).is_err(), "a line that isn't there");
        let elsewhere = RevealReq {
            scope: Scope::Everywhere,
            stream_id: None,
            folder: work.path().display().to_string(),
            path: "repo/.env".into(),
            line: 1,
        };
        assert!(
            reveal_line(&state, &elsewhere).is_err(),
            "only folders search covers"
        );
    }

    #[test]
    fn the_bakerys_own_machinery_is_never_searched() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let base = state.base.to_path_buf();
        let made = |rel: &str| {
            let path = base.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
            path
        };
        for kept in [
            "projects/app/src/x.rs",
            "chats/c1/notes.md",
            "worktrees/w/x.rs",
        ] {
            assert!(!never_searched(&state, &made(kept)), "{kept}");
        }
        for machinery in [
            "node.json",
            "heart.db",
            "seats/a/.claude/auth.json",
            "proxy/config.yaml",
        ] {
            assert!(never_searched(&state, &made(machinery)), "{machinery}");
        }
        assert!(!never_searched(
            &state,
            Path::new("/tmp/elsewhere/notes.md")
        ));
    }

    #[test]
    fn lines_show_whole_or_around_the_match_and_secrets_masked() {
        let line = format!("{}needle{}", "a".repeat(400), "b".repeat(400));
        let at = line.find("needle").unwrap();
        let text = shown(&line, at);
        assert!(text.starts_with('…') && text.contains("needle"), "{text}");
        assert!(text.chars().count() <= crate::tools::MAX_LINE_CHARS + 1);
        assert_eq!(shown("  short needle  ", 8), "short needle");
        assert_eq!(masked("PASSWORD=hunter2"), "PASSWORD=•••");
        assert_eq!(
            masked("  \"token\": \"ghp_0123456789abcdefgh\","),
            "\"token\":ghp…•••"
        );
        assert_eq!(masked("MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"), "•••");
        assert_eq!(
            masked("MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDf=="),
            "•••"
        );
        assert_eq!(
            masked("export DB_URL='postgres://u:p@h/db'"),
            "export DB_URL=pos…•••"
        );
    }

    fn hits(s: &Searched) -> Vec<String> {
        let mut all: Vec<String> = s
            .matches
            .iter()
            .map(|m| format!("{}:{}", m.path, m.line.unwrap_or(0)))
            .collect();
        all.sort();
        all
    }
}
