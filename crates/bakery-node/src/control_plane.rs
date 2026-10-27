//! A project's control plane, read from its own files whenever it's asked
//! (the panel opening, or its refresh): per folder, what it's built with,
//! its direct dependencies and the command-line tools it relies on; the MCP
//! servers it defines, plus the Bakery's own; its skills, once each, with
//! the skill folders that hold them; its instruction files; and the
//! Bakery's own setup for it, with what its files suggest. Every item names
//! the file it came from, so adding one later writes the same kind of file.
//! [`scan`] reads any folder, a project's or not (onboarding a machine's
//! projects starts from it). Nothing here writes into a folder, and secret
//! files are never opened.

use crate::node::ProjectStacks;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How deep below a folder's root a file is still read
/// (`app/frontend/pubspec.yaml` is two folders down).
const MAX_DEPTH: usize = 3;
/// A file bigger than this isn't read.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// A skill's description, at most.
const MAX_DESCRIPTION_CHARS: usize = 300;
/// Never walked into: heavy, or someone else's.
const SKIPPED: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    ".git",
    ".venv",
    "venv",
    ".venvs",
    "__pycache__",
    ".dart_tool",
    "Pods",
    "vendor",
    ".gradle",
    ".next",
    ".svelte-kit",
    ".turbo",
    "coverage",
    ".worktrees",
];
/// The only hidden folders walked into: where projects keep CI, platform
/// and toolchain files.
const HIDDEN_WALKED: &[&str] = &[".github", ".devcontainer", ".moon"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Language,
    Framework,
    Platform,
    Toolchain,
    Ci,
    Check,
}

/// One thing a folder is built with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub(crate) struct StackItem {
    pub(crate) kind: Kind,
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    /// The file that says so, relative to its folder.
    pub(crate) source: String,
}

/// One direct dependency, as its manifest names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    /// For development only (tests, builds, linting).
    pub(crate) dev: bool,
    pub(crate) source: String,
}

