//! A secret-named file a project's build needs (GreenLeaf bundles
//! `assets/pub.pem`): a room refuses it, as it refuses every secret-named
//! file, and the operator decides with one press, after seeing what the
//! node says it is. The node notices each refusal in macOS's own log of
//! sandbox denials and puts a card in the thread that was refused. The
//! node, never an agent, looks at the file and says what it is (a public
//! key, a certificate, a private key, a token or env file); its content
//! goes nowhere, card or history. An allowed file is readable, never
//! writable, in every room of the project's threads from then on.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A secret-named file a thread's room was refused, waiting on the
/// operator: on the thread (its card) and on the project's stream (its
/// list).
pub(crate) const ASK: &str = "file.read.ask";
/// The operator's answer for a project: on the project's stream, and on
/// each asking thread, citing its ask.
pub(crate) const DECIDED: &str = "file.read.decided";

/// What the node says a secret-named file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretKind {
    PublicKey,
    Certificate,
    PrivateKey,
    /// Lines of `NAME=value`, or a token on its own.
    TokenOrEnv,
    Unknown,
}

/// What a file is, from its start alone.
fn kind_of(head: &str) -> SecretKind {
    if head.contains("PRIVATE KEY-----") {
        return SecretKind::PrivateKey;
    }
    if head.contains("-----BEGIN PUBLIC KEY-----")
        || head.contains("-----BEGIN RSA PUBLIC KEY-----")
        || ["ssh-rsa ", "ssh-ed25519 ", "ecdsa-sha2-"].iter().any(|k| head.starts_with(k))
    {
        return SecretKind::PublicKey;
    }
    if head.contains("-----BEGIN CERTIFICATE-----") {
        return SecretKind::Certificate;
    }
    let lines: Vec<&str> = head
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let assignment = |l: &&str| {
        l.split_once('=').is_some_and(|(name, _)| {
            let name = name.trim().trim_start_matches("export ");
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    let token = lines.len() == 1 && !lines[0].contains(' ') && lines[0].len() >= 20;
    if (!lines.is_empty() && lines.iter().all(assignment)) || token {
        return SecretKind::TokenOrEnv;
    }
    SecretKind::Unknown
}

/// The node's own look at a file: what it is, and its size. Nothing it
/// reads leaves this function.
fn look(path: &Path) -> (SecretKind, u64) {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut head = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(8192).read_to_end(&mut head));
    match read {
        Ok(_) => (kind_of(&String::from_utf8_lossy(&head)), bytes),
        Err(_) => (SecretKind::Unknown, bytes),
    }
}

#[derive(Serialize, Deserialize)]
struct Asked {
    project_id: String,
    /// From the thread's folder, as every thread of the project has it.
    path: String,
    program: Option<String>,
    kind: SecretKind,
    bytes: u64,
    /// On the project's stream only: the asking thread, and its card.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    stream_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    ask: String,
}

#[derive(Serialize, Deserialize)]
struct Decided {
    project_id: String,
    path: String,
    allow: Option<bool>,
}

/// A file waiting on the operator.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FileReadAsk {
    /// From the thread's folder (`frontend/assets/pub.pem`).
    pub(crate) path: String,
    /// What the node says it is.
    pub(crate) kind: SecretKind,
    pub(crate) bytes: u64,
    /// The program that was refused it (`dartvm`), as macOS names it.
    pub(crate) program: Option<String>,
    /// The thread that asked.
    pub(crate) stream_id: String,
    /// The ask's event in that thread: its card.
    pub(crate) event_hash: String,
    pub(crate) at: String,
}

/// The operator's answers for a project, latest per path: allowed or not,
/// and when.
fn decisions(state: &AppState, project_id: &str) -> Result<HashMap<String, (bool, String)>, String> {
    let heart = state.heart.lock().unwrap();
    let mut out = HashMap::new();
    for ev in heart.events_of_family(project_id, DECIDED).map_err(|e| e.to_string())? {
        let Ok(d) = serde_json::from_str::<Decided>(&ev.payload) else {
            continue;
        };
        match d.allow {
            Some(allow) => out.insert(d.path, (allow, ev.ts)),
            None => out.remove(&d.path),
        };
    }
    Ok(out)
}

/// The project's asks the operator hasn't answered, oldest first.
fn open_asks(state: &AppState, project_id: &str) -> Result<Vec<FileReadAsk>, String> {
    let heart = state.heart.lock().unwrap();
    let asks = heart.events_of_family(project_id, ASK).map_err(|e| e.to_string())?;
    let decided = heart.events_of_family(project_id, DECIDED).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for ev in asks {
        let Ok(a) = serde_json::from_str::<Asked>(&ev.payload) else {
            continue;
        };
        let answered = decided.iter().filter(|d| d.seq > ev.seq).any(|d| {
            serde_json::from_str::<Decided>(&d.payload).is_ok_and(|d| d.allow.is_some() && d.path == a.path)
        });
        if answered || crate::state::discarded_of(&heart, &a.stream_id) {
            continue;
        }
        out.push(FileReadAsk {
            path: a.path,
            kind: a.kind,
            bytes: a.bytes,
            program: a.program,
            stream_id: a.stream_id,
            event_hash: a.ask,
            at: ev.ts,
        });
    }
    Ok(out)
}

/// The file a thread's oldest unanswered ask names: the thread needs the
/// operator.
pub(crate) fn asking_locked(heart: &bakery_heart::Heart, stream_id: &str) -> Option<String> {
    let asks = heart.events_of_family(stream_id, ASK).ok()?;
    if asks.is_empty() {
        return None;
    }
    let decided = heart.events_of_family(stream_id, DECIDED).ok()?;
    asks.iter()
        .filter(|a| !decided.iter().any(|d| d.causality.contains(&a.event_hash)))
        .find_map(|a| serde_json::from_str::<Asked>(&a.payload).ok())
        .map(|a| a.path)
}

/// A path from a thread's folder, kept inside it.
fn relative(path: &str) -> Result<String, String> {
    let inside = Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if path.is_empty() || !inside {
        return Err(format!("{path:?} isn't a path inside the project's folder"));
    }
    Ok(path.to_string())
}

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A thread's room was refused `path` (inside its `workspace`) by
/// `program`: a card in the thread and a line in the project's list, unless
/// the project already asks about it or the operator already answered.
pub(crate) fn refused(
    state: &AppState,
    stream_id: &str,
    workspace: &Path,
    path: &Path,
    program: Option<String>,
) -> Result<(), String> {
    let project_id = {
        let heart = state.heart.lock().unwrap();
        crate::state::socket_project(&heart, stream_id).0
    };
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let Ok(rel) = path.strip_prefix(workspace) else {
        return Ok(());
    };
    let rel = relative(&rel.display().to_string())?;
    let _one = one_at_a_time();
    if decisions(state, &project_id)?.contains_key(&rel)
        || open_asks(state, &project_id)?.iter().any(|a| a.path == rel)
    {
        return Ok(());
    }
    if crate::learning::scratch(state, stream_id) {
        crate::learning::refuse(state, stream_id, &format!("reading {rel}"));
        return Ok(());
    }
    let (kind, bytes) = look(path);
    let mut asked = Asked {
        project_id: project_id.clone(),
        path: rel,
        program,
        kind,
        bytes,
        stream_id: String::new(),
        ask: String::new(),
    };
    let card = serde_json::to_string(&asked).map_err(|e| e.to_string())?;
    let card = crate::state::append(state, stream_id, "bakery", ASK, card, vec![])?;
    asked.stream_id = stream_id.to_string();
    asked.ask = card.event_hash.clone();
    let line = serde_json::to_string(&asked).map_err(|e| e.to_string())?;
    crate::state::append(state, &project_id, "bakery", ASK, line, vec![card.event_hash]).map(drop)
}

/// The secret-named files a thread's rooms may read: the ones the operator
/// allowed for its project, in the thread's own folder.
pub(crate) fn readable_in(state: &AppState, stream_id: &str, workspace: &Path) -> Vec<PathBuf> {
    let project_id = {
        let heart = state.heart.lock().unwrap();
        crate::state::socket_project(&heart, stream_id).0
    };
    let Some(project_id) = project_id else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = decisions(state, &project_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, (allow, _))| *allow)
        .map(|(path, _)| workspace.join(path))
        .collect();
    paths.sort();
    paths
}

