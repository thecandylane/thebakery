//! A conversation's own build of the app's UI, put in front of the operator
//! without a deploy. The build is the conversation's `bakery-client/build`,
//! never a path from the caller, hashed exactly as the desktop shell checks
//! a bundle (bakery-client/electron/bundle.ts).
//!
//! Installing asks the operator every time, in every mode: the app window
//! runs the build with their full access, and the approval card says so,
//! with the branch, commit and uncommitted changes the node itself finds.
//! The install copies the build into a staging folder under
//! `~/.bakery/deploy/ui`, which no agent can write, hashes that copy, and
//! only when it is the approved hash moves it into place and points the
//! deployed app at it; the open app checks it and reloads. The hash it
//! replaces is kept as `current/ui-hash.previous`. `ui_bundle_rollback`
//! puts it back one way only, so the Baker can undo a broken install on its
//! own but never reach an older build without asking; the operator's
//! `./scripts/bakery.sh rollback --ui` swaps the two instead.

use crate::routes::git::{checkout, git};
use crate::state::AppState;
use crate::tools::Jail;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const BUILD: &str = "bakery-client/build";
/// Far more than a UI build (bakery-client's is under 20 MB): a bigger one
/// is a mistake, and isn't copied into the node's home.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct BundleQuery {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct InstallReq {
    pub(crate) stream_id: String,
    /// The hash ui_bundle answered: the build the operator approves.
    pub(crate) hash: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Bundle {
    /// What the app checks it against.
    pub(crate) hash: String,
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    /// The checkout it was built in: its branch (None when detached) and commit.
    pub(crate) branch: Option<String>,
    pub(crate) commit: String,
    /// bakery-client has uncommitted changes: the build may hold work no commit has.
    pub(crate) client_dirty: bool,
    /// The bundle the deployed app shows now.
    pub(crate) current: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Installed {
    pub(crate) hash: String,
    /// The bundle it replaced, which ui_bundle_rollback puts back.
    pub(crate) previous: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct RolledBack {
    /// The bundle the app shows now.
    pub(crate) hash: String,
    /// The one it took off.
    pub(crate) replaced: Option<String>,
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A bundle's files as bundle.ts lists them: slash-separated paths under
/// `dir`, in JavaScript's sort order. A link, or anything else that isn't a
/// plain file or folder, is refused: a bundle is plain files.
fn files(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                let rel = path.strip_prefix(dir).ok().and_then(Path::to_str);
                out.push(rel.ok_or_else(|| format!("{} has a name the app can't serve", path.display()))?.to_string());
            } else {
                return Err(format!("{} is not a plain file", path.display()));
            }
        }
    }
    out.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(out)
}

/// bundle.ts's hashBundle: sha256 over each file's path, length and bytes.
fn hash(files: &[String], read: impl Fn(&str) -> Result<Vec<u8>, String>) -> Result<(String, u64), String> {
    let mut sha = Sha256::new();
    let mut bytes = 0;
    for file in files {
        let body = read(file)?;
        bytes += body.len() as u64;
        if bytes > MAX_BYTES {
            return Err(format!("the build is over {} MB; a UI build never is", MAX_BYTES >> 20));
        }
        sha.update(format!("{file}\0{}\0", body.len()));
        sha.update(&body);
    }
    Ok((format!("{:x}", sha.finalize()), bytes))
}

fn hash_dir(dir: &Path) -> Result<String, String> {
    hash(&files(dir)?, |f| std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))).map(|(h, _)| h)
}

/// The conversation's checkout, a jail around it, and its build's files.
fn build(state: &AppState, stream_id: &str) -> Result<(PathBuf, Jail, Vec<String>), String> {
    let dir = checkout(state, stream_id)?;
    let jail = Jail::new(&dir)?;
    let build = jail.resolve(BUILD)?;
    if !build.is_dir() {
        return Err(format!("this conversation has no UI build: run `pnpm --dir bakery-client build` in {} first", dir.display()));
    }
    let files = files(&build)?;
    Ok((dir, jail, files))
}