/// A command-line tool a folder relies on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub(crate) struct Tool {
    pub(crate) name: String,
    pub(crate) source: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Folder {
    /// The folder, one of the project's roots.
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) stack: Vec<StackItem>,
    pub(crate) dependencies: Vec<Dependency>,
    pub(crate) tools: Vec<Tool>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct McpServer {
    pub(crate) name: String,
    /// How it runs: a command and its arguments, or a URL.
    pub(crate) command: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) url: Option<String>,
    /// The names its environment and its headers set; never their values.
    pub(crate) env: Vec<String>,
    pub(crate) headers: Vec<String>,
    /// The file that defines it, relative to `folder`; none for the
    /// Bakery's own, which every Bakery agent gets.
    pub(crate) source: Option<String>,
    pub(crate) folder: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct Skill {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// Each copy's `SKILL.md`, with its folder.
    pub(crate) files: Vec<String>,
    /// The skill folders that hold it: `.claude/skills` (Claude reads it),
    /// `.agents/skills` (other agents do).
    pub(crate) dirs: Vec<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Instruction {
    pub(crate) folder: String,
    /// `AGENTS.md`, `CLAUDE.md` or `GEMINI.md`.
    pub(crate) file: String,
    pub(crate) bytes: u64,
}

/// A setting a folder's files point to, different from the current one.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct Suggestion {
    /// `toolchain`, `tasks`, `containers`, `secrets` or `sandbox`.
    pub(crate) field: String,
    pub(crate) value: String,
    /// The file that suggests it, with its folder.
    pub(crate) because: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Setup {
    pub(crate) current: ProjectStacks,
    pub(crate) suggestions: Vec<Suggestion>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ControlPlane {
    pub(crate) project_id: String,
    pub(crate) name: String,
    pub(crate) folders: Vec<Folder>,
    pub(crate) mcp: Vec<McpServer>,
    pub(crate) skills: Vec<Skill>,
    pub(crate) instructions: Vec<Instruction>,
    /// The Bakery's own setup for the project, which the operator may change.
    pub(crate) setup: Setup,
    /// What its own toolchain copies hold that the operator's don't (a Mac
    /// sandbox project's), each of which the operator may keep for good.
    pub(crate) additions: Vec<crate::mac_sandbox::Addition>,
    /// Every install its agents asked for, with the operator's answer and
    /// how it went.
    pub(crate) installs: Vec<crate::mac_sandbox::Install>,
    /// The secret-named files the operator let its builds read, or not, and
    /// the ones its rooms were refused that wait on them.
    pub(crate) secret_files: Option<crate::secret_reads::SecretFiles>,
    /// Its own services (its setup names them), as they run.
    pub(crate) services: Vec<crate::services::ServiceRunning>,
    /// The separate copies its threads started for destructive work.
    pub(crate) service_copies: Vec<crate::services::ServiceCopy>,
    /// The folders beyond its own its agents reach, which the operator adds
    /// (Settings → Machine access, for this project); none at first.
    pub(crate) extra_folders: Vec<crate::machine_access::Grant>,
}

/// One folder read: everything [`ControlPlane`] holds of it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Scan {
    pub(crate) folder: Folder,
    pub(crate) mcp: Vec<McpServer>,
    pub(crate) skills: Vec<Skill>,
    pub(crate) instructions: Vec<Instruction>,
    /// Everything its files point to, the current setup's own values too:
    /// a project's first folder to point a field one way settles it.
    #[serde(skip)]
    wanted: Vec<Suggestion>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ControlQuery {
    /// The project; "" for the Bakery's own, read from its main checkout
    /// and the worktrees its own conversations work in.
    pub(crate) project_id: String,
}

/// `GET /project/control`: a project's control plane, read from its files
/// now.
pub(crate) async fn control(
    State(st): State<AppState>,
    Query(q): Query<ControlQuery>,
) -> Result<Json<ControlPlane>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || detect(&st, &q.project_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct SetupReq {
    /// The project; "" for the Bakery's own, which its own conversations
    /// follow (where its agents run stays Machine access's).
    pub(crate) project_id: String,
    pub(crate) setup: ProjectStacks,
}

/// `POST /project/setup`: the operator's choice of the Bakery's own setup
/// for a project; each agent's next turn uses it.
pub(crate) async fn set_setup(
    State(st): State<AppState>,
    Json(req): Json<SetupReq>,
) -> Result<Json<ProjectStacks>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        crate::catalog::set_project_stacks(&st, &req.project_id, req.setup.clone())
            .map(|()| req.setup)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// A project's control plane (or the Bakery's own, for ""): each of its
/// folders read, a skill in more than one counted once.
pub(crate) fn detect(state: &AppState, project_id: &str) -> Result<ControlPlane, String> {
    let project = crate::catalog::project_or_bakery(state, project_id)
        .ok_or_else(|| format!("unknown project {project_id}"))?;
    let mut plane = ControlPlane {
        project_id: project.project_id.clone(),
        name: project.name.clone(),
        folders: Vec::new(),
        mcp: vec![bakery_mcp()],
        skills: Vec::new(),
        instructions: Vec::new(),
        setup: Setup {
            current: project.stacks.clone(),
            suggestions: Vec::new(),
        },
        additions: crate::mac_sandbox::additions(state, &project.project_id),
        installs: crate::mac_sandbox::installs(state, &project.project_id),
        secret_files: crate::secret_reads::files(state, &project.project_id).ok(),
        services: crate::services::status(state, &project.project_id),
        service_copies: crate::services::project_copies(state, &project.project_id),
        extra_folders: state
            .node
            .lock()
            .unwrap()
            .machine_access
            .projects
            .get(&project.project_id)
            .map(|own| own.grants.clone())
            .unwrap_or_default(),
    };
    let mut skills: BTreeMap<String, Skill> = BTreeMap::new();
    for root in &project.roots {
        let dir = PathBuf::from(&root.path);
        if !dir.is_dir() {
            continue;
        }
        let scanned = scan(&dir);
        plane.folders.push(scanned.folder);
        plane.mcp.extend(scanned.mcp);
        for skill in scanned.skills {
            match skills.get_mut(&skill.name) {
                Some(seen) => {
                    seen.files.extend(skill.files);
                    seen.dirs.extend(skill.dirs);
                    seen.dirs.sort();
                    seen.dirs.dedup();
                    seen.description = seen.description.take().or(skill.description);
                }
                None => {
                    skills.insert(skill.name.clone(), skill);
                }
            }
        }
        plane.instructions.extend(scanned.instructions);
        for suggestion in scanned.wanted {
            if !plane
                .setup
                .suggestions
                .iter()
                .any(|s| s.field == suggestion.field)
            {
                plane.setup.suggestions.push(suggestion);
            }
        }
    }
    let current = plane.setup.current.clone();
    // Where the Bakery's own agents run is Machine access's, never a setup's.
    let own = project_id.is_empty();
    plane
        .setup
        .suggestions
        .retain(|s| differs(&current, s) && !(own && s.field == "sandbox"));
    plane.skills = skills.into_values().collect();
    Ok(plane)
}

/// Any one folder, read.
pub(crate) fn scan(dir: &Path) -> Scan {
    let files = files_of(dir);
    let mut skills = skills_in(dir);
    for skill in &mut skills {
        skill.dirs.sort();
    }
    let wanted = wanted(dir, &files);
    Scan {
        folder: folder(dir, &files),
        mcp: mcp_servers(dir),
        skills,
        instructions: instructions_in(dir),
        wanted,
    }
}

/// What one folder is built with, alone: what the network list reads to
/// know the hosts a project's files point to.
pub(crate) fn stack_of(dir: &Path) -> Vec<StackItem> {
    folder(dir, &files_of(dir)).stack
}

/// The Bakery's own MCP server, which every Bakery agent gets.
fn bakery_mcp() -> McpServer {
    McpServer {
        name: "bakery".to_string(),
        command: None,
        args: Vec::new(),
        url: None,
        env: Vec::new(),
        headers: Vec::new(),
        source: None,
        folder: None,
    }
}

/// A folder's files, as paths from it, down to [`MAX_DEPTH`]: never into
/// heavy folders, never into hidden ones but where projects keep CI and
/// platform files, never a secret file.
fn files_of(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let hidden = name.starts_with('.') && !HIDDEN_WALKED.contains(&name.as_str());
                if depth < MAX_DEPTH && !hidden && !SKIPPED.contains(&name.as_str()) {
                    walk(root, &path, depth + 1, out);
                }
            } else if kind.is_file() && !bakery_exec::is_credential_filename(&path) {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out
}

pub(crate) fn read(path: &Path) -> Option<String> {
    let small = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES);
    if !small || bakery_exec::is_credential_filename(path) {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// What a folder is built with, its direct dependencies and its tools.
fn folder(dir: &Path, files: &[String]) -> Folder {
    let mut found = Found::default();
    for rel in files {
        let path = dir.join(rel);
        let name = file_name(rel);
        match name {
            "package.json" => found.package_json(dir, rel, read(&path)),
            "pubspec.yaml" => found.pubspec(rel, read(&path)),
            "Cargo.toml" => found.cargo(rel, read(&path)),
            "pyproject.toml" => found.pyproject(rel, read(&path)),
            "go.mod" => found.go_mod(rel, read(&path)),
            "Gemfile" => found.item(Kind::Language, "Ruby", None, rel),
            "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                found.item(Kind::Language, "Java", None, rel)
            }
            "Package.swift" => found.item(Kind::Language, "Swift", None, rel),
            "Podfile" => found.with_tool(Kind::Platform, "CocoaPods", "pod", rel),
            "composer.json" => found.item(Kind::Language, "PHP", None, rel),
            "deno.json" | "deno.jsonc" => found.item(Kind::Language, "TypeScript", None, rel),
            "buf.yaml" => found.with_tool(Kind::Language, "Protocol Buffers", "buf", rel),
            "firebase.json" => found.with_tool(Kind::Platform, "Firebase", "firebase", rel),
            "firestore.rules" => found.item(Kind::Platform, "Firestore", None, rel),
            "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" => {
                found.with_tool(Kind::Platform, "Docker Compose", "docker", rel)
            }
            "Dockerfile" => found.with_tool(Kind::Platform, "Docker", "docker", rel),
            "vercel.json" => found.with_tool(Kind::Platform, "Vercel", "vercel", rel),
            "netlify.toml" => found.with_tool(Kind::Platform, "Netlify", "netlify", rel),
            "fly.toml" => found.with_tool(Kind::Platform, "Fly.io", "flyctl", rel),
            "wrangler.toml" | "wrangler.jsonc" | "wrangler.json" => {
                found.with_tool(Kind::Platform, "Cloudflare Workers", "wrangler", rel)
            }
            "flake.nix" => found.with_tool(Kind::Platform, "Nix", "nix", rel),
            "devcontainer.json" => found.item(Kind::Platform, "Dev Containers", None, rel),
            "mise.toml" | ".mise.toml" => found.mise(rel, read(&path)),
            ".tool-versions" => found.tool_versions(rel, read(&path)),
            ".nvmrc" | ".node-version" => found.version_file("Node.js", rel, read(&path)),
            ".python-version" => found.version_file("Python", rel, read(&path)),
            "rust-toolchain" => found.version_file("Rust", rel, read(&path)),
            "rust-toolchain.toml" => found.rust_toolchain(rel, read(&path)),
            ".fvmrc" => found.fvmrc(rel, read(&path)),
            "moon.yml" => found.tool("moon", rel),
            "workspace.yml" if rel.starts_with(".moon/") => found.tool("moon", rel),
            "Makefile" => found.tool("make", rel),
            "justfile" | "Justfile" => found.tool("just", rel),
            "Taskfile.yml" | "Taskfile.yaml" => found.tool("task", rel),
            ".gitlab-ci.yml" => found.item(Kind::Ci, "GitLab CI", None, rel),
            ".pre-commit-config.yaml" => {
                found.with_tool(Kind::Check, "pre-commit", "pre-commit", rel)
            }
            "ruff.toml" | ".ruff.toml" => found.with_tool(Kind::Check, "Ruff", "ruff", rel),
            "biome.json" | "biome.jsonc" => found.with_tool(Kind::Check, "Biome", "biome", rel),
            "analysis_options.yaml" => found.item(Kind::Check, "Dart analyzer", None, rel),
            "rustfmt.toml" | ".rustfmt.toml" => found.item(Kind::Check, "rustfmt", None, rel),
            "clippy.toml" => found.item(Kind::Check, "Clippy", None, rel),
            "mypy.ini" => found.item(Kind::Check, "mypy", None, rel),
            ".golangci.yml" | ".golangci.yaml" => {
                found.item(Kind::Check, "golangci-lint", None, rel)
            }
            "pnpm-lock.yaml" => found.tool("pnpm", rel),
            "package-lock.json" => found.tool("npm", rel),
            "yarn.lock" => found.tool("yarn", rel),
            "bun.lockb" | "bun.lock" => found.tool("bun", rel),
            "uv.lock" => found.tool("uv", rel),
            "poetry.lock" => found.tool("poetry", rel),
            _ if name.starts_with("requirements") && name.ends_with(".txt") => {
                found.requirements(rel, read(&path))
            }
            _ if name.ends_with("_requirements.txt") || name.ends_with("-requirements.txt") => {
                found.requirements(rel, read(&path))
            }
            _ if name.starts_with("eslint.config.") || name.starts_with(".eslintrc") => {
                found.item(Kind::Check, "ESLint", None, rel)
            }
            _ if name.starts_with(".prettierrc") || name.starts_with("prettier.config.") => {
                found.item(Kind::Check, "Prettier", None, rel)
            }
            _ if rel.starts_with(".github/workflows/")
                && (name.ends_with(".yml") || name.ends_with(".yaml")) =>
            {
                found.item(Kind::Ci, "GitHub Actions", None, rel)
            }
            "config.toml" if rel.starts_with("supabase/") => {
                found.with_tool(Kind::Platform, "Supabase", "supabase", rel)
            }
            _ => {}
        }
    }
    Folder {
        path: dir.display().to_string(),
        name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        stack: found.stack,
        dependencies: found.dependencies,
        tools: found.tools,
    }
}

/// Frameworks, by the dependency that brings them in.
const FRAMEWORKS: &[(&str, &str)] = &[
    ("react", "React"),
    ("next", "Next.js"),
    ("svelte", "Svelte"),
    ("@sveltejs/kit", "SvelteKit"),
    ("vue", "Vue"),
    ("nuxt", "Nuxt"),
    ("@angular/core", "Angular"),
    ("solid-js", "Solid"),
    ("astro", "Astro"),
    ("vite", "Vite"),
    ("electron", "Electron"),
    ("@tauri-apps/api", "Tauri"),
    ("express", "Express"),
    ("fastify", "Fastify"),
    ("@nestjs/core", "NestJS"),
    ("hono", "Hono"),
    ("react-native", "React Native"),
    ("expo", "Expo"),
    ("firebase-functions", "Cloud Functions for Firebase"),
    ("tailwindcss", "Tailwind CSS"),
    ("tauri", "Tauri"),
    ("axum", "axum"),
    ("actix-web", "Actix Web"),
    ("rocket", "Rocket"),
    ("leptos", "Leptos"),
    ("bevy", "Bevy"),
    ("dioxus", "Dioxus"),
    ("django", "Django"),
    ("fastapi", "FastAPI"),
    ("flask", "Flask"),
    ("streamlit", "Streamlit"),
    ("github.com/gin-gonic/gin", "Gin"),
    ("github.com/labstack/echo/v4", "Echo"),
    ("github.com/gofiber/fiber/v2", "Fiber"),
];

#[derive(Default)]
struct Found {
    stack: Vec<StackItem>,
    dependencies: Vec<Dependency>,
    tools: Vec<Tool>,
}

impl Found {
    fn item(&mut self, kind: Kind, name: &str, version: Option<String>, source: &str) {
        if !self.stack.iter().any(|i| i.kind == kind && i.name == name) {
            self.stack.push(StackItem {
                kind,
                name: name.to_string(),
                version,
                source: source.to_string(),
            });
        }
    }

    fn tool(&mut self, name: &str, source: &str) {
        if !self.tools.iter().any(|t| t.name == name) {
            self.tools.push(Tool {
                name: name.to_string(),
                source: source.to_string(),
            });
        }
    }

    fn with_tool(&mut self, kind: Kind, name: &str, tool: &str, source: &str) {
        self.item(kind, name, None, source);
        self.tool(tool, source);
    }

    fn dependency(&mut self, name: &str, version: Option<String>, dev: bool, source: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some((_, framework)) = FRAMEWORKS.iter().find(|(dep, _)| *dep == name) {
            self.item(Kind::Framework, framework, None, source);
        }
        let version = version.filter(|v| !v.is_empty());
        // Once, though a workspace's manifests name it again: as the first
        // names it, with a later one's version when it gave none.
        match self.dependencies.iter_mut().find(|d| d.name == name && d.dev == dev) {
            Some(seen) => seen.version = seen.version.take().or(version),
            None => self.dependencies.push(Dependency {
                name: name.to_string(),
                version,
                dev,
                source: source.to_string(),
            }),
        }
    }

    fn package_json(&mut self, dir: &Path, rel: &str, text: Option<String>) {
        let Some(json) = text.and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            return;
        };
        let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
        let typescript = dir.join(parent).join("tsconfig.json").is_file()
            || ["dependencies", "devDependencies"]
                .iter()
                .any(|k| json[k].get("typescript").is_some());
        let language = if typescript {
            "TypeScript"
        } else {
            "JavaScript"
        };
        self.item(Kind::Language, language, None, rel);
        for (key, dev) in [("dependencies", false), ("devDependencies", true)] {
            for (name, version) in json[key].as_object().into_iter().flatten() {
                self.dependency(name, version.as_str().map(str::to_string), dev, rel);
            }
        }
        if let Some(node) = json["engines"]["node"].as_str() {
            self.item(Kind::Toolchain, "Node.js", Some(node.to_string()), rel);
        }
        if let Some(manager) = json["packageManager"].as_str() {
            let (name, _) = manager.split_once('@').unwrap_or((manager, ""));
            self.tool(name, rel);
        }
    }

    /// pubspec.yaml's `dependencies` and `dev_dependencies`, read line by
    /// line (entries two spaces in; a nested entry is an SDK or a path).
    fn pubspec(&mut self, rel: &str, text: Option<String>) {
        let Some(text) = text else {
            return;
        };
        let mut block: Option<bool> = None;
        let mut flutter = false;
        for line in text.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            if !line.starts_with(' ') {
                block = match line.trim_end().trim_end_matches(':') {
                    "dependencies" => Some(false),
                    "dev_dependencies" => Some(true),
                    _ => None,
                };
                continue;
            }
            let Some(dev) = block else {
                continue;
            };
            if line.trim() == "sdk: flutter" {
                flutter = true;
            }
            let entry = line.strip_prefix("  ").filter(|l| !l.starts_with(' '));
            if let Some((name, version)) = entry.and_then(|l| l.split_once(':')) {
                let version = version.trim().trim_matches(['"', '\'']);
                self.dependency(name, Some(version.to_string()), dev, rel);
            }
        }
        self.item(Kind::Language, "Dart", None, rel);
        if flutter {
            self.item(Kind::Framework, "Flutter", None, rel);
            self.tool("flutter", rel);
        } else {
            self.tool("dart", rel);
        }
    }

    fn cargo(&mut self, rel: &str, text: Option<String>) {
        self.item(Kind::Language, "Rust", None, rel);
        self.tool("cargo", rel);
        let Some(toml) = text.and_then(|t| t.parse::<toml::Table>().ok()) else {
            return;
        };
        let tables = [
            (toml.get("dependencies"), false),
            (toml.get("dev-dependencies"), true),
            (toml.get("build-dependencies"), true),
            (
                toml.get("workspace").and_then(|w| w.get("dependencies")),
                false,
            ),
        ];
        for (table, dev) in tables {
            for (name, spec) in table.and_then(|t| t.as_table()).into_iter().flatten() {
                let version = spec
                    .as_str()
                    .or_else(|| spec.get("version").and_then(|v| v.as_str()))
                    .map(str::to_string);
                self.dependency(name, version, dev, rel);
            }
        }
    }

    fn pyproject(&mut self, rel: &str, text: Option<String>) {
        self.item(Kind::Language, "Python", None, rel);
        let Some(toml) = text.and_then(|t| t.parse::<toml::Table>().ok()) else {
            return;
        };
        let strings = |v: Option<&toml::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        };
        let project = toml.get("project");
        for spec in strings(project.and_then(|p| p.get("dependencies"))) {
            self.pep508(&spec, false, rel);
        }
        let optional = project.and_then(|p| p.get("optional-dependencies"));
        for (group, specs) in optional.and_then(|o| o.as_table()).into_iter().flatten() {
            let dev = matches!(group.as_str(), "dev" | "test" | "tests" | "lint" | "docs");
            for spec in strings(Some(specs)) {
                self.pep508(&spec, dev, rel);
            }
        }
        let groups = toml.get("dependency-groups").and_then(|g| g.as_table());
        for (_, specs) in groups.into_iter().flatten() {
            for spec in strings(Some(specs)) {
                self.pep508(&spec, true, rel);
            }
        }
        let poetry = toml.get("tool").and_then(|t| t.get("poetry"));
        if poetry.is_some() {
            self.tool("poetry", rel);
        }
        let poetry_deps = poetry
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_table());
        for (name, spec) in poetry_deps.into_iter().flatten() {
            if name != "python" {
                self.dependency(name, spec.as_str().map(str::to_string), false, rel);
            }
        }
        if toml.get("tool").and_then(|t| t.get("ruff")).is_some() {
            self.with_tool(Kind::Check, "Ruff", "ruff", rel);
        }
    }

    /// One `name[extras] spec ; marker` requirement.
    fn pep508(&mut self, spec: &str, dev: bool, rel: &str) {
        let spec = spec.split(';').next().unwrap_or("").trim();
        let end = spec
            .find(|c: char| !(c.is_alphanumeric() || "-_.".contains(c)))
            .unwrap_or(spec.len());
        let (name, rest) = spec.split_at(end);
        let rest = rest.trim();
        let rest = match rest.strip_prefix('[') {
            Some(r) => r.split_once(']').map_or("", |(_, after)| after.trim()),
            None => rest,
        };
        self.dependency(name, Some(rest.to_string()), dev, rel);
    }

    fn requirements(&mut self, rel: &str, text: Option<String>) {
        self.item(Kind::Language, "Python", None, rel);
        self.tool("pip", rel);
        let dev = ["dev", "test", "lint"].iter().any(|w| rel.contains(w));
        for line in text.unwrap_or_default().lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('-') {
                continue;
            }
            self.pep508(line, dev, rel);
        }
    }

    fn go_mod(&mut self, rel: &str, text: Option<String>) {
        self.item(Kind::Language, "Go", None, rel);
        self.tool("go", rel);
        let mut in_block = false;
        for line in text.unwrap_or_default().lines() {
            let line = line.trim();
            if let Some(version) = line.strip_prefix("go ") {
                self.item(Kind::Toolchain, "Go", Some(version.trim().to_string()), rel);
            } else if line == "require (" {
                in_block = true;
            } else if in_block && line == ")" {
                in_block = false;
            } else if !line.contains("// indirect") {
                let entry = if in_block {
                    Some(line)
                } else {
                    line.strip_prefix("require ")
                };
                let mut parts = entry.unwrap_or("").split_whitespace();
                if let (Some(path), Some(version)) = (parts.next(), parts.next()) {
                    self.dependency(path, Some(version.to_string()), false, rel);
                }
            }
        }
    }

    fn mise(&mut self, rel: &str, text: Option<String>) {
        self.tool("mise", rel);
        let Some(toml) = text.and_then(|t| t.parse::<toml::Table>().ok()) else {
            return;
        };
        let tools = toml.get("tools").and_then(|t| t.as_table());
        for (name, version) in tools.into_iter().flatten() {
            let version = version
                .as_str()
                .map(str::to_string)
                .or_else(|| version.as_integer().map(|v| v.to_string()))
                .or_else(|| {
                    version
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
            self.item(Kind::Toolchain, name, version, rel);
        }
    }

    fn tool_versions(&mut self, rel: &str, text: Option<String>) {
        for line in text.unwrap_or_default().lines() {
            let mut parts = line.split('#').next().unwrap_or("").split_whitespace();
            if let (Some(name), Some(version)) = (parts.next(), parts.next()) {
                self.item(Kind::Toolchain, name, Some(version.to_string()), rel);
            }
        }
    }

    fn version_file(&mut self, name: &str, rel: &str, text: Option<String>) {
        let version = text
            .and_then(|t| t.lines().next().map(|l| l.trim().to_string()))
            .filter(|v| !v.is_empty());
        self.item(Kind::Toolchain, name, version, rel);
    }

    fn rust_toolchain(&mut self, rel: &str, text: Option<String>) {
        let channel = text
            .and_then(|t| t.parse::<toml::Table>().ok())
            .and_then(|t| {
                t.get("toolchain")
                    .and_then(|c| c.get("channel"))
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            });
        self.item(Kind::Toolchain, "Rust", channel, rel);
    }

    fn fvmrc(&mut self, rel: &str, text: Option<String>) {
        let version = text
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|j| j["flutter"].as_str().map(str::to_string));
        self.item(Kind::Toolchain, "Flutter", version, rel);
    }
}

