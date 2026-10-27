//! The disk guard: where this Mac's disk goes for the Bakery (each thread's
//! build folders, the images, containers and volumes on Docker in Colima
//! and on Apple container, each told by the project that uses it, Colima's
//! own disk, the Bakery's own data), a warning to the app when free space
//! runs low, and cleanups the operator picks and confirms one at a time.
//! Each is of something that comes back on the next build, pull or start,
//! never a project's own files: a build folder only when the project's own
//! tools make it and git ignores it whole, holding nothing git tracks; never
//! the Bakery's own images; a volume only by its name, never a prune.

use crate::containers::Backend;
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type Fail = (StatusCode, String);

fn operator_only() -> Result<(), Fail> {
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "the disk guard is the operator's".to_string(),
        ));
    }
    Ok(())
}

/// The folders a project's tools build into, by the file that says the tool
/// is there, beside that file.
const BUILT_BY: &[(&str, &str, &[&str])] = &[
    ("Cargo.toml", "Cargo", &["target"]),
    ("pubspec.yaml", "Flutter", &["build", ".dart_tool"]),
    (
        "package.json",
        "npm",
        &["node_modules", ".next", ".svelte-kit", ".turbo", "dist"],
    ),
    ("build.gradle", "Gradle", &["build", ".gradle"]),
    ("build.gradle.kts", "Gradle", &["build", ".gradle"]),
    ("Package.swift", "SwiftPM", &[".build"]),
    ("Podfile", "CocoaPods", &["Pods"]),
];