/// One of the operator's answers.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FileReadAnswer {
    pub(crate) path: String,
    pub(crate) allow: bool,
    pub(crate) at: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct SecretFiles {
    pub(crate) project_id: String,
    /// The operator's answers, allowed and denied, by path.
    pub(crate) files: Vec<FileReadAnswer>,
    pub(crate) asks: Vec<FileReadAsk>,
}

/// A project's answers and its open asks, as the control plane shows them.
pub(crate) fn files(state: &AppState, project_id: &str) -> Result<SecretFiles, String> {
    let mut files: Vec<FileReadAnswer> = decisions(state, project_id)?
        .into_iter()
        .map(|(path, (allow, at))| FileReadAnswer { path, allow, at })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SecretFiles {
        project_id: project_id.to_string(),
        files,
        asks: open_asks(state, project_id)?,
    })
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct FileReadDecide {
    pub(crate) project_id: String,
    /// From the thread's folder, as the ask names it.
    pub(crate) path: String,
    /// Let the project's builds read it, or not; `null` forgets the answer,
    /// so it asks again.
    pub(crate) allow: Option<bool>,
    /// For a file the node says is a private key: the operator's second
    /// yes.
    #[serde(default)]
    pub(crate) sure: bool,
}

/// The operator's answer for a project. A private key takes saying twice.
pub(crate) fn decide(state: &AppState, req: &FileReadDecide) -> Result<SecretFiles, (StatusCode, String)> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator lets a build read a secret-named file".to_string(),
        ));
    }
    if crate::catalog::project_by_id(state, &req.project_id).is_none() {
        return Err((StatusCode::NOT_FOUND, format!("unknown project {}", req.project_id)));
    }
    let path = relative(&req.path).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let one = one_at_a_time();
    let open: Vec<FileReadAsk> = open_asks(state, &req.project_id)
        .map_err(internal)?
        .into_iter()
        .filter(|a| a.path == path)
        .collect();
    if req.allow == Some(true) && !req.sure && open.iter().any(|a| a.kind == SecretKind::PrivateKey) {
        return Err((
            StatusCode::CONFLICT,
            format!("{path} looks like a private key: allow it again to be sure"),
        ));
    }
    let decided = serde_json::to_string(&Decided {
        project_id: req.project_id.clone(),
        path: path.clone(),
        allow: req.allow,
    })
    .map_err(|e| internal(e.to_string()))?;
    crate::state::append(state, &req.project_id, "human", DECIDED, decided.clone(), vec![]).map_err(internal)?;
    let mut answered: Vec<String> = Vec::new();
    if req.allow.is_some() {
        for ask in &open {
            crate::state::append(state, &ask.stream_id, "human", DECIDED, decided.clone(), vec![ask.event_hash.clone()])
                .map_err(internal)?;
            if !answered.contains(&ask.stream_id) {
                answered.push(ask.stream_id.clone());
            }
        }
    }
    drop(one);
    if req.allow == Some(true) {
        for stream_id in answered {
            crate::agent_control::tell(
                state,
                &stream_id,
                format!("The operator let this project's builds read {path}. Try again."),
            )
            .map_err(internal)?;
        }
    }
    files(state, &req.project_id).map_err(internal)
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct SecretFilesQuery {
    pub(crate) project_id: String,
}