/// The MCP servers a folder's own files define: `.mcp.json`,
/// `.cursor/mcp.json`, `.vscode/mcp.json`, `.gemini/settings.json` and
/// `.codex/config.toml`. Never a value of their environment or headers.
fn mcp_servers(dir: &Path) -> Vec<McpServer> {
    let folder = dir.display().to_string();
    let keys = |v: Option<&Value>| -> Vec<String> {
        v.and_then(|m| m.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (rel, key) in [
        (".mcp.json", "mcpServers"),
        (".cursor/mcp.json", "mcpServers"),
        (".gemini/settings.json", "mcpServers"),
        (".vscode/mcp.json", "servers"),
    ] {
        let json = read(&dir.join(rel)).and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let Some(json) = json else {
            continue;
        };
        for (name, server) in json[key].as_object().into_iter().flatten() {
            let text = |k: &str| server.get(k).and_then(|v| v.as_str()).map(str::to_string);
            out.push(McpServer {
                name: name.clone(),
                command: text("command"),
                args: server["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect(),
                url: text("url").or_else(|| text("httpUrl")),
                env: keys(server.get("env")),
                headers: keys(server.get("headers")),
                source: Some(rel.to_string()),
                folder: Some(folder.clone()),
            });
        }
    }
    let codex = read(&dir.join(".codex/config.toml")).and_then(|t| t.parse::<toml::Table>().ok());
    let servers = codex
        .as_ref()
        .and_then(|t| t.get("mcp_servers"))
        .and_then(|s| s.as_table());
    for (name, server) in servers.into_iter().flatten() {
        let text = |k: &str| server.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let names = |k: &str| {
            server
                .get(k)
                .and_then(|v| v.as_table())
                .map(|t| t.keys().cloned().collect())
                .unwrap_or_default()
        };
        out.push(McpServer {
            name: name.clone(),
            command: text("command"),
            args: server
                .get("args")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect(),
            url: text("url"),
            env: names("env"),
            headers: names("http_headers"),
            source: Some(".codex/config.toml".to_string()),
            folder: Some(folder.clone()),
        });
    }
    out
}

/// A folder's skills: each `SKILL.md` under `.claude/skills` and
/// `.agents/skills`, named by its frontmatter (or its folder).
fn skills_in(dir: &Path) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for skills_dir in [".claude/skills", ".agents/skills"] {
        let Ok(entries) = std::fs::read_dir(dir.join(skills_dir)) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let folder_name = entry.file_name().to_string_lossy().into_owned();
            let rel = format!("{skills_dir}/{folder_name}/SKILL.md");
            let Some(text) = read(&dir.join(&rel)) else {
                continue;
            };
            let (name, description) = frontmatter(&text);
            let name = name.unwrap_or(folder_name);
            let file = format!("{}/{rel}", dir.display());
            match out.iter_mut().find(|s| s.name == name) {
                Some(seen) => {
                    seen.files.push(file);
                    seen.dirs.push(skills_dir.to_string());
                    seen.description = seen.description.take().or(description);
                }
                None => out.push(Skill {
                    name,
                    description,
                    files: vec![file],
                    dirs: vec![skills_dir.to_string()],
                }),
            }
        }
    }
    out
}

/// A skill's `name` and `description` from its frontmatter, a folded
/// (`>`) or literal (`|`) description included.
pub(crate) fn frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let unquote = |v: &str| v.trim().trim_matches(['"', '\'']).to_string();
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut block: Option<String> = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(text) = block.as_mut() {
            if line.starts_with([' ', '\t']) {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(line.trim());
                continue;
            }
            description = block.take();
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(unquote(v)).filter(|n| !n.is_empty());
        } else if let Some(v) = line.strip_prefix("description:") {
            match v.trim() {
                ">" | "|" | ">-" | "|-" => block = Some(String::new()),
                v => description = Some(unquote(v)),
            }
        }
    }
    let description = block
        .or(description)
        .filter(|d| !d.is_empty())
        .map(|d| d.chars().take(MAX_DESCRIPTION_CHARS).collect());
    (name, description)
}