/// One of the build's files, read inside the checkout only, even if a link
/// appeared after the build was listed.
fn read_in(jail: &Jail, file: &str) -> Result<Vec<u8>, String> {
    let path = jail.resolve(&format!("{BUILD}/{file}"))?;
    if bakery_exec::is_credential_filename(&path) {
        return Err(format!("{file} looks like a credential; a UI build never holds one"));
    }
    std::fs::read(&path).map_err(|e| format!("{file}: {e}"))
}

fn deploy_dir(state: &AppState) -> PathBuf {
    state.base.join("deploy")
}

fn current(deploy: &Path) -> Option<String> {
    let hash = std::fs::read_to_string(deploy.join("current/ui-hash")).ok()?;
    Some(hash.trim().to_string()).filter(|h| !h.is_empty())
}

pub(crate) fn bundle(state: &AppState, stream_id: &str) -> Result<Bundle, String> {
    let (dir, jail, files) = build(state, stream_id)?;
    let (hash, bytes) = hash(&files, |f| read_in(&jail, f))?;
    let line = |args: &[&str]| git(&dir, args).map(|s| s.trim().to_string());
    Ok(Bundle {
        hash,
        files: files.len(),
        bytes,
        branch: line(&["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|b| !b.is_empty() && b != "HEAD"),
        commit: line(&["rev-parse", "--short", "HEAD"]).unwrap_or_default(),
        // Unknown counts as changed.
        client_dirty: line(&["status", "--porcelain", "--", "bakery-client"]).map_or(true, |s| !s.is_empty()),
        current: current(&deploy_dir(state)),
    })
}

/// What the install's approval card says: the node's own look at the build,
/// never the agent's word for it.
pub(crate) fn approval_note(state: &AppState, args: &Value) -> String {
    let full = "The app window will run this build with your full access: whatever you can do in Bakery, its code can do.";
    match bundle(state, args["stream_id"].as_str().unwrap_or_default()) {
        Err(e) => format!("{full}\nThe node couldn't look at the build: {e}"),
        Ok(b) => format!(
            "{full}\nBranch: {}\nCommit: {}\nbakery-client: {}\nBuild: {} files, {} KB{}",
            b.branch.as_deref().unwrap_or("(detached)"),
            if b.commit.is_empty() { "(none)" } else { &b.commit },
            if b.client_dirty { "has uncommitted changes, which the build may include" } else { "no uncommitted changes" },
            b.files,
            b.bytes / 1024,
            if args["hash"].as_str() == Some(b.hash.as_str()) {
                ""
            } else {
                "\nThe build has changed since this hash was taken: the install will refuse it."
            },
        ),
    }
}

/// Removed when dropped, on every way out: the deploy lock and the staging folder.
struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Written whole or not at all, as bakery.sh writes it.
fn write_hash(path: &Path, hash: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{hash}\n"))
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// bakery.sh's own lock: one deploy, rollback or UI change at a time.
fn take_lock(deploy: &Path) -> Result<Temporary, String> {
    let lock = deploy.join(".lock");
    match std::fs::create_dir(&lock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "A deploy, rollback or upgrade is running: try again once it's done (if none is, remove {}).",
                lock.display()
            ))
        }
        Err(e) => return Err(format!("{}: {e}", lock.display())),
    }
    let lock = Temporary(lock);
    std::fs::write(lock.0.join("pid"), std::process::id().to_string()).map_err(|e| e.to_string())?;
    Ok(lock)
}