/// `GET /files/read/asks`: a project's answers and open asks.
pub(crate) async fn list(
    State(st): State<AppState>,
    Query(q): Query<SecretFilesQuery>,
) -> Result<Json<SecretFiles>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || files(&st, &q.project_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e))
}

/// `POST /files/read/decide`: the operator's answer.
pub(crate) async fn decide_route(
    State(st): State<AppState>,
    Json(req): Json<FileReadDecide>,
) -> Result<Json<SecretFiles>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || decide(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

/// The program and the path a sandbox read denial names, from a line of
/// `log stream --style compact`:
/// `… Sandbox: dartvm(4169) deny(1) file-read-data /path/to/assets/pub.pem`.
fn denial(line: &str) -> Option<(String, PathBuf)> {
    let (_, said) = line.rsplit_once("Sandbox: ")?;
    let (program, rest) = said.split_once('(')?;
    let (_, path) = rest.split_once(" deny(1) file-read-data ")?;
    Some((program.trim().to_string(), PathBuf::from(path.trim())))
}

/// The thread a refused path belongs to: the one whose folder holds it
/// most closely, the latest when several share it.
fn thread_of(state: &AppState, path: &Path) -> Option<(String, PathBuf)> {
    let streams = crate::state::known_streams(state).ok()?;
    streams
        .into_iter()
        .filter_map(|s| {
            let folder = Path::new(&s.worktree_path).canonicalize().ok()?;
            path.starts_with(&folder)
                .then(|| (folder.components().count(), s.last_ts, s.stream_id, folder))
        })
        .max_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)))
        .map(|(_, _, stream_id, folder)| (stream_id, folder))
}