#[derive(Debug, Clone, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct BuildFolder {
    /// A thread whose folder holds it, and its name.
    pub(crate) stream_id: String,
    pub(crate) thread: String,
    pub(crate) path: String,
    /// The tool that makes it: Cargo, Flutter, npm, Gradle, SwiftPM or
    /// CocoaPods.
    pub(crate) made_by: String,
    pub(crate) bytes: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct DiskUse {
    /// `images`, `containers`, `volumes` or `build cache`.
    pub(crate) kind: String,
    pub(crate) count: u64,
    /// In use by a container.
    pub(crate) active: u64,
    pub(crate) bytes: u64,
    /// What nothing uses.
    pub(crate) reclaimable: u64,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct EngineItem {
    /// `docker` (Colima's), `orbstack` or `apple`.
    pub(crate) engine: String,
    /// `image`, `container` or `volume`.
    pub(crate) kind: String,
    /// What the engine calls it: repository:tag, a container's name, a
    /// volume's name.
    pub(crate) name: String,
    /// What removes it: an image's or a container's id, a volume's name.
    pub(crate) id: String,
    pub(crate) bytes: Option<u64>,
    /// A container's state; for an image or a volume, `used` or `unused`.
    pub(crate) state: String,
    /// The Bakery project that uses it, `Bakery` for the Bakery's own; none
    /// when nothing tells.
    pub(crate) project: Option<String>,
    /// The compose or Supabase project its labels name.
    pub(crate) group: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct DataPart {
    pub(crate) name: String,
    pub(crate) bytes: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DiskReport {
    /// The disk the Bakery's data is on.
    pub(crate) free: u64,
    pub(crate) total: u64,
    /// Free space is under a tenth of the disk.
    pub(crate) low: bool,
    /// Largest first.
    pub(crate) build_folders: Vec<BuildFolder>,
    /// Docker in Colima's default VM, by kind; absent when it isn't running.
    pub(crate) docker: Option<Vec<DiskUse>>,
    /// Docker in OrbStack, by kind; absent when it isn't installed or
    /// running.
    pub(crate) orbstack: Option<Vec<DiskUse>>,
    /// Absent when Apple container isn't running.
    pub(crate) apple_container: Option<Vec<DiskUse>>,
    /// Every image, container and volume on both, with the project that
    /// uses each.
    pub(crate) items: Vec<EngineItem>,
    /// What Colima's VMs take on this Mac's disk; absent without Colima.
    pub(crate) colima: Option<u64>,
    /// The Bakery's own data, by its top folders, largest first.
    pub(crate) bakery: Vec<DataPart>,
}

/// Free and total bytes of the disk `path` is on, as an app may use it.
pub(crate) fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    Some((stats.f_bavail as u64 * block, stats.f_blocks as u64 * block))
}

// ponytail: one fixed line, a tenth of the disk; the operator's own number
// when they want one.
fn low(free: u64, total: u64) -> bool {
    free < total / 10
}

/// Each path's size on disk, as `du` counts it.
fn sizes(paths: &[PathBuf]) -> Vec<u64> {
    if paths.is_empty() {
        return Vec::new();
    }
    let out = Command::new("/usr/bin/du")
        .arg("-sk")
        .args(paths)
        .stdin(Stdio::null())
        .output();
    let text = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut by_path = HashMap::new();
    for line in text.lines() {
        if let Some((kib, path)) = line.split_once('\t') {
            by_path.insert(
                path.to_string(),
                kib.trim().parse::<u64>().unwrap_or(0) * 1024,
            );
        }
    }
    paths
        .iter()
        .map(|p| by_path.get(&p.display().to_string()).copied().unwrap_or(0))
        .collect()
}

// ---- Build folders -------------------------------------------------------

/// The build folders in one thread's folder: beside a file that says the
/// tool is there, a real folder (never a link) that git ignores whole and
/// that holds nothing git tracks.
fn build_folders_in(folder: &Path) -> Vec<(PathBuf, &'static str)> {
    let manifests: BTreeSet<String> = crate::control_plane::stack_of(folder)
        .into_iter()
        .map(|item| item.source)
        .collect();
    let mut found = Vec::new();
    for manifest in &manifests {
        let rel = Path::new(manifest);
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let Some((_, made_by, dirs)) = BUILT_BY.iter().find(|(file, _, _)| *file == name) else {
            continue;
        };
        let beside = rel.parent().unwrap_or(Path::new(""));
        for dir in *dirs {
            let rel = beside.join(dir);
            let path = folder.join(&rel);
            let real = path.symlink_metadata().is_ok_and(|m| m.is_dir());
            if !real || found.iter().any(|(p, _)| *p == path) {
                continue;
            }
            let rel = rel.display().to_string();
            let ignored = crate::git::run(folder, &["check-ignore", "-q", &rel]).is_ok();
            let tracked = crate::git::run(folder, &["ls-files", "--", &rel])
                .map_or(true, |out| !out.trim().is_empty());
            if ignored && !tracked {
                found.push((path, *made_by));
            }
        }
    }
    found
}

/// Every thread's build folders, each folder once, largest first.
fn build_folders(state: &AppState) -> Vec<BuildFolder> {
    let mut seen = BTreeSet::new();
    let mut found: Vec<(String, String, PathBuf, &'static str)> = Vec::new();
    for thread in crate::state::known_streams(state).unwrap_or_default() {
        if !thread.exists_on_disk || !seen.insert(thread.worktree_path.clone()) {
            continue;
        }
        for (path, made_by) in build_folders_in(Path::new(&thread.worktree_path)) {
            let shown = thread.title.clone().unwrap_or_else(|| thread.name.clone());
            found.push((thread.stream_id.clone(), shown, path, made_by));
        }
    }
    let paths: Vec<PathBuf> = found.iter().map(|(_, _, p, _)| p.clone()).collect();
    let mut folders: Vec<BuildFolder> = found
        .into_iter()
        .zip(sizes(&paths))
        .map(|((stream_id, thread, path, made_by), bytes)| BuildFolder {
            stream_id,
            thread,
            path: path.display().to_string(),
            made_by: made_by.to_string(),
            bytes,
        })
        .collect();
    folders.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    folders
}

// ---- Engines' images, containers and volumes -----------------------------

/// Docker's sizes as it prints them: `29.93GB`, `15.15GB (50%)`, `0B`.
fn docker_bytes(said: &str) -> u64 {
    let said = said.split(" (").next().unwrap_or_default().trim();
    let digits = said
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(said.len());
    let (number, unit) = said.split_at(digits);
    let scale: f64 = match unit.to_ascii_uppercase().as_str() {
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => 1.0,
    };
    (number.parse::<f64>().unwrap_or(0.0) * scale) as u64
}

/// `docker system df --format json`, one line a kind.
fn docker_use(rows: &[Value]) -> Vec<DiskUse> {
    let count = |v: &Value| v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
    rows.iter()
        .map(|r| DiskUse {
            kind: r["Type"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches("Local ")
                .to_lowercase(),
            count: count(&r["TotalCount"]),
            active: count(&r["Active"]),
            bytes: docker_bytes(r["Size"].as_str().unwrap_or_default()),
            reclaimable: docker_bytes(r["Reclaimable"].as_str().unwrap_or_default()),
        })
        .collect()
}

/// `container system df --format json`.
fn apple_use(report: &Value) -> Vec<DiskUse> {
    ["images", "containers", "volumes"]
        .into_iter()
        .filter(|kind| report[kind].is_object())
        .map(|kind| {
            let r = &report[kind];
            DiskUse {
                kind: kind.to_string(),
                count: r["total"].as_u64().unwrap_or(0),
                active: r["active"].as_u64().unwrap_or(0),
                bytes: r["sizeInBytes"].as_u64().unwrap_or(0),
                reclaimable: r["reclaimable"].as_u64().unwrap_or(0),
            }
        })
        .collect()
}

/// Docker's labels as it prints them: `a=1,b=2`.
fn docker_labels(said: &str) -> HashMap<String, String> {
    said.split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Letters and digits only, lowercased: `Acme Store` and
/// `Acme-Store` read the same.
fn plain(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}

/// The images the Bakery's own workspace image is built from.
fn bakery_bases() -> Vec<&'static str> {
    crate::seat_container::DOCKERFILE
        .lines()
        .filter_map(|line| line.strip_prefix("FROM "))
        .filter_map(|from| from.split([':', ' ']).next())
        .collect()
}

fn bakerys_image(repository: &str) -> bool {
    repository.starts_with("bakery/") || bakery_bases().contains(&repository)
}

/// Who uses what: the Bakery's projects, by name and folders and by id, and
/// its threads, by stream.
struct Owners {
    projects: Vec<(String, Vec<PathBuf>)>,
    names: HashMap<String, String>,
    threads: HashMap<String, String>,
}

impl Owners {
    fn of(state: &AppState) -> Owners {
        let records = crate::catalog::known_projects(state).unwrap_or_default();
        let names: HashMap<String, String> = records
            .iter()
            .map(|p| (p.project_id.clone(), p.name.clone()))
            .collect();
        let streams = crate::state::known_streams(state).unwrap_or_default();
        let threads = {
            let heart = state.heart.lock().unwrap();
            streams
                .into_iter()
                .filter_map(|s| {
                    let project = crate::state::socket_project(&heart, &s.stream_id).0?;
                    Some((s.stream_id, names.get(&project)?.clone()))
                })
                .collect()
        };
        Owners {
            projects: records
                .into_iter()
                .map(|p| {
                    (
                        p.name,
                        p.roots.into_iter().map(|r| PathBuf::from(r.path)).collect(),
                    )
                })
                .collect(),
            names,
            threads,
        }
    }

    fn by_name(&self, name: &str) -> Option<String> {
        let name = plain(name);
        self.projects
            .iter()
            .find(|(project, roots)| {
                plain(project) == name
                    || roots.iter().any(|r| {
                        r.file_name()
                            .is_some_and(|f| plain(&f.to_string_lossy()) == name)
                    })
            })
            .map(|(project, _)| project.clone())
    }

    fn by_folder(&self, dir: &Path) -> Option<String> {
        self.projects
            .iter()
            .find(|(_, roots)| roots.iter().any(|r| dir.starts_with(r)))
            .map(|(project, _)| project.clone())
    }

    /// The project and group a thing's labels tell: the Bakery's own, its
    /// compose working folder, then its compose or Supabase project's name.
    fn by_labels(&self, labels: &HashMap<String, String>) -> (Option<String>, Option<String>) {
        let group = labels
            .get("com.supabase.cli.project")
            .or_else(|| labels.get("com.docker.compose.project"))
            .cloned();
        if labels
            .keys()
            .any(|k| k == "com.bakery" || k.starts_with("com.bakery."))
        {
            // A thread's, else a project's own (its services), else the Bakery's.
            let owner = labels
                .get("com.bakery.stream")
                .and_then(|sid| self.threads.get(sid))
                .or_else(|| labels.get("com.bakery.project").and_then(|id| self.names.get(id)));
            return (
                Some(owner.cloned().unwrap_or_else(|| "Bakery".to_string())),
                group,
            );
        }
        let project = labels
            .get("com.docker.compose.project.working_dir")
            .and_then(|dir| self.by_folder(Path::new(dir)))
            .or_else(|| group.as_deref().and_then(|g| self.by_name(g)));
        (project, group)
    }
}

/// A Docker engine's images, containers and volumes (`docker system df
/// -v`), each with the project that uses it.
fn docker_items(engine: &str, report: &Value, owners: &Owners) -> Vec<EngineItem> {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let mut items = Vec::new();
    let mut by_image: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    let mut by_volume: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for c in report["Containers"].as_array().into_iter().flatten() {
        let labels = docker_labels(c["Labels"].as_str().unwrap_or_default());
        let (mut project, group) = owners.by_labels(&labels);
        let image = text(&c["Image"]);
        if project.is_none() && bakerys_image(image.split(':').next().unwrap_or_default()) {
            project = Some("Bakery".to_string());
        }
        by_image
            .entry(image)
            .or_insert((project.clone(), group.clone()));
        for volume in c["Mounts"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .filter(|v| !v.is_empty())
        {
            by_volume
                .entry(volume.to_string())
                .or_insert((project.clone(), group.clone()));
        }
        items.push(EngineItem {
            engine: engine.to_string(),
            kind: "container".to_string(),
            name: text(&c["Names"]),
            id: text(&c["ID"]),
            bytes: Some(docker_bytes(c["Size"].as_str().unwrap_or_default())),
            state: text(&c["State"]),
            project,
            group,
        });
    }
    let groups: Vec<(Option<String>, Option<String>)> = by_image
        .values()
        .chain(by_volume.values())
        .filter(|(_, g)| g.is_some())
        .cloned()
        .collect();
    for i in report["Images"].as_array().into_iter().flatten() {
        let (repository, tag) = (text(&i["Repository"]), text(&i["Tag"]));
        let name = if tag.is_empty() || tag == "<none>" {
            repository.clone()
        } else {
            format!("{repository}:{tag}")
        };
        let used = i["Containers"].as_str().is_some_and(|n| n != "0");
        let from_container = by_image
            .get(&name)
            .or_else(|| {
                (tag == "latest")
                    .then(|| by_image.get(&repository))
                    .flatten()
            })
            .cloned();
        // A compose project's own builds are named <project>-<service>; the
        // longest such name that fits wins (`acme-shop-web`
        // is never `acme`'s).
        let from_group = || {
            groups
                .iter()
                .filter(|(_, g)| {
                    g.as_deref().is_some_and(|g| {
                        repository.starts_with(&format!("{g}-"))
                            || repository.starts_with(&format!("{g}_"))
                    })
                })
                .max_by_key(|(_, g)| g.as_ref().map_or(0, String::len))
                .cloned()
        };
        let (project, group) = if bakerys_image(&repository) {
            (Some("Bakery".to_string()), None)
        } else {
            from_container.or_else(from_group).unwrap_or((None, None))
        };
        items.push(EngineItem {
            engine: engine.to_string(),
            kind: "image".to_string(),
            name,
            id: text(&i["ID"]),
            bytes: Some(docker_bytes(i["Size"].as_str().unwrap_or_default())),
            state: if used { "used" } else { "unused" }.to_string(),
            project,
            group,
        });
    }
    for v in report["Volumes"].as_array().into_iter().flatten() {
        let name = text(&v["Name"]);
        let (project, group) =
            match owners.by_labels(&docker_labels(v["Labels"].as_str().unwrap_or_default())) {
                (None, None) => by_volume.get(&name).cloned().unwrap_or((None, None)),
                told => told,
            };
        let used = v["Links"].as_str().is_some_and(|n| n != "0");
        items.push(EngineItem {
            engine: engine.to_string(),
            kind: "volume".to_string(),
            id: name.clone(),
            name,
            bytes: Some(docker_bytes(v["Size"].as_str().unwrap_or_default())),
            state: if used { "used" } else { "unused" }.to_string(),
            project,
            group,
        });
    }
    items
}

/// Apple container's containers, images and volumes, each with the project
/// that uses it.
fn apple_items(
    containers: &Value,
    images: &Value,
    volumes: &Value,
    owners: &Owners,
) -> Vec<EngineItem> {
    let labels_of = |v: &Value| -> HashMap<String, String> {
        v.as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect()
    };
    let mut items = Vec::new();
    let mut used: HashMap<String, Option<String>> = HashMap::new();
    for c in containers.as_array().into_iter().flatten() {
        let image = c["configuration"]["image"]["reference"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (mut project, group) = owners.by_labels(&labels_of(&c["configuration"]["labels"]));
        if project.is_none() && bakerys_image(image.split(':').next().unwrap_or_default()) {
            project = Some("Bakery".to_string());
        }
        used.entry(image).or_insert(project.clone());
        let id = c["id"].as_str().unwrap_or_default().to_string();
        items.push(EngineItem {
            engine: "apple".to_string(),
            kind: "container".to_string(),
            name: id.clone(),
            id,
            bytes: None,
            state: c["status"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            project,
            group,
        });
    }
    for i in images.as_array().into_iter().flatten() {
        let name = i["configuration"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let bytes = i["variants"]
            .as_array()
            .map(|vs| vs.iter().filter_map(|v| v["size"].as_u64()).sum());
        let repository = name
            .rsplit_once(':')
            .map_or(name.as_str(), |(r, _)| r)
            .to_string();
        let project = if bakerys_image(&repository) {
            Some("Bakery".to_string())
        } else {
            used.get(&name).cloned().flatten()
        };
        items.push(EngineItem {
            engine: "apple".to_string(),
            kind: "image".to_string(),
            id: name.clone(),
            state: if used.contains_key(&name) {
                "used"
            } else {
                "unused"
            }
            .to_string(),
            name,
            bytes,
            project,
            group: None,
        });
    }
    let sources: Vec<PathBuf> = volumes
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| PathBuf::from(v["configuration"]["source"].as_str().unwrap_or_default()))
        .collect();
    let on_disk = sizes(&sources);
    for (v, bytes) in volumes.as_array().into_iter().flatten().zip(on_disk) {
        let name = v["configuration"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (project, group) = owners.by_labels(&labels_of(&v["configuration"]["labels"]));
        items.push(EngineItem {
            engine: "apple".to_string(),
            kind: "volume".to_string(),
            id: name.clone(),
            name,
            bytes: Some(bytes),
            state: String::new(),
            project,
            group,
        });
    }
    items
}

/// A container tool's answer, when it succeeds.
fn ask(backend: Backend, args: &[&str]) -> Option<String> {
    let out = crate::containers::cli(backend)
        .ok()?
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ask_json(backend: Backend, args: &[&str]) -> Option<Value> {
    ask(backend, args).and_then(|text| serde_json::from_str(&text).ok())
}

fn items(state: &AppState) -> Vec<EngineItem> {
    let owners = Owners::of(state);
    let mut items = Vec::new();
    for backend in [Backend::Docker, Backend::Orbstack] {
        if let Some(report) = ask_json(backend, &["system", "df", "--verbose", "--format", "json"])
        {
            items.extend(docker_items(engine_name(backend), &report, &owners));
        }
    }
    if let (Some(containers), Some(images)) = (
        ask_json(Backend::Apple, &["list", "--all", "--format", "json"]),
        ask_json(Backend::Apple, &["image", "list", "--format", "json"]),
    ) {
        let volumes =
            ask_json(Backend::Apple, &["volume", "list", "--format", "json"]).unwrap_or_default();
        items.extend(apple_items(&containers, &images, &volumes, &owners));
    }
    items
}

// ---- The report ----------------------------------------------------------

/// The Bakery's own data, by its top folders.
fn bakery_parts(base: &Path) -> Vec<DataPart> {
    let dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let mut parts: Vec<DataPart> = sizes(&dirs)
        .into_iter()
        .zip(&dirs)
        .map(|(bytes, dir)| DataPart {
            name: dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            bytes,
        })
        .collect();
    parts.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    parts
}

fn report(state: &AppState) -> DiskReport {
    let (free, total) = space(&state.base).unwrap_or((0, 0));
    let colima = crate::machine_access::home_of(state).join(".colima");
    let docker_use_of = |backend| {
        ask(backend, &["system", "df", "--format", "json"]).map(|text| {
            match crate::containers::parse_json(&text) {
                Value::Array(rows) => docker_use(&rows),
                one => docker_use(&[one]),
            }
        })
    };
    DiskReport {
        free,
        total,
        low: low(free, total),
        build_folders: build_folders(state),
        docker: docker_use_of(Backend::Docker),
        orbstack: docker_use_of(Backend::Orbstack),
        apple_container: ask_json(Backend::Apple, &["system", "df", "--format", "json"])
            .map(|r| apple_use(&r)),
        items: items(state),
        colima: colima
            .is_dir()
            .then(|| sizes(&[colima]).first().copied().unwrap_or(0)),
        bakery: bakery_parts(&state.base),
    }
}

/// `GET /disk`: where the disk goes for the Bakery, and what may be cleaned.
pub(crate) async fn disk_route(State(st): State<AppState>) -> Result<Json<DiskReport>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(move || report(&st))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ---- Cleanups ------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "what", rename_all = "snake_case")]
pub(crate) enum CleanReq {
    /// One of the build folders `GET /disk` lists.
    BuildFolder { path: String },
    /// The images no container uses, never the Bakery's own.
    UnusedImages { backend: Backend },
    /// Every stopped container.
    StoppedContainers { backend: Backend },
    /// A Docker engine's build cache.
    BuildCache { backend: Backend },
    /// The volumes the operator ticked, by name; never one in use.
    Volumes {
        backend: Backend,
        names: Vec<String>,
    },
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Cleaned {
    /// Free space after.
    pub(crate) free: u64,
    /// What the tool said, for a container cleanup.
    pub(crate) output: String,
}

/// Whether any agent's turn is running in this thread.
fn working(state: &AppState, stream_id: &str) -> bool {
    state
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .iter()
        .any(|((sid, _), n)| sid == stream_id && *n > 0)
}

fn run(backend: Backend, args: &[&str]) -> Result<String, Fail> {
    let out = crate::containers::cli(backend)?
        .args(args)
        .output()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !out.status.success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn engine_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Docker => "docker",
        Backend::Orbstack => "orbstack",
        Backend::Apple => "apple",
    }
}

fn clean(state: &AppState, req: &CleanReq) -> Result<String, Fail> {
    match req {
        CleanReq::BuildFolder { path } => {
            // Only what the guard lists now: never anything else.
            let listed = build_folders(state)
                .into_iter()
                .find(|f| f.path == *path)
                .ok_or((
                    StatusCode::BAD_REQUEST,
                    format!("{path} isn't a build folder the disk guard lists"),
                ))?;
            if working(state, &listed.stream_id) {
                return Err((
                    StatusCode::CONFLICT,
                    format!(
                        "{}'s agent is working; clean it once it's done",
                        listed.thread
                    ),
                ));
            }
            std::fs::remove_dir_all(path)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{path}: {e}")))?;
            Ok(String::new())
        }
        CleanReq::UnusedImages { backend } => {
            let engine = engine_name(*backend);
            let unused: Vec<String> = items(state)
                .into_iter()
                .filter(|i| i.engine == engine && i.kind == "image" && i.state == "unused")
                .filter(|i| i.project.as_deref() != Some("Bakery"))
                .map(|i| i.id)
                .collect();
            if unused.is_empty() {
                return Ok("no image is unused".to_string());
            }
            let mut args = vec!["image", "rm"];
            args.extend(unused.iter().map(String::as_str));
            run(*backend, &args)
        }
        CleanReq::StoppedContainers { backend } => match backend {
            Backend::Docker | Backend::Orbstack => {
                run(*backend, &["container", "prune", "--force"])
            }
            Backend::Apple => run(Backend::Apple, &["prune"]),
        },
        CleanReq::BuildCache {
            backend: Backend::Apple,
        } => Err((
            StatusCode::BAD_REQUEST,
            "Apple container keeps its builder's cache in its builder".to_string(),
        )),
        CleanReq::BuildCache { backend } => run(*backend, &["builder", "prune", "--force"]),
        CleanReq::Volumes { backend, names } => {
            let engine = engine_name(*backend);
            let known = items(state);
            for name in names {
                let volume = known
                    .iter()
                    .find(|i| i.engine == engine && i.kind == "volume" && i.name == *name);
                match volume {
                    None => {
                        return Err((StatusCode::BAD_REQUEST, format!("{name}: no such volume")))
                    }
                    Some(v) if v.state == "used" => {
                        return Err((
                            StatusCode::CONFLICT,
                            format!("{name} is in use by a container"),
                        ))
                    }
                    Some(_) => {}
                }
            }
            if names.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "tick the volumes to remove".to_string(),
                ));
            }
            let mut args = match backend {
                Backend::Docker | Backend::Orbstack => vec!["volume", "rm"],
                Backend::Apple => vec!["volume", "delete"],
            };
            args.extend(names.iter().map(String::as_str));
            run(*backend, &args)
        }
    }
}

/// `POST /disk/clean`: one cleanup the operator chose and confirmed.
pub(crate) async fn clean_route(
    State(st): State<AppState>,
    Json(req): Json<CleanReq>,
) -> Result<Json<Cleaned>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(move || {
        let output = clean(&st, &req)?;
        let free = space(&st.base).map_or(0, |(free, _)| free);
        Ok(Json(Cleaned { free, output }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

static LOW: AtomicBool = AtomicBool::new(false);

/// From now on, a look at free space every few minutes: the app hears when
/// it runs low, and when it's back, as a `{"kind":"disk-low"}` envelope.
pub(crate) fn guard(state: AppState) {
    std::thread::spawn(move || loop {
        if let Some((free, total)) = space(&state.base) {
            let now = low(free, total);
            if now != LOW.swap(now, Ordering::Relaxed) {
                let envelope = serde_json::json!({ "kind": "disk-low", "low": now, "free": free, "total": total });
                let _ = state.tx.send(envelope.to_string());
            }
        }
        std::thread::sleep(Duration::from_secs(300));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_engine_is_read_in_its_own_units() {
        assert_eq!(docker_bytes("29.93GB"), 29_930_000_000);
        assert_eq!(docker_bytes("15.15GB (50%)"), 15_150_000_000);
        assert_eq!(docker_bytes("4.1kB"), 4_100);
        assert_eq!(docker_bytes("0B"), 0);
        let docker = docker_use(&[serde_json::json!(
            {"Active":"4","Reclaimable":"0B (0%)","Size":"230.2MB","TotalCount":"5","Type":"Local Volumes"}
        )]);
        assert_eq!(
            docker[0],
            DiskUse {
                kind: "volumes".into(),
                count: 5,
                active: 4,
                bytes: 230_200_000,
                reclaimable: 0
            }
        );
        let apple = apple_use(&serde_json::json!({
            "images": {"active": 2, "reclaimable": 6027153408u64, "sizeInBytes": 12163358720u64, "total": 8}
        }));
        assert_eq!(apple[0].reclaimable, 6_027_153_408);
        assert!(low(9, 100) && !low(10, 100), "under a tenth is low");
        let (free, total) = space(Path::new("/")).unwrap();
        assert!(total > 0 && free <= total);
        assert!(
            bakerys_image("bakery/workspace") && bakerys_image("rust") && bakerys_image("node")
        );
        assert!(!bakerys_image("supabase/postgres"));
    }

    /// Each image, container and volume is told by what says who uses it:
    /// the Bakery's labels, a compose working folder, a compose or Supabase
    /// project's name, the container that uses it.
    #[test]
    fn each_thing_is_told_by_the_project_that_uses_it() {
        let owners = Owners {
            projects: vec![
                (
                    "Acme Store".to_string(),
                    vec![PathBuf::from("/w/acme-store")],
                ),
                ("GreenLeaf".to_string(), vec![PathBuf::from("/w/greenleaf")]),
            ],
            names: HashMap::from([("project_f".to_string(), "GreenLeaf".to_string())]),
            threads: HashMap::from([("socket_1".to_string(), "GreenLeaf".to_string())]),
        };
        let report = serde_json::json!({
            "Containers": [
                {"ID":"c1","Image":"supabase/postgres:15.8","Names":"supabase_db_Acme-Store","State":"exited",
                 "Labels":"com.supabase.cli.project=Acme-Store","Mounts":"supabase_db_Acme-Store","Size":"4.1kB"},
                {"ID":"c2","Image":"acme-app:local","Names":"web","State":"exited",
                 "Labels":"com.docker.compose.project=acme,com.docker.compose.project.working_dir=/w/acme-store/app","Mounts":"","Size":"0B"},
                {"ID":"c3","Image":"alpine","Names":"seat","State":"running","Labels":"com.bakery=run,com.bakery.stream=socket_1","Mounts":"","Size":"0B"},
                {"ID":"c4","Image":"gone:1","Names":"odd","State":"exited","Labels":"com.docker.compose.project=acme-shop","Mounts":"","Size":"0B"},
                {"ID":"c5","Image":"postgres:18","Names":"bakery-project_f-postgres","State":"running","Labels":"com.bakery=service,com.bakery.project=project_f","Mounts":"","Size":"0B"}
            ],
            "Images": [
                {"ID":"sha256:a","Repository":"supabase/postgres","Tag":"15.8","Containers":"1","Size":"1GB"},
                {"ID":"sha256:b","Repository":"supabase/postgres","Tag":"15.1","Containers":"0","Size":"1GB"},
                {"ID":"sha256:c","Repository":"bakery/workspace","Tag":"dev","Containers":"0","Size":"3.1GB"},
                {"ID":"sha256:d","Repository":"acme-shop-web","Tag":"latest","Containers":"0","Size":"5MB"}
            ],
            "Volumes": [
                {"Name":"supabase_db_Acme-Store","Labels":"com.supabase.cli.project=Acme-Store","Links":"1","Size":"70MB"},
                {"Name":"loose","Labels":"","Links":"0","Size":"1MB"}
            ]
        });
        let items = docker_items("docker", &report, &owners);
        let told = |kind: &str, name: &str| {
            let i = items
                .iter()
                .find(|i| i.kind == kind && i.name == name)
                .unwrap_or_else(|| panic!("{name}"));
            (i.project.as_deref(), i.group.as_deref(), i.state.as_str())
        };
        assert_eq!(
            told("container", "supabase_db_Acme-Store"),
            (Some("Acme Store"), Some("Acme-Store"), "exited")
        );
        assert_eq!(
            told("container", "web"),
            (Some("Acme Store"), Some("acme"), "exited"),
            "by its compose folder"
        );
        assert_eq!(
            told("container", "seat"),
            (Some("GreenLeaf"), None, "running"),
            "by its thread"
        );
        assert_eq!(
            told("container", "bakery-project_f-postgres"),
            (Some("GreenLeaf"), None, "running"),
            "a project's own service"
        );
        assert_eq!(
            told("container", "odd"),
            (None, Some("acme-shop"), "exited")
        );
        assert_eq!(
            told("image", "supabase/postgres:15.8"),
            (Some("Acme Store"), Some("Acme-Store"), "used")
        );
        assert_eq!(
            told("image", "supabase/postgres:15.1"),
            (None, None, "unused"),
            "left over"
        );
        assert_eq!(
            told("image", "bakery/workspace:dev"),
            (Some("Bakery"), None, "unused")
        );
        assert_eq!(
            told("image", "acme-shop-web:latest"),
            (None, Some("acme-shop"), "unused")
        );
        assert_eq!(
            told("volume", "supabase_db_Acme-Store"),
            (Some("Acme Store"), Some("Acme-Store"), "used")
        );
        assert_eq!(told("volume", "loose"), (None, None, "unused"));
    }

    /// Only what the project's own tools make and git ignores whole is a
    /// build folder, and only a listed one is ever cleaned.
    #[test]
    fn a_build_folder_is_one_git_ignores_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("farm");
        std::fs::create_dir_all(repo.join("app/target/debug")).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(
            repo.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("app/target/debug/app"), vec![0u8; 8192]).unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        // Flutter's build folder, but git tracks a file in it: the project's.
        std::fs::write(repo.join("pubspec.yaml"), "name: farm\n").unwrap();
        std::fs::create_dir_all(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/notes.md"), "mine").unwrap();
        // A link named like one: never followed.
        std::os::unix::fs::symlink(repo.join("app"), repo.join(".dart_tool")).unwrap();
        git(&["add", "."]);

        let found = build_folders_in(&repo);
        let rels: Vec<(String, &str)> = found
            .iter()
            .map(|(p, by)| (p.strip_prefix(&repo).unwrap().display().to_string(), *by))
            .collect();
        assert_eq!(rels, [("app/target".to_string(), "Cargo")]);
        assert!(sizes(&[repo.join("app/target")])[0] >= 8192);

        let state = crate::testutil::state_with(
            &dir.path().join("base"),
            std::sync::Arc::new(bakery_agent::StubModel),
        );
        let root = crate::catalog::DirectoryRootSpec::Attach(repo.to_str().unwrap());
        crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let listed = build_folders(&state);
        assert_eq!(listed.len(), 1, "{listed:?}");
        let elsewhere = CleanReq::BuildFolder {
            path: repo.join("build").display().to_string(),
        };
        assert_eq!(
            clean(&state, &elsewhere).unwrap_err().0,
            StatusCode::BAD_REQUEST,
            "never a folder it doesn't list"
        );
        assert!(repo.join("build/notes.md").is_file());
        clean(
            &state,
            &CleanReq::BuildFolder {
                path: listed[0].path.clone(),
            },
        )
        .unwrap();
        assert!(!repo.join("app/target").exists() && repo.join("app/Cargo.toml").is_file());
    }
}
