//! Project catalog routes (07d-A3): create/rename/pin/retire a Project,
//! open a plain directory as a root + socket, and attach/detach an
//! existing socket's membership. Thin wrappers over `crate::catalog` —
//! every actual invariant (root uniqueness, fail-closed reads, atomicity)
//! lives there and is unit-tested there; these handlers only translate
//! HTTP <-> the catalog's own request/response shapes.

use crate::catalog::{self, ProjectRecord};
use crate::node::ProjectStacks;
use crate::state::{context_for, ActiveContext, AppState};
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct RootView {
    pub(crate) root_id: String,
    pub(crate) path: String,
    pub(crate) is_git: bool,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct ProjectView {
    pub(crate) project_id: String,
    pub(crate) name: String,
    pub(crate) pinned: bool,
    pub(crate) roots: Vec<RootView>,
    pub(crate) stacks: ProjectStacks,
}

impl From<ProjectRecord> for ProjectView {
    fn from(r: ProjectRecord) -> Self {
        ProjectView {
            project_id: r.project_id,
            name: r.name,
            pinned: r.pinned,
            roots: r
                .roots
                .into_iter()
                .map(|root| RootView {
                    root_id: root.root_id,
                    path: root.path,
                    is_git: root.is_git,
                })
                .collect(),
            stacks: r.stacks,
        }
    }
}

/// Explicit attach-existing vs. create-new directory-root intent (replaces
/// the old ambiguous `path` + `create_if_missing: bool`, which let a
/// "create new" request against an already-existing path silently attach
/// to it instead of erroring). Mirrors `catalog::DirectoryRootSpec`
/// structurally — `PreparedRoot::spec` below is the only place that converts between
/// them, so the wire shape and the internal enum can never drift apart
/// unnoticed.
#[derive(schemars::JsonSchema, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum CreateProjectRoot {
    Attach {
        path: String,
    },
    Create {
        parent: String,
        name: String,
    },
    /// A fresh folder inside Bakery's own (`catalog::scaffold_bakery_project`)
    /// — no path to choose.
    Bakery {},
}

/// A root request made concrete. `Bakery {}` becomes a fresh folder inside
/// Bakery first, then attaches like any existing folder; `undo` removes that
/// folder when the project or root is then refused.
pub(crate) struct PreparedRoot<'a> {
    req: &'a CreateProjectRoot,
    made: Option<String>,
}

impl<'a> PreparedRoot<'a> {
    pub(crate) fn new(
        state: &AppState,
        name: &str,
        req: &'a CreateProjectRoot,
    ) -> Result<Self, String> {
        let made = match req {
            CreateProjectRoot::Bakery {} => Some(
                catalog::scaffold_bakery_project(&state.base, name)?
                    .to_string_lossy()
                    .into_owned(),
            ),
            _ => None,
        };
        Ok(Self { req, made })
    }