/// From now on, every secret-named file a room is refused becomes a card in
/// its thread. macOS logs each sandbox denial; the node reads only the read
/// denials of secret-named files. git's are left out: its status and its
/// adds touch every file in a folder, and a secret it tried to add is never
/// one to allow.
pub(crate) fn watch(state: AppState) {
    std::thread::spawn(move || {
        let child = std::process::Command::new("/usr/bin/log")
            .args([
                "stream",
                "--style",
                "compact",
                "--predicate",
                r#"process == "kernel" AND eventMessage CONTAINS "deny(1) file-read-data""#,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return;
        };
        let Some(out) = child.stdout.take() else {
            return;
        };
        let mut seen: HashMap<PathBuf, Instant> = HashMap::new();
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let Some((program, path)) = denial(&line) else {
                continue;
            };
            if program == "git" || !bakery_exec::is_credential_filename(&path) {
                continue;
            }
            // A build tries a file again and again: once a minute is plenty.
            if seen.get(&path).is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
                continue;
            }
            seen.insert(path.clone(), Instant::now());
            if let Some((stream_id, folder)) = thread_of(&state, &path) {
                if let Err(e) = refused(&state, &stream_id, &folder, &path, Some(program)) {
                    eprintln!("secret reads: {}: {e}", path.display());
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn the_node_says_what_a_secret_named_file_is_without_showing_it() {
        assert_eq!(kind_of("-----BEGIN PUBLIC KEY-----\nMIIBIj\n"), SecretKind::PublicKey);
        assert_eq!(kind_of("ssh-ed25519 AAAAC3Nz someone@mac\n"), SecretKind::PublicKey);
        assert_eq!(kind_of("-----BEGIN CERTIFICATE-----\nMIID\n"), SecretKind::Certificate);
        assert_eq!(kind_of("-----BEGIN RSA PRIVATE KEY-----\nMIIE\n"), SecretKind::PrivateKey);
        assert_eq!(kind_of("-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl\n"), SecretKind::PrivateKey);
        assert_eq!(kind_of("-----BEGIN ENCRYPTED PRIVATE KEY-----\n"), SecretKind::PrivateKey);
        assert_eq!(kind_of("# local\nAPI_KEY=abc\nexport HOST=x\n"), SecretKind::TokenOrEnv);
        assert_eq!(kind_of("ghp_0123456789abcdefghij0123\n"), SecretKind::TokenOrEnv);
        assert_eq!(kind_of("{\"hello\": \"world\"}"), SecretKind::Unknown);
        assert_eq!(
            denial("2026-10-11 13:48:06.038 E  kernel[0:3d9dde4] (Sandbox) Sandbox: dartvm(4169) deny(1) file-read-data /w/frontend/assets/pub.pem"),
            Some(("dartvm".to_string(), PathBuf::from("/w/frontend/assets/pub.pem")))
        );
        assert_eq!(denial("Sandbox: git(1) deny(1) file-read-metadata /w/.env"), None);
    }

    /// A refused file asks once, a card in the thread that the node's look
    /// describes; only the operator answers; a private key takes a second
    /// yes; and an allowed file is readable in each thread's own folder.
    #[test]
    fn a_refused_file_asks_once_and_the_operators_answer_holds_for_the_project() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = work.path().join("farm");
        std::fs::create_dir_all(app.join("assets")).unwrap();
        std::fs::write(
            app.join("assets/pub.pem"),
            "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----\n",
        )
        .unwrap();
        std::fs::write(app.join("assets/key.pem"), "-----BEGIN PRIVATE KEY-----\nMIIE\n").unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let p = project.project_id.clone();
        let folder = app.canonicalize().unwrap();
        let sid = thread.stream_id.as_str();
        let pem = folder.join("assets/pub.pem");
        refused(&state, sid, &folder, &pem, Some("dartvm".into())).unwrap();
        refused(&state, sid, &folder, &pem, Some("dartvm".into())).unwrap();
        let asks = files(&state, &p).unwrap().asks;
        assert_eq!(asks.len(), 1, "asked once");
        assert_eq!((asks[0].path.as_str(), asks[0].kind), ("assets/pub.pem", SecretKind::PublicKey));
        let card = state.heart.lock().unwrap().events_of_family(sid, ASK).unwrap();
        assert!(!card[0].payload.contains("MIIB"), "never the content");
        let status = || {
            let heart = state.heart.lock().unwrap();
            crate::agents::conversation_status_locked(&heart, sid, false, 0, None).1
        };
        assert_eq!(status().as_deref(), Some("wants to read assets/pub.pem"));

        let answer = |path: &str, allow: Option<bool>, sure: bool| {
            decide(
                &state,
                &FileReadDecide {
                    project_id: p.clone(),
                    path: path.into(),
                    allow,
                    sure,
                },
            )
        };
        let agent = crate::actor::agent(sid, crate::node::Seat::Orchestrator);
        let by_agent = crate::actor::with(agent, || answer("assets/pub.pem", Some(true), false));
        assert_eq!(by_agent.unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(
            answer("../outside.pem", Some(true), false).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        answer("assets/pub.pem", Some(true), false).unwrap();
        assert_eq!(status(), None, "answered");
        assert_eq!(readable_in(&state, sid, &folder), vec![pem.clone()]);
        let told = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(sid, crate::agent_control::FOLLOW_UP_QUEUED)
            .unwrap()
            .iter()
            .filter(|e| e.payload.contains("read assets/pub.pem. Try again"))
            .count();
        assert_eq!(told, 1);

        refused(&state, sid, &folder, &folder.join("assets/key.pem"), None).unwrap();
        let once = answer("assets/key.pem", Some(true), false).unwrap_err();
        assert_eq!(once.0, StatusCode::CONFLICT, "a private key takes a second yes");
        answer("assets/key.pem", Some(true), true).unwrap();
        assert_eq!(readable_in(&state, sid, &folder).len(), 2);
        answer("assets/key.pem", None, false).unwrap();
        assert_eq!(readable_in(&state, sid, &folder), vec![pem], "forgotten");
    }
}