fn instructions_in(dir: &Path) -> Vec<Instruction> {
    ["AGENTS.md", "CLAUDE.md", "GEMINI.md"]
        .iter()
        .filter_map(|file| {
            let meta = std::fs::metadata(dir.join(file))
                .ok()
                .filter(|m| m.is_file())?;
            Some(Instruction {
                folder: dir.display().to_string(),
                file: file.to_string(),
                bytes: meta.len(),
            })
        })
        .collect()
}

/// Whether a suggestion would change the current setup.
fn differs(current: &ProjectStacks, s: &Suggestion) -> bool {
    let now = match s.field.as_str() {
        "toolchain" => &current.toolchain,
        "tasks" => &current.tasks,
        "containers" => &current.containers,
        "sandbox" => &current.sandbox,
        _ => &current.secrets,
    };
    *now != s.value
}

/// What a folder's files point to for the Bakery's own setup, the current
/// setup's own values too.
fn wanted(dir: &Path, files: &[String]) -> Vec<Suggestion> {
    let has = |names: &[&str]| files.iter().find(|f| names.contains(&f.as_str())).cloned();
    let containers = files.iter().find(|f| {
        matches!(
            file_name(f),
            "docker-compose.yml"
                | "docker-compose.yaml"
                | "compose.yml"
                | "compose.yaml"
                | "Dockerfile"
        )
    });
    let flutter = files
        .iter()
        .find(|f| {
            file_name(f) == "pubspec.yaml"
                && read(&dir.join(f)).is_some_and(|t| t.lines().any(|l| l.trim() == "sdk: flutter"))
        })
        .cloned();
    let apple = files
        .iter()
        .find(|f| {
            matches!(file_name(f), "Podfile" | "Package.swift")
                || f.ends_with(".xcodeproj/project.pbxproj")
        })
        .cloned()
        .or(flutter);
    let wanted = [
        (
            "toolchain",
            "mise",
            has(&["mise.toml", ".mise.toml", ".tool-versions"]),
        ),
        ("toolchain", "proto", has(&[".prototools"])),
        ("tasks", "moon", has(&["moon.yml", ".moon/workspace.yml"])),
        ("containers", "colima", containers.cloned()),
        ("secrets", "infisical", has(&[".infisical.json"])),
        // Flutter and Apple apps (Xcode, CocoaPods, Swift packages) build on
        // this Mac, as the operator decided; anything else runs in a Linux
        // container.
        ("sandbox", "mac", apple.clone()),
        (
            "sandbox",
            "container",
            apple
                .is_none()
                .then(|| {
                    has(&[
                        "package.json",
                        "Cargo.toml",
                        "pyproject.toml",
                        "go.mod",
                        "requirements.txt",
                    ])
                })
                .flatten(),
        ),
    ];
    wanted
        .into_iter()
        .filter_map(|(field, value, file)| {
            Some(Suggestion {
                field: field.to_string(),
                value: value.to_string(),
                because: format!("{}/{}", dir.display(), file?),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn put(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A folder shaped like GreenLeaf's: a Flutter app with Firebase and its
    /// functions, a Python backend, CI and checks, skills in both skill
    /// folders, and MCP servers whose secrets never come back.
    #[test]
    fn a_project_shows_its_stack_tools_mcp_servers_skills_and_setup() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let app = work.path().join("app");
        put(&app, "frontend/pubspec.yaml", "name: app\ndependencies:\n  flutter:\n    sdk: flutter\n  provider: ^6.1.0\n  # a comment\ndev_dependencies:\n  flutter_test:\n    sdk: flutter\n  mocktail: 1.0.4\nflutter:\n  uses-material-design: true\n");
        put(
            &app,
            "functions/package.json",
            r#"{"dependencies": {"firebase-functions": "^6.0.0", "express": "4.21.0"}, "devDependencies": {"typescript": "^5.6.0"}, "engines": {"node": "22"}}"#,
        );
        put(&app, "package-lock.json", "{}");
        put(&app, "firebase.json", "{}");
        put(&app, "firestore.rules", "rules_version = '2';");
        put(&app, ".github/workflows/ci.yml", "on: push");
        put(&app, ".pre-commit-config.yaml", "repos: []");
        put(&app, "backend/webserver_requirements.txt", "fastapi==0.115.0\nuvicorn[standard]>=0.30 ; python_version > '3.9'\n# note\n-r base.txt\n");
        put(&app, "backend/ruff.toml", "line-length = 100");
        put(
            &app,
            "mise.toml",
            "[tools]\nnode = \"22\"\npython = \"3.12\"\n",
        );
        put(
            &app,
            "node_modules/x/package.json",
            r#"{"dependencies": {"left-pad": "1"}}"#,
        );
        put(&app, ".env", "SECRET=nope");
        put(
            &app,
            ".claude/skills/qa/SKILL.md",
            "---\nname: farm-qa\ndescription: >\n  Checks the app\n  end to end.\n---\nbody",
        );
        put(
            &app,
            ".agents/skills/qa/SKILL.md",
            "---\nname: farm-qa\ndescription: Checks the app end to end.\n---\n",
        );
        put(&app, ".claude/skills/smoke/SKILL.md", "no frontmatter");
        put(&app, "AGENTS.md", "# Agents\n");
        put(
            &app,
            ".mcp.json",
            r#"{"mcpServers": {"jira": {"command": "npx", "args": ["-y", "jira-mcp"], "env": {"JIRA_TOKEN": "s3cret-token"}}, "docs": {"type": "http", "url": "https://example.test/mcp", "headers": {"Authorization": "Bearer s3cret"}}}}"#,
        );
        put(&app, ".codex/config.toml", "[mcp_servers.files]\ncommand = \"mcp-files\"\nargs = [\"--root\", \".\"]\n[mcp_servers.files.env]\nAPI_KEY = \"s3cret-key\"\n");
        let lib = work.path().join("lib");
        put(&lib, "Cargo.toml", "[package]\nname = \"lib\"\n[dependencies]\naxum = \"0.8\"\ntokio = { version = \"1\", features = [\"full\"] }\nserde = { workspace = true }\n[dev-dependencies]\ntempfile = \"3\"\n[workspace.dependencies]\nserde = \"1\"\n");
        put(&lib, "crates/cli/Cargo.toml", "[package]\nname = \"cli\"\n[dependencies]\naxum = { workspace = true }\n[dev-dependencies]\ntempfile = \"3\"\n");
        put(&lib, "go.mod", "module x\n\ngo 1.23\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.10.0\n\tgolang.org/x/sys v0.20.0 // indirect\n)\n");

        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (project, _) = crate::catalog::create_project(
            &state,
            "farm",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                app.to_str().unwrap(),
            )),
        )
        .unwrap();
        crate::catalog::attach_directory_root(
            &state,
            &project.project_id,
            "lib",
            crate::catalog::DirectoryRootSpec::Attach(lib.to_str().unwrap()),
        )
        .unwrap();
        let plane = detect(&state, &project.project_id).unwrap();

        let app_folder = plane.folders.iter().find(|f| f.name == "app").unwrap();
        let named = |kind: Kind| -> Vec<&str> {
            app_folder
                .stack
                .iter()
                .filter(|i| i.kind == kind)
                .map(|i| i.name.as_str())
                .collect()
        };
        assert_eq!(named(Kind::Language), ["Python", "Dart", "TypeScript"]);
        assert_eq!(
            named(Kind::Framework),
            [
                "FastAPI",
                "Flutter",
                "Cloud Functions for Firebase",
                "Express"
            ]
        );
        assert_eq!(named(Kind::Platform), ["Firebase", "Firestore"]);
        assert_eq!(named(Kind::Ci), ["GitHub Actions"]);
        assert_eq!(named(Kind::Check), ["pre-commit", "Ruff"]);
        let node = app_folder
            .stack
            .iter()
            .find(|i| i.kind == Kind::Toolchain && i.name == "node")
            .unwrap();
        assert_eq!(
            (node.version.as_deref(), node.source.as_str()),
            (Some("22"), "mise.toml")
        );
        let tools: Vec<&str> = app_folder.tools.iter().map(|t| t.name.as_str()).collect();
        for tool in [
            "flutter",
            "firebase",
            "npm",
            "pre-commit",
            "ruff",
            "pip",
            "mise",
        ] {
            assert!(tools.contains(&tool), "{tool} in {tools:?}");
        }
        let dep = |name: &str| app_folder.dependencies.iter().find(|d| d.name == name);
        assert_eq!(dep("provider").unwrap().version.as_deref(), Some("^6.1.0"));
        assert!(dep("mocktail").unwrap().dev);
        assert_eq!(dep("uvicorn").unwrap().version.as_deref(), Some(">=0.30"));
        assert!(dep("typescript").unwrap().dev);
        assert!(dep("left-pad").is_none(), "never inside node_modules");
        assert!(
            dep("flutter").is_some_and(|d| d.version.is_none()),
            "an SDK entry"
        );

        let lib = plane.folders.iter().find(|f| f.name == "lib").unwrap();
        let lib_dep = |name: &str| lib.dependencies.iter().find(|d| d.name == name);
        assert_eq!(lib_dep("tokio").unwrap().version.as_deref(), Some("1"));
        assert!(lib_dep("tempfile").unwrap().dev);
        assert!(
            lib_dep("golang.org/x/sys").is_none(),
            "an indirect one isn't direct"
        );
        assert!(lib.stack.iter().any(|i| i.name == "axum"));
        assert!(lib.stack.iter().any(|i| i.name == "Gin"));
        let times = |name: &str| lib.dependencies.iter().filter(|d| d.name == name).count();
        assert_eq!((times("axum"), times("serde"), times("tempfile")), (1, 1, 1), "once each, named again or not");
        assert_eq!(lib_dep("axum").unwrap().source, "Cargo.toml", "as the first names it");
        assert_eq!(lib_dep("serde").unwrap().version.as_deref(), Some("1"), "the workspace's version");

        let mcp: Vec<&str> = plane.mcp.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            mcp,
            ["bakery", "jira", "docs", "files"],
            "in each file's own order"
        );
        let jira = plane.mcp.iter().find(|m| m.name == "jira").unwrap();
        assert_eq!(jira.command.as_deref(), Some("npx"));
        assert_eq!(jira.env, ["JIRA_TOKEN"]);
        assert_eq!(plane.mcp[3].env, ["API_KEY"]);
        let all = serde_json::to_string(&plane).unwrap();
        assert!(
            !all.contains("s3cret") && !all.contains("nope"),
            "no secret value, ever"
        );

        let skills: Vec<&str> = plane.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(skills, ["farm-qa", "smoke"]);
        let qa = &plane.skills[0];
        assert_eq!(qa.dirs, [".agents/skills", ".claude/skills"]);
        assert_eq!(
            qa.description.as_deref(),
            Some("Checks the app end to end.")
        );
        assert_eq!(plane.instructions.len(), 1);

        assert_eq!(plane.setup.current.toolchain, "none");
        let suggested: Vec<(&str, &str)> = plane
            .setup
            .suggestions
            .iter()
            .map(|s| (s.field.as_str(), s.value.as_str()))
            .collect();
        // Its Apple folder puts its agents in the Mac sandbox, where they are
        // already: no other folder of it suggests otherwise.
        assert_eq!(plane.setup.current.sandbox, "mac");
        assert_eq!(suggested, [("toolchain", "mise")]);
    }

    #[test]
    fn the_operator_changes_the_bakerys_own_setup_and_bad_choices_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (project, _) = crate::catalog::create_project(&state, "p", None).unwrap();
        let mut stacks = ProjectStacks::default();
        stacks.toolchain = "mise".into();
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks.clone()).unwrap();
        let record = crate::catalog::project_by_id(&state, &project.project_id).unwrap();
        assert_eq!(record.stacks, stacks);
        stacks.tasks = "make".into();
        assert!(crate::catalog::set_project_stacks(&state, &project.project_id, stacks).is_err());
        let unknown =
            crate::catalog::set_project_stacks(&state, "project:none", ProjectStacks::default());
        assert!(unknown.is_err());
    }

    /// The Bakery's own project (""): its main checkout, then the worktrees
    /// its own conversations work in, never a project's; its setup, which
    /// its own conversations follow, never a project's thread; and where its
    /// agents run stays Machine access's.
    #[tokio::test]
    async fn the_bakerys_own_plane_reads_its_checkouts_and_its_setup_holds_for_its_own() {
        use crate::testutil::*;
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        put(
            &repo,
            "Cargo.toml",
            "[package]\nname = \"bakery\"\n[dependencies]\naxum = \"0.8\"\n",
        );
        put(&repo, "mise.toml", "[tools]\nrust = \"1.98.0\"\n");
        put(&repo, "AGENTS.md", "# Agents\n");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = crate::testutil::state_with_bakery_root(dir.path(), &repo);
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None)
            .unwrap()
            .stream_id;
        let tidy = crate::workspaces::create_workspace(&state, "Tidy", None, "human").unwrap();
        let app = dir.path().join("app");
        put(&app, "pubspec.yaml", "name: app\n");
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (_, theirs) = crate::catalog::create_project(&state, "app", Some(root)).unwrap();

        let read = || {
            control(
                State(state.clone()),
                Query(ControlQuery {
                    project_id: String::new(),
                }),
            )
        };
        let plane = read().await.unwrap().0;
        assert_eq!(
            (plane.project_id.as_str(), plane.name.as_str()),
            ("", "Bakery")
        );
        let folders: Vec<&str> = plane.folders.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            folders,
            [repo.to_str().unwrap(), tidy.worktree_path.as_str()],
            "the Baker's own folder once, then its workspace's"
        );
        assert!(plane.folders[0].stack.iter().any(|i| i.name == "Rust"));
        assert_eq!(plane.instructions.len(), 2, "each folder's AGENTS.md");
        let suggested: Vec<(&str, &str)> = plane
            .setup
            .suggestions
            .iter()
            .map(|s| (s.field.as_str(), s.value.as_str()))
            .collect();
        assert_eq!(
            suggested,
            [("toolchain", "mise")],
            "never where its agents run"
        );

        let set = |setup: ProjectStacks| {
            set_setup(
                State(state.clone()),
                Json(SetupReq {
                    project_id: String::new(),
                    setup,
                }),
            )
        };
        let mise = ProjectStacks {
            toolchain: "mise".into(),
            ..ProjectStacks::default()
        };
        assert_eq!(set(mise.clone()).await.unwrap().0, mise);
        assert_eq!(read().await.unwrap().0.setup.current, mise);
        assert!(read().await.unwrap().0.setup.suggestions.is_empty());
        for own in [&baker, &tidy.stream_id] {
            assert_eq!(
                crate::catalog::stacks_for_socket(&state, own),
                mise,
                "{own} follows the Bakery's"
            );
            assert_eq!(
                crate::machine_access::reach_for(&state, own),
                crate::machine_access::Reach::HostFull,
                "the Bakery's own start on the whole Mac"
            );
        }
        assert_eq!(
            crate::catalog::stacks_for_socket(&state, &theirs.stream_id),
            ProjectStacks::default(),
            "a project's own"
        );
        let contained = ProjectStacks {
            sandbox: "container".into(),
            ..mise.clone()
        };
        assert_eq!(set(contained).await.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(
            crate::machine_access::reach_for(&state, &baker),
            crate::machine_access::Reach::HostFull
        );
    }
}