    pub(crate) fn spec(&self) -> catalog::DirectoryRootSpec<'_> {
        match self.req {
            CreateProjectRoot::Attach { path } => catalog::DirectoryRootSpec::Attach(path),
            CreateProjectRoot::Create { parent, name } => {
                catalog::DirectoryRootSpec::Create { parent, name }
            }
            CreateProjectRoot::Bakery {} => {
                catalog::DirectoryRootSpec::Attach(self.made.as_deref().unwrap_or_default())
            }
        }
    }

    pub(crate) fn undo(&self) {
        if let Some(dir) = &self.made {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct CreateProjectReq {
    pub(crate) name: String,
    /// `None` = chat-only (no root at creation time).
    #[serde(default)]
    pub(crate) root: Option<CreateProjectRoot>,
    #[serde(default)]
    pub(crate) stacks: ProjectStacks,
    /// What its first conversation's agents may use of the Bakery's tools:
    /// all (the default), read or none.
    #[serde(default)]
    pub(crate) tools: Option<crate::api_tools::ToolsLimit>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct CreateProjectResp {
    pub(crate) project: ProjectView,
    pub(crate) socket: ActiveContext,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct CreateChatReq {
    #[serde(default)]
    pub(crate) title: String,
    /// What its agents may use of the Bakery's tools: all (the default),
    /// read or none.
    #[serde(default)]
    pub(crate) tools: Option<crate::api_tools::ToolsLimit>,
}

/// A new loose chat (`catalog::create_chat`).
pub(crate) async fn chat_create(
    State(st): State<AppState>,
    Json(req): Json<CreateChatReq>,
) -> Result<Json<ActiveContext>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        let chat = catalog::create_chat(&st, &req.title)?;
        crate::api_tools::set_tools_at_creation(&st, &chat.stream_id, req.tools)?;
        Ok(chat)
    })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn project_create(
    State(st): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Result<Json<CreateProjectResp>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || create_project(&st, req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// One path for the human route and the Baker's `project_create` op.
pub(crate) fn create_project(
    state: &AppState,
    req: CreateProjectReq,
) -> Result<CreateProjectResp, String> {
    let prepared = match &req.root {
        Some(root) => Some(PreparedRoot::new(state, &req.name, root)?),
        None => None,
    };
    let created = catalog::create_project_with(
        state,
        &req.name,
        prepared.as_ref().map(PreparedRoot::spec),
        req.stacks,
    )
    .map(|(record, socket)| CreateProjectResp {
        project: record.into(),
        socket,
    })
    .inspect_err(|_| {
        if let Some(p) = &prepared {
            p.undo();
        }
    })?;
    crate::api_tools::set_tools_at_creation(state, &created.socket.stream_id, req.tools)?;
    Ok(created)
}

/// One path for the human route and the Baker's `socket_open` op.
pub(crate) fn open_root(state: &AppState, req: &OpenRootReq) -> Result<ActiveContext, String> {
    let prepared = PreparedRoot::new(state, &req.name, &req.root)?;
    let opened = catalog::attach_directory_root(state, &req.project_id, &req.name, prepared.spec())
        .inspect_err(|_| prepared.undo())?;
    crate::api_tools::set_tools_at_creation(state, &opened.stream_id, req.tools)?;
    Ok(opened)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RenameProjectReq {
    pub(crate) project_id: String,
    pub(crate) name: String,
}

pub(crate) async fn project_rename(
    State(st): State<AppState>,
    Json(req): Json<RenameProjectReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    catalog::rename_project(&st, &req.project_id, &req.name)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // A real (empty) JSON body, not 204 No Content — the client's `send()`
    // always calls `JSON.parse` on the response text, which throws on "".
    Ok(Json(serde_json::json!({})))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PinProjectReq {
    pub(crate) project_id: String,
    pub(crate) pinned: bool,
}

pub(crate) async fn project_pin(
    State(st): State<AppState>,
    Json(req): Json<PinProjectReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    catalog::set_project_pinned(&st, &req.project_id, req.pinned)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(serde_json::json!({})))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RetireProjectReq {
    pub(crate) project_id: String,
}

pub(crate) async fn project_retire(
    State(st): State<AppState>,
    Json(req): Json<RetireProjectReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    catalog::retire_project(&st, &req.project_id).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // Its own terminals go with it: no shell left in a folder it let go.
    let terminals = format!("{}{}", crate::terminals::PROJECT_STREAM_PREFIX, req.project_id);
    crate::terminals::fence_active_ptys(&st, &terminals, None).await;
    Ok(Json(serde_json::json!({})))
}

/// "Open any directory as a root + in-place socket" on an existing Project —
/// no git worktree machinery, no admission requirement beyond "the path
/// resolves to a directory" (07d-A3 §1).
#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct OpenRootReq {
    pub(crate) project_id: String,
    pub(crate) name: String,
    pub(crate) root: CreateProjectRoot,
    /// What its agents may use of the Bakery's tools: all (the default),
    /// read or none.
    #[serde(default)]
    pub(crate) tools: Option<crate::api_tools::ToolsLimit>,
}

pub(crate) async fn project_root_open(
    State(st): State<AppState>,
    Json(req): Json<OpenRootReq>,
) -> Result<Json<ActiveContext>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || open_root(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// Re-parent (or first-attach) an ALREADY-OPEN socket to a Project — the
/// socket's own recorded `worktree_path`/`repo_root` (from its
/// `project.onboarded` event) supplies the root path and git-ness; the
/// caller never re-states them.
#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AttachSocketReq {
    pub(crate) stream_id: String,
    pub(crate) project_id: String,
}

pub(crate) async fn project_socket_attach(
    State(st): State<AppState>,
    Json(req): Json<AttachSocketReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ctx = context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if ctx.worktree_path.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "this socket has no root to attach — it is chat-only".to_string(),
        ));
    }
    let is_git = !ctx.repo_root.is_empty();
    catalog::link_socket_to_project(
        &st,
        &req.stream_id,
        &req.project_id,
        &ctx.worktree_path,
        is_git,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(serde_json::json!({})))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct DetachSocketReq {
    pub(crate) stream_id: String,
}

pub(crate) async fn project_socket_detach(
    State(st): State<AppState>,
    Json(req): Json<DetachSocketReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    catalog::unlink_socket_from_project(&st, &req.stream_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(serde_json::json!({})))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MakeProjectReq {
    pub(crate) stream_id: String,
    /// The project's name; the chat's title when absent.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// The folder, absolute, that the project's folder goes in; Bakery's own
    /// projects folder when absent.
    #[serde(default)]
    pub(crate) parent: Option<String>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct MadeProject {
    pub(crate) project_id: String,
    pub(crate) name: String,
    /// Where the chat's files are now: the project's root.
    pub(crate) folder: String,
}

/// `POST /chat/make-project`: a chat becomes a project, its folder moved
/// into Bakery's projects folder (or one the operator chose) as a Git
/// repository, and the conversation carries on there. Its runtime,
/// terminals and editor stop first, since their folder moves; its next
/// message starts a fresh runtime there. Hidden from agents.
pub(crate) async fn chat_make_project(
    State(st): State<AppState>,
    Json(req): Json<MakeProjectReq>,
) -> Result<Json<MadeProject>, (StatusCode, String)> {
    let ctx = context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if !catalog::is_chat_folder(&st.base, &ctx.worktree_path) {
        return Err((
            StatusCode::CONFLICT,
            "only a chat with its own folder becomes a project this way".to_string(),
        ));
    }
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let (state, stream_id) = (st.clone(), req.stream_id.clone());
    crate::actor::spawn_blocking(move || crate::agents::restart_runtime_inner(&state, &stream_id))
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    if !crate::terminals::fence_active_ptys(&st, &req.stream_id, None).await {
        return Err((
            StatusCode::CONFLICT,
            "a terminal of it didn't end in time; nothing moved".to_string(),
        ));
    }
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&ctx.name)
        .to_string();
    let (state, stream_id, parent) = (st.clone(), req.stream_id.clone(), req.parent);
    let (record, folder) = crate::actor::spawn_blocking(move || {
        crate::views::stop_all(&stream_id);
        let parent = parent.as_deref().map(std::path::Path::new);
        catalog::make_project_from_chat(&state, &stream_id, &name, parent)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(Json(MadeProject {
        project_id: record.project_id,
        name: record.name,
        folder,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::{open_stream, OpenReq};
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// "Make this a project": the chat's folder moves into Bakery's projects
    /// folder as a Git repository whose first commit holds its files, and the
    /// conversation carries on there, in the new project. A chosen folder
    /// works the same; a second try, or a conversation that isn't a chat, is
    /// refused.
    #[tokio::test]
    async fn a_chat_becomes_a_project_and_carries_on_there() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let chat = catalog::create_chat(&state, "Garden plans").unwrap();
        let old = std::path::PathBuf::from(&chat.worktree_path);
        std::fs::write(old.join("beds.md"), "tomatoes").unwrap();
        let make = |stream_id: &str, name: Option<&str>, parent: Option<String>| {
            chat_make_project(
                State(state.clone()),
                Json(MakeProjectReq {
                    stream_id: stream_id.to_string(),
                    name: name.map(str::to_string),
                    parent,
                }),
            )
        };

        let made = make(&chat.stream_id, None, None).await.unwrap().0;
        assert_eq!(made.name, "Garden plans");
        let folder = std::path::PathBuf::from(&made.folder);
        assert!(folder.starts_with(state.base.join("projects").canonicalize().unwrap()));
        assert_eq!(folder.file_name().unwrap(), "Garden-plans");
        assert!(!old.exists(), "moved, not copied");
        assert_eq!(
            std::fs::read_to_string(folder.join("beds.md")).unwrap(),
            "tomatoes"
        );
        let tracked = crate::git::run(&folder, &["ls-files"]).unwrap();
        assert!(
            tracked.contains("beds.md"),
            "the first commit holds it: {tracked}"
        );
        assert_eq!(
            context_for(&state, &chat.stream_id).unwrap().worktree_path,
            made.folder
        );
        let project = catalog::project_by_id(&state, &made.project_id).unwrap();
        assert_eq!(project.roots[0].path, made.folder);
        let axum::Json(view) = crate::routes::studio::studio_state(State(state.clone()))
            .await
            .unwrap();
        let group = view
            .projects
            .iter()
            .find(|p| p.project_id == made.project_id)
            .unwrap();
        let socket = group
            .sockets
            .iter()
            .find(|s| s.stream_id == chat.stream_id)
            .unwrap();
        assert!(!socket.chat, "it's the project's now");
        assert!(
            make(&chat.stream_id, None, None).await.is_err(),
            "only once"
        );

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let notes = catalog::create_chat(&state, "notes").unwrap();
        let chosen = make(
            &notes.stream_id,
            Some("Notes"),
            Some(elsewhere.display().to_string()),
        )
        .await
        .unwrap()
        .0;
        assert!(std::path::Path::new(&chosen.folder).starts_with(elsewhere.canonicalize().unwrap()));

        let (_, starter) = catalog::create_project(&state, "Real", None).unwrap();
        assert!(
            make(&starter.stream_id, None, None).await.is_err(),
            "not a chat"
        );
    }

    /// A project with no folder chosen gets its own inside Bakery — a Git
    /// repo with a first commit, worked on in place — and two projects with
    /// the same name never share a folder.
    #[tokio::test]
    async fn a_new_project_gets_its_own_folder_inside_bakery() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let create = |name: &str| {
            project_create(
                State(state.clone()),
                Json(CreateProjectReq {
                    name: name.into(),
                    root: Some(CreateProjectRoot::Bakery {}),
                    stacks: Default::default(),
                    tools: None,
                }),
            )
        };

        let first = create("Green Leaf").await.unwrap().0;
        let folder = std::path::PathBuf::from(&first.socket.worktree_path);
        assert!(folder.starts_with(state.base.join("projects").canonicalize().unwrap()));
        assert_eq!(folder.file_name().unwrap(), "Green-Leaf");
        assert!(folder.join("README.md").is_file());
        assert!(crate::git::inspect(&first.socket.worktree_path).is_git);
        assert_eq!(first.socket.branch, "main");
        assert_eq!(first.project.roots.len(), 1);

        let second = create("Green Leaf").await.unwrap().0;
        assert!(second.socket.worktree_path.ends_with("Green-Leaf-2"));
    }

    #[tokio::test]
    async fn create_rename_pin_retire_round_trip_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "Http Project".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(created.project.name, "Http Project");
        assert!(created.socket.stream_id.starts_with("socket_"));

        project_rename(
            State(state.clone()),
            Json(RenameProjectReq {
                project_id: created.project.project_id.clone(),
                name: "Renamed".into(),
            }),
        )
        .await
        .unwrap()
        .0;

        project_pin(
            State(state.clone()),
            Json(PinProjectReq {
                project_id: created.project.project_id.clone(),
                pinned: true,
            }),
        )
        .await
        .unwrap()
        .0;

        let after_pin = catalog::project_by_id(&state, &created.project.project_id).unwrap();
        assert_eq!(after_pin.name, "Renamed");
        assert!(after_pin.pinned);

        project_retire(
            State(state.clone()),
            Json(RetireProjectReq {
                project_id: created.project.project_id.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(catalog::project_by_id(&state, &created.project.project_id).is_none());
    }

    #[tokio::test]
    async fn root_open_adds_a_socket_to_an_existing_project_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "P".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;
        let extra = dir.path().join("extra");

        let ctx = project_root_open(
            State(state.clone()),
            Json(OpenRootReq {
                project_id: created.project.project_id.clone(),
                name: "extra-view".into(),
                root: CreateProjectRoot::Create {
                    parent: dir.path().to_string_lossy().into_owned(),
                    name: "extra".into(),
                },
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(extra.is_dir());
        assert_eq!(ctx.project, "P");

        let after = catalog::project_by_id(&state, &created.project.project_id).unwrap();
        assert_eq!(after.roots.len(), 1);
    }

    #[tokio::test]
    async fn root_open_create_refuses_a_target_that_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "P".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;
        std::fs::create_dir_all(dir.path().join("already-here")).unwrap();

        let err = project_root_open(
            State(state.clone()),
            Json(OpenRootReq {
                project_id: created.project.project_id.clone(),
                name: "extra-view".into(),
                root: CreateProjectRoot::Create {
                    parent: dir.path().to_string_lossy().into_owned(),
                    name: "already-here".into(),
                },
                tools: None,
            }),
        )
        .await
        .unwrap_err();
        assert!(err.1.contains("already exists"), "{:?}", err);
    }

    /// 07d follow-up: the same containment guarantee `catalog::tests::
    /// create_new_project_root_never_exposes_a_sibling_file_in_the_parent`
    /// proves directly against `Jail`, proven here through the real HTTP
    /// handler + the exact `files_list_inner` function the client's file
    /// tree actually calls.
    #[tokio::test]
    async fn project_create_over_http_never_exposes_a_sibling_of_a_freshly_created_root() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let parent = dir.path().join("ws");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(parent.join("secret.txt"), "shh").unwrap();

        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "Contained".into(),
                root: Some(CreateProjectRoot::Create {
                    parent: parent.to_string_lossy().into_owned(),
                    name: "child".into(),
                }),
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;

        let entries =
            crate::routes::project::files_list_inner(&state, &created.socket.stream_id, ".")
                .unwrap();
        assert!(
            !entries.iter().any(|e| e.name == "secret.txt"),
            "{entries:?}"
        );
    }

    #[tokio::test]
    async fn socket_attach_and_detach_round_trip_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let socket = open_stream(&state, &open_req(&repo, "httpattach"))
            .unwrap()
            .context;
        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "Home".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;

        project_socket_attach(
            State(state.clone()),
            Json(AttachSocketReq {
                stream_id: socket.stream_id.clone(),
                project_id: created.project.project_id.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        let heart = state.heart.lock().unwrap();
        let (pid, _) = crate::state::socket_project(&heart, &socket.stream_id);
        assert_eq!(pid.as_deref(), Some(created.project.project_id.as_str()));
        drop(heart);

        project_socket_detach(
            State(state.clone()),
            Json(DetachSocketReq {
                stream_id: socket.stream_id.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        let heart = state.heart.lock().unwrap();
        let (pid, _) = crate::state::socket_project(&heart, &socket.stream_id);
        assert_eq!(pid, None);
    }

    #[tokio::test]
    async fn attaching_a_chat_only_socket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let chat_only = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "Chatty".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;
        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "Target".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;

        let err = project_socket_attach(
            State(state.clone()),
            Json(AttachSocketReq {
                stream_id: chat_only.socket.stream_id,
                project_id: created.project.project_id,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("chat-only"), "got: {}", err.1);
    }

    #[tokio::test]
    async fn open_git_attach_links_a_project_via_project_open() {
        use crate::routes::project::project_open;

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let created = project_create(
            State(state.clone()),
            Json(CreateProjectReq {
                name: "GitHome".into(),
                root: None,
                stacks: Default::default(),
                tools: None,
            }),
        )
        .await
        .unwrap()
        .0;

        let resp = project_open(
            State(state.clone()),
            Json(OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                name: Some("gitlinked".into()),
                project_id: Some(created.project.project_id.clone()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .0;

        let heart = state.heart.lock().unwrap();
        let (pid, rid) = crate::state::socket_project(&heart, &resp.context.stream_id);
        assert_eq!(pid.as_deref(), Some(created.project.project_id.as_str()));
        assert!(rid.is_some());
        drop(heart);

        let after = catalog::project_by_id(&state, &created.project.project_id).unwrap();
        assert_eq!(after.roots.len(), 1);
        assert!(after.roots[0].is_git);
    }

    #[tokio::test]
    async fn open_git_attach_to_an_unknown_project_fails_the_whole_call() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let err = crate::routes::project::project_open(
            State(state.clone()),
            Json(OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                name: Some("orphaned-open".into()),
                project_id: Some("project_doesnotexist".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
}