pub(crate) fn install(state: &AppState, req: &InstallReq) -> Result<Installed, String> {
    let wanted = req.hash.trim();
    if !is_hash(wanted) {
        return Err(format!("{wanted:?} is not a UI bundle hash: ui_bundle answers one"));
    }
    let deploy = deploy_dir(state);
    if !deploy.join("current/Bakery.app").is_dir() {
        return Err("No desktop app is deployed to install a UI into: ./scripts/bakery.sh deploy first.".to_string());
    }
    let (_, jail, files) = build(state, &req.stream_id)?;
    let _lock = take_lock(&deploy)?;

    let stage = deploy.join("ui/.stage");
    let _stage = Temporary(stage.clone());
    let _ = std::fs::remove_dir_all(&stage);
    let mut bytes = 0;
    for file in &files {
        let body = read_in(&jail, file)?;
        bytes += body.len() as u64;
        if bytes > MAX_BYTES {
            return Err(format!("the build is over {} MB; a UI build never is", MAX_BYTES >> 20));
        }
        let to = stage.join(file);
        std::fs::create_dir_all(to.parent().unwrap_or(&stage))
            .and_then(|_| std::fs::write(&to, body))
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    // The staged copy, which no agent can write, is what's checked and served.
    let got = hash_dir(&stage)?;
    if got != wanted {
        return Err(format!(
            "the build changed after it was approved (it now hashes to {}): ask ui_bundle again, then for approval again",
            &got[..12]
        ));
    }
    let dest = deploy.join("ui").join(&got);
    if hash_dir(&dest).ok().as_deref() != Some(got.as_str()) {
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&stage, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    let before = current(&deploy);
    if before.as_deref() != Some(got.as_str()) {
        if let Some(before) = &before {
            write_hash(&deploy.join("current/ui-hash.previous"), before)?;
        }
        write_hash(&deploy.join("current/ui-hash"), &got)?;
    }
    Ok(Installed { previous: before.filter(|b| *b != got), hash: got })
}

/// The UI from before the last install, back in place; its undo record goes
/// with it, so going forward again takes a new install and a new approval.
pub(crate) fn rollback(state: &AppState) -> Result<RolledBack, String> {
    let deploy = deploy_dir(state);
    let undo = deploy.join("current/ui-hash.previous");
    let nothing = "Nothing to roll the UI back to: it hasn't changed since it was deployed or last rolled back.";
    if !undo.is_file() {
        return Err(nothing.to_string());
    }
    let _lock = take_lock(&deploy)?;
    let hash = std::fs::read_to_string(&undo).map(|h| h.trim().to_string()).map_err(|_| nothing.to_string())?;
    if !is_hash(&hash) || !deploy.join("ui").join(&hash).is_dir() {
        return Err(format!("the UI before the last install ({:.12}) is no longer installed", hash));
    }
    let replaced = current(&deploy);
    write_hash(&deploy.join("current/ui-hash"), &hash)?;
    std::fs::remove_file(&undo).map_err(|e| format!("{}: {e}", undo.display()))?;
    Ok(RolledBack { hash, replaced })
}

pub(crate) async fn bundle_route(
    State(st): State<AppState>,
    Query(q): Query<BundleQuery>,
) -> Result<Json<Bundle>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || bundle(&st, &q.stream_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn install_route(
    State(st): State<AppState>,
    Json(req): Json<InstallReq>,
) -> Result<Json<Installed>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || install(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn rollback_route(State(st): State<AppState>) -> Result<Json<RolledBack>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || rollback(&st))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use serde_json::json;
    use std::sync::Arc;

    fn put(dir: &Path, file: &str, text: &str) {
        std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
    }

    #[test]
    fn the_node_hashes_a_bundle_exactly_as_the_app_does() {
        // electron/shell.check.ts pins the app's hash of these same files.
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "index.html", "<!doctype html><p>hi</p>\n");
        put(dir.path(), "a.js", "1");
        put(dir.path(), "a/b.js", "export {}\n");
        put(dir.path(), "_app/immutable/x.css", "");
        assert_eq!(hash_dir(dir.path()).unwrap(), "b3d74736eef58ad680734a1afcfdff658066efef93dbe254efa5dcd3abe9ea01");
        std::os::unix::fs::symlink("/etc/hosts", dir.path().join("hosts")).unwrap();
        assert!(hash_dir(dir.path()).unwrap_err().contains("not a plain file"));
    }

    #[test]
    fn a_ui_build_installs_only_as_the_approved_hash_and_keeps_the_one_it_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        put(&repo, "bakery-client/.gitignore", "build/\n");
        crate::git::run(&repo, &["add", "."]).unwrap();
        crate::git::run(&repo, &["commit", "-qm", "client"]).unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&repo, "ui")).unwrap().context;
        let work = Path::new(&ctx.worktree_path);
        put(work, "bakery-client/build/index.html", "<p>new</p>");
        let sid = ctx.stream_id.as_str();
        let install_as = |hash: &str| install(&state, &InstallReq { stream_id: sid.to_string(), hash: hash.to_string() });

        // api_tools::post_card adds the note to this tool's card, by this name.
        use crate::api_catalog::Risk;
        assert_eq!(crate::api_tools::risk_of("ui_bundle_install"), Some(Risk::Destructive));
        assert_eq!(crate::api_tools::risk_of("ui_bundle"), Some(Risk::Read));

        let deploy = dir.path().join("deploy");
        assert!(install_as(&"0".repeat(64)).unwrap_err().contains("No desktop app is deployed"));
        std::fs::create_dir_all(deploy.join("current/Bakery.app")).unwrap();
        let old = "a".repeat(64);
        std::fs::write(deploy.join("current/ui-hash"), format!("{old}\n")).unwrap();

        let b = bundle(&state, sid).unwrap();
        assert_eq!((b.files, b.branch.as_deref(), b.client_dirty), (1, Some("ui"), false), "{b:?}");
        assert_eq!(b.current.as_deref(), Some(old.as_str()));
        let note = approval_note(&state, &json!({"stream_id": sid, "hash": b.hash}));
        assert!(note.contains("full access") && note.contains("Branch: ui") && note.contains(&b.commit), "{note}");
        assert!(note.contains("no uncommitted changes") && !note.contains("will refuse"), "{note}");
        put(work, "bakery-client/src/app.ts", "edited");
        let note = approval_note(&state, &json!({"stream_id": sid, "hash": "0".repeat(64)}));
        assert!(note.contains("has uncommitted changes") && note.contains("will refuse"), "{note}");

        // Changed after approval: refused, and nothing is left behind.
        put(work, "bakery-client/build/index.html", "<p>swapped</p>");
        assert!(install_as(&b.hash).unwrap_err().contains("changed after it was approved"));
        assert!(!deploy.join("ui").join(&b.hash).exists());
        assert!(!deploy.join("ui/.stage").exists() && !deploy.join(".lock").exists());
        assert_eq!(std::fs::read_to_string(deploy.join("current/ui-hash")).unwrap(), format!("{old}\n"));

        put(work, "bakery-client/build/index.html", "<p>new</p>");
        std::fs::create_dir(deploy.join(".lock")).unwrap();
        assert!(install_as(&b.hash).unwrap_err().contains("is running"));
        std::fs::remove_dir(deploy.join(".lock")).unwrap();

        let done = install_as(&b.hash).unwrap();
        assert_eq!((done.hash.as_str(), done.previous.as_deref()), (b.hash.as_str(), Some(old.as_str())));
        assert_eq!(hash_dir(&deploy.join("ui").join(&b.hash)).unwrap(), b.hash);
        assert_eq!(std::fs::read_to_string(deploy.join("current/ui-hash")).unwrap(), format!("{}\n", b.hash));
        assert_eq!(std::fs::read_to_string(deploy.join("current/ui-hash.previous")).unwrap(), format!("{old}\n"));
        assert!(!deploy.join("ui/.stage").exists() && !deploy.join(".lock").exists());

        // Rolled back one way only: once.
        assert!(rollback(&state).unwrap_err().contains("no longer installed"), "the old bundle was never installed here");
        std::fs::create_dir_all(deploy.join("ui").join(&old)).unwrap();
        let back = rollback(&state).unwrap();
        assert_eq!((back.hash.as_str(), back.replaced.as_deref()), (old.as_str(), Some(b.hash.as_str())));
        assert_eq!(std::fs::read_to_string(deploy.join("current/ui-hash")).unwrap(), format!("{old}\n"));
        assert!(!deploy.join("current/ui-hash.previous").exists() && !deploy.join(".lock").exists());
        assert!(rollback(&state).unwrap_err().contains("Nothing to roll the UI back to"));
        assert_eq!(crate::api_tools::risk_of("ui_bundle_rollback"), Some(Risk::Write));

        // A link in the build, even to a file inside the checkout, is refused.
        std::os::unix::fs::symlink(work.join("f.txt"), work.join("bakery-client/build/f.txt")).unwrap();
        assert!(bundle(&state, sid).unwrap_err().contains("not a plain file"));
    }
}
