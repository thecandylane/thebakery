//! The network list: per project, the hosts its threads' sandboxes reach
//! through the Bakery's gateway (`seat_gateway`). A thread starts from its
//! sandbox kind's list ([`Start`]): the defaults, and the hosts its
//! project's files point to. Any other host asks the operator once per
//! project (`network.host.ask`: a card in the thread, which then needs
//! them), and their answer holds for the whole project
//! (`network.host.decided`: kept on the project's stream, and one on each
//! asking thread citing its ask, so every card resolves where it was
//! asked). Only the operator answers, in the app or on the phone.
//!
//! Every sandbox names its thread to the gateway with a proxy credential of
//! its own ([`credential`]); a CONNECT without one is refused (407), so the
//! operator's answers hold for every process in the sandbox, defaults
//! included.

use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// A host a thread's sandbox tried, waiting on the operator: on the thread
/// (its card) and on the project's stream (its list).
pub(crate) const ASK: &str = "network.host.ask";
/// The operator's answer for a project: on the project's stream, and on
/// each asking thread, citing its ask.
pub(crate) const DECIDED: &str = "network.host.decided";

/// Where a sandbox kind's list starts, before the operator answers any ask.
/// Each kind's start is one line, below.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Start {
    /// Hosts every thread of the kind reaches, beside the ones the operator
    /// adds in Settings → Machine access. A leading dot allows any
    /// subdomain.
    pub(crate) defaults: &'static [&'static str],
    /// Whether the hosts its project's files point to count too.
    pub(crate) implied: bool,
}

/// A Linux container seat, as the operator answered: the defaults plus the
/// hosts its project's files point to.
pub(crate) const CONTAINER: Start = Start {
    defaults: crate::seat_gateway::BUILT_IN,
    implied: true,
};

/// The Mac sandbox, as the operator answered for GreenLeaf: its list through
/// the Bakery, starting the same way, asking once per new host.
pub(crate) const MAC: Start = Start {
    defaults: crate::seat_gateway::BUILT_IN,
    implied: true,
};

/// The kind of sandbox a credential names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Container,
    Mac,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Container => "container",
            Kind::Mac => "mac",
        }
    }

    fn of(word: &str) -> Option<Kind> {
        match word {
            "container" => Some(Kind::Container),
            "mac" => Some(Kind::Mac),
            _ => None,
        }
    }

    fn start(self) -> Start {
        match self {
            Kind::Container => CONTAINER,
            Kind::Mac => MAC,
        }
    }

    /// The ports its sandbox may reach a listed host on: HTTPS, and for this
    /// Mac's doors plain HTTP too (a Preview's http:// pages, and its ws://,
    /// which a browser opens with a CONNECT to port 80).
    fn ports(self) -> &'static [&'static str] {
        match self {
            Kind::Container => &["443"],
            Kind::Mac => &["443", "80"],
        }
    }
}

/// The hosts a project's files point to, by what detection names. Never a
/// shared-tenant domain (anyone's bucket or site, storage.googleapis.com
/// say): those ask, so the operator sees what allowing one means.
const IMPLIED: &[(&str, &[&str])] = &[
    ("Dart", &["pub.dev"]),
    // Flutter web's fallback fonts and its renderer: Google's own files.
    ("Flutter", &["fonts.gstatic.com", "www.gstatic.com"]),
    ("CocoaPods", &["cdn.cocoapods.org"]),
    ("Ruby", &["rubygems.org", "index.rubygems.org"]),
    (
        "Firebase",
        &[
            "firebase.googleapis.com",
            "firestore.googleapis.com",
            "identitytoolkit.googleapis.com",
            "securetoken.googleapis.com",
            "firebaseinstallations.googleapis.com",
        ],
    ),
    ("Cloudflare Workers", &["api.cloudflare.com"]),
    ("Protocol Buffers", &["buf.build"]),
];

/// What a thread's sandbox meets at the gateway.
#[derive(Debug, Clone)]
struct Pass {
    stream_id: String,
    project_id: Option<String>,
    start: Start,
    /// The hosts its project's files point to.
    implied: Vec<String>,
}

/// A thread's pass, its project's files read at most once a minute.
// ponytail: a minute's cache; a file written since counts from the next read.
fn pass(state: &AppState, stream_id: &str, kind: Kind) -> Pass {
    static PASSES: OnceLock<Mutex<HashMap<(String, Kind), (Instant, Pass)>>> = OnceLock::new();
    let passes = PASSES.get_or_init(Default::default);
    let key = (stream_id.to_string(), kind);
    if let Some((at, pass)) = passes.lock().unwrap().get(&key) {
        if at.elapsed() < Duration::from_secs(60) {
            return pass.clone();
        }
    }
    let start = kind.start();
    let project_id = match crate::terminals::project_of_stream(stream_id) {
        Some(project) => Some(project.to_string()),
        None => crate::catalog::settings_project_locked(&state.heart.lock().unwrap(), stream_id),
    };
    // None from the Bakery's own files (`implied` reads catalog projects
    // only): its threads reach what they reached before.
    let implied = match &project_id {
        Some(project) if start.implied => implied(state, project)
            .into_iter()
            .map(|(host, _)| host)
            .collect(),
        _ => Vec::new(),
    };
    let pass = Pass {
        stream_id: stream_id.to_string(),
        project_id,
        start,
        implied,
    };
    passes
        .lock()
        .unwrap()
        .insert(key, (Instant::now(), pass.clone()));
    pass
}

/// Asks and answers one at a time, so a project never asks twice about one
/// host and an answer never misses an ask landing beside it.
// ponytail: one lock for every project; per-project locks if asks ever queue.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a credential is signed with: drawn from the node's own secret, so a
/// thread's credential holds across restarts and needs no table, and none
/// can be made or guessed without it.
fn key(state: &AppState) -> [u8; 32] {
    let secret = state
        .node
        .lock()
        .unwrap()
        .operator_token
        .clone()
        .unwrap_or_default();
    blake3::derive_key(
        "bakery sandbox proxy credential 2026-10-11",
        secret.as_bytes(),
    )
}

/// The proxy credential a thread's sandbox of `kind` names itself with: the
/// name `bakery-<kind>-<the thread, in hex>`, and that name signed with the
/// node's key. Never logged.
pub(crate) fn credential(state: &AppState, stream_id: &str, kind: Kind) -> (String, String) {
    let thread: String = stream_id.bytes().map(|b| format!("{b:02x}")).collect();
    let name = format!("bakery-{}-{thread}", kind.word());
    let signed = blake3::keyed_hash(&key(state), name.as_bytes())
        .to_hex()
        .to_string();
    (name, signed)
}

/// The proxy a thread's sandbox of `kind` uses: the gateway at `gateway`,
/// with its credential.
pub(crate) fn proxy_url(
    state: &AppState,
    stream_id: &str,
    kind: Kind,
    gateway: std::net::Ipv4Addr,
) -> String {
    let (name, signed) = credential(state, stream_id, kind);
    format!(
        "http://{name}:{signed}@{gateway}:{}",
        crate::seat_gateway::PORT
    )
}

/// The thread and kind a CONNECT's credential names, when its signature
/// holds: `Proxy-Authorization: Basic` of the name and its signature.
fn caller(state: &AppState, headers: &HeaderMap) -> Option<(String, Kind)> {
    credential_of(state, headers.get(header::PROXY_AUTHORIZATION)?.to_str().ok()?)
}

/// The thread and kind a `Basic` credential (`Basic <name:signature>`)
/// names, when its signature holds.
pub(crate) fn credential_of(state: &AppState, basic: &str) -> Option<(String, Kind)> {
    let basic = basic.strip_prefix("Basic ")?;
    let pair = String::from_utf8(crate::attachments::decode_base64(basic.trim()).ok()?).ok()?;
    let (name, signed) = pair.split_once(':')?;
    let signed = blake3::Hash::from_hex(signed).ok()?;
    // blake3's Hash compares in constant time.
    if signed != blake3::keyed_hash(&key(state), name.as_bytes()) {
        return None;
    }
    let (kind, thread) = name.strip_prefix("bakery-")?.split_once('-')?;
    let bytes: Option<Vec<u8>> = (0..thread.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(thread.get(i..i + 2)?, 16).ok())
        .collect();
    Some((String::from_utf8(bytes?).ok()?, Kind::of(kind)?))
}

/// A refusal the sandbox sees: its status, and why.
pub(crate) type Refusal = (StatusCode, String);

/// Whether the gateway opens a tunnel to `authority` (`host:port`) for the
/// sandbox whose CONNECT carried `headers`: the thread its credential names.
pub(crate) fn verdict(
    state: &AppState,
    authority: &str,
    headers: &HeaderMap,
) -> Result<(), Refusal> {
    let Some((stream_id, kind)) = caller(state, headers) else {
        return Err((
            StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            "the Bakery's gateway takes only a thread's own credential".to_string(),
        ));
    };
    verdict_for(state, authority, &stream_id, kind, headers)
}

/// Whether `stream_id`'s sandbox of `kind` may open a tunnel to `authority`.
/// In order: the operator's answer for the project, then the defaults, then
/// what the project's files point to; any other host asks the operator.
pub(crate) fn verdict_for(
    state: &AppState,
    authority: &str,
    stream_id: &str,
    kind: Kind,
    headers: &HeaderMap,
) -> Result<(), Refusal> {
    let refused = || {
        Err((
            StatusCode::FORBIDDEN,
            format!("{authority} is not one of the hosts seats may reach"),
        ))
    };
    let Some((host, port)) = authority.rsplit_once(':') else {
        return refused();
    };
    if !kind.ports().contains(&port) {
        return refused();
    }
    let Ok(host) = entry(host) else {
        return refused();
    };
    if host.starts_with("*.") {
        return refused();
    }
    let pass = pass(state, stream_id, kind);
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let _one = one_at_a_time();
    if let Some(project_id) = pass.project_id.as_deref() {
        match answer(&decisions(state, project_id).map_err(internal)?, &host) {
            Some(true) => return Ok(()),
            Some(false) => {
                let whose = if project_id.is_empty() {
                    "the Bakery"
                } else {
                    "this project"
                };
                return Err((
                    StatusCode::FORBIDDEN,
                    format!("the operator said no to {host} for {whose}"),
                ));
            }
            None => {}
        }
    }
    let added = state
        .node
        .lock()
        .unwrap()
        .machine_access
        .allow_hosts
        .clone();
    let port: u16 = port.parse().unwrap_or_default();
    if crate::seat_gateway::allowed(authority, pass.start.defaults, port)
        || crate::seat_gateway::allowed(authority, &added, port)
        || pass.implied.contains(&host)
    {
        return Ok(());
    }
    // The Bakery's own threads never ask: they reach the defaults and what
    // the operator allowed for the Bakery, as before.
    let Some(project_id) = pass.project_id.as_deref().filter(|p| !p.is_empty()) else {
        return refused();
    };
    if crate::learning::scratch(state, &pass.stream_id) {
        let refusal = crate::learning::refuse(state, &pass.stream_id, &format!("a new site: {host}"));
        return Err((StatusCode::FORBIDDEN, refusal));
    }
    ask(state, &pass.stream_id, project_id, &host, program(headers)).map_err(internal)?;
    Err((
        StatusCode::FORBIDDEN,
        format!("{host} waits on the operator: the Bakery asked them about it. Try again once they allow it."),
    ))
}

/// The program that tried, as its `User-Agent` names it (`Dart/3.9`,
/// `curl/8.7.1`), when it says: its first word, kept short.
fn program(headers: &HeaderMap) -> Option<String> {
    let first = headers
        .get(header::USER_AGENT)?
        .to_str()
        .ok()?
        .split_whitespace()
        .next()?;
    let plain = first.len() <= 40
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "./-_+".contains(c));
    plain.then(|| first.to_string())
}

/// A list entry as the list keeps it: a host, or `*.domain` for any host
/// under that domain, in lower case. Never an address, a one-word name, or
/// a whole top-level domain.
fn entry(host: &str) -> Result<String, String> {
    let lower = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let name = lower.strip_prefix("*.").unwrap_or(&lower);
    let labels: Vec<&str> = name.split('.').collect();
    let named = name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && !labels
            .last()
            .is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()));
    if !named {
        return Err(format!(
            "{host:?} is not a host name: give one like pub.dev, or *.googleapis.com for any host under a domain"
        ));
    }
    Ok(lower)
}

/// Whether a list entry (a host, or `*.domain`) covers `host`.
fn covers(entry: &str, host: &str) -> bool {
    match entry.strip_prefix("*.") {
        Some(domain) => host
            .strip_suffix(domain)
            .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
        None => entry == host,
    }
}

#[derive(Serialize, Deserialize)]
struct Decided {
    project_id: String,
    host: String,
    allow: Option<bool>,
}

/// The operator's answers for a project, by entry: whether to allow it, and
/// when they said so. A forgotten one is gone.
fn decisions(
    state: &AppState,
    project_id: &str,
) -> Result<BTreeMap<String, (bool, String)>, String> {
    let events = state
        .heart
        .lock()
        .unwrap()
        .events_of_family(crate::catalog::kept_on(project_id), DECIDED)
        .map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for ev in events {
        let Ok(d) = serde_json::from_str::<Decided>(&ev.payload) else {
            continue;
        };
        match d.allow {
            Some(allow) => out.insert(d.host, (allow, ev.ts)),
            None => out.remove(&d.host),
        };
    }
    Ok(out)
}

/// The answer that covers `host`: its own, else the narrowest `*.domain`.
fn answer(decisions: &BTreeMap<String, (bool, String)>, host: &str) -> Option<bool> {
    if let Some((allow, _)) = decisions.get(host) {
        return Some(*allow);
    }
    decisions
        .iter()
        .filter(|(entry, _)| covers(entry, host))
        .max_by_key(|(entry, _)| entry.len())
        .map(|(_, (allow, _))| *allow)
}

/// The hosts `project_id`'s files point to, each with the file that says
/// so.
fn implied(state: &AppState, project_id: &str) -> Vec<(String, String)> {
    let Some(project) = crate::catalog::project_by_id(state, project_id) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for root in &project.roots {
        for item in crate::control_plane::stack_of(Path::new(&root.path)) {
            let hosts = IMPLIED
                .iter()
                .filter(|(name, _)| *name == item.name)
                .flat_map(|(_, hosts)| hosts.iter());
            for host in hosts {
                if !out.iter().any(|(h, _)| h == host) {
                    out.push((host.to_string(), item.source.clone()));
                }
            }
        }
    }
    out
}

#[derive(Serialize, Deserialize)]
struct Asked {
    project_id: String,
    host: String,
    port: u16,
    program: Option<String>,
    /// On the project's stream only: the asking thread, and its card.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    stream_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    ask: String,
}

/// The thread's sandbox tried `host`: a card in the thread and a line in
/// the project's list, unless the project already asks about it.
fn ask(
    state: &AppState,
    stream_id: &str,
    project_id: &str,
    host: &str,
    program: Option<String>,
) -> Result<(), String> {
    if open_asks(state, project_id)?.iter().any(|a| a.host == host) {
        return Ok(());
    }
    let mut asked = Asked {
        project_id: project_id.to_string(),
        host: host.to_string(),
        port: 443,
        program,
        stream_id: String::new(),
        ask: String::new(),
    };
    let card = serde_json::to_string(&asked).map_err(|e| e.to_string())?;
    let card = crate::state::append(state, stream_id, "bakery", ASK, card, vec![])?;
    asked.stream_id = stream_id.to_string();
    asked.ask = card.event_hash.clone();
    crate::state::append(
        state,
        project_id,
        "bakery",
        ASK,
        serde_json::to_string(&asked).map_err(|e| e.to_string())?,
        vec![card.event_hash],
    )
    .map(drop)
}

/// A host waiting on the operator.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Ask {
    pub(crate) host: String,
    pub(crate) port: u16,
    /// The thread that asked.
    pub(crate) stream_id: String,
    /// The ask's event in that thread: its card.
    pub(crate) event_hash: String,
    pub(crate) at: String,
    /// The program that tried, as it names itself (`Dart/3.9`), when it
    /// said.
    pub(crate) program: Option<String>,
}

/// The project's asks the operator hasn't answered, oldest first. An ask
/// from a thread since discarded no longer counts.
fn open_asks(state: &AppState, project_id: &str) -> Result<Vec<Ask>, String> {
    let heart = state.heart.lock().unwrap();
    let stream = crate::catalog::kept_on(project_id);
    let asks = heart
        .events_of_family(stream, ASK)
        .map_err(|e| e.to_string())?;
    let decided = heart
        .events_of_family(stream, DECIDED)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for ev in asks {
        let Ok(a) = serde_json::from_str::<Asked>(&ev.payload) else {
            continue;
        };
        let answered = decided.iter().filter(|d| d.seq > ev.seq).any(|d| {
            serde_json::from_str::<Decided>(&d.payload)
                .is_ok_and(|d| d.allow.is_some() && covers(&d.host, &a.host))
        });
        if answered || crate::state::discarded_of(&heart, &a.stream_id) {
            continue;
        }
        out.push(Ask {
            host: a.host,
            port: a.port,
            stream_id: a.stream_id,
            event_hash: a.ask,
            at: ev.ts,
            program: a.program,
        });
    }
    Ok(out)
}

/// The host a thread's oldest unanswered ask names: the thread needs the
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
        .map(|a| a.host)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Source {
    /// On every thread's starting list (or added in Settings → Machine
    /// access); the operator's answer for the project takes its place.
    Default,
    /// The project's files point to it.
    Project,
    /// The operator's answer for this project.
    Operator,
}

/// One line of a project's list.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Host {
    /// A host, or `*.domain` for any host under that domain.
    pub(crate) host: String,
    pub(crate) allow: bool,
    pub(crate) source: Source,
    /// The file that points to it (`pubspec.yaml`), for `project`.
    pub(crate) because: Option<String>,
    /// When the operator answered, for `operator`.
    pub(crate) at: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Hosts {
    pub(crate) project_id: String,
    /// Each host once, as it stands now: defaults first (the operator's
    /// answer in a default's place), then the operator's other answers,
    /// then what the project's files point to.
    pub(crate) hosts: Vec<Host>,
    pub(crate) asks: Vec<Ask>,
}

/// A project's list, as its threads meet it now ("": the Bakery's own, as
/// its own threads do).
pub(crate) fn hosts(state: &AppState, project_id: &str) -> Result<Hosts, String> {
    if !crate::catalog::is_known(state, project_id) {
        return Err(format!("unknown project {project_id}"));
    }
    // A container's start, the one kind with a list yet.
    let start = CONTAINER;
    let added = state
        .node
        .lock()
        .unwrap()
        .machine_access
        .allow_hosts
        .clone();
    let mut list: Vec<Host> = Vec::new();
    // The first line for a host, but the operator's answer takes a
    // default's place: it is what the threads meet ([`verdict_for`]).
    let mut add = |line: Host| match list.iter_mut().find(|h| h.host == line.host) {
        Some(seen) if seen.source == Source::Default && line.source == Source::Operator => {
            *seen = line
        }
        Some(_) => {}
        None => list.push(line),
    };
    for host in start.defaults.iter().map(|h| h.to_string()).chain(added) {
        add(Host {
            host: match host.strip_prefix('.') {
                Some(domain) => format!("*.{domain}"),
                None => host,
            },
            allow: true,
            source: Source::Default,
            because: None,
            at: None,
        });
    }
    for (host, (allow, at)) in decisions(state, project_id)? {
        add(Host {
            host,
            allow,
            source: Source::Operator,
            because: None,
            at: Some(at),
        });
    }
    if start.implied {
        for (host, because) in implied(state, project_id) {
            add(Host {
                host,
                allow: true,
                source: Source::Project,
                because: Some(because),
                at: None,
            });
        }
    }
    Ok(Hosts {
        project_id: project_id.to_string(),
        hosts: list,
        asks: open_asks(state, project_id)?,
    })
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct Decide {
    /// The project; "" for the Bakery's own, which its own threads meet.
    pub(crate) project_id: String,
    /// A host, or `*.domain` for any host under that domain.
    pub(crate) host: String,
    /// Allow or deny it for the project; `null` forgets the answer, so the
    /// host asks again.
    pub(crate) allow: Option<bool>,
}

/// The operator's answer for a project. An allow or a deny resolves every
/// open ask it covers, in each thread that asked.
pub(crate) fn decide(state: &AppState, req: &Decide) -> Result<Hosts, (StatusCode, String)> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator answers which hosts a project reaches".to_string(),
        ));
    }
    if !crate::catalog::is_known(state, &req.project_id) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown project {}", req.project_id),
        ));
    }
    let host = entry(&req.host).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let one = one_at_a_time();
    let open = open_asks(state, &req.project_id).map_err(internal)?;
    let decided = serde_json::to_string(&Decided {
        project_id: req.project_id.clone(),
        host: host.clone(),
        allow: req.allow,
    })
    .map_err(|e| internal(e.to_string()))?;
    crate::state::append(
        state,
        crate::catalog::kept_on(&req.project_id),
        "human",
        DECIDED,
        decided.clone(),
        vec![],
    )
    .map_err(internal)?;
    let mut answered: Vec<&str> = Vec::new();
    if req.allow.is_some() {
        for ask in open.iter().filter(|a| covers(&host, &a.host)) {
            crate::state::append(
                state,
                &ask.stream_id,
                "human",
                DECIDED,
                decided.clone(),
                vec![ask.event_hash.clone()],
            )
            .map_err(internal)?;
            if !answered.contains(&ask.stream_id.as_str()) {
                answered.push(&ask.stream_id);
            }
        }
    }
    drop(one);
    // Each thread that asked hears it may try again, once.
    if req.allow == Some(true) {
        for stream_id in answered {
            crate::agent_control::tell(
                state,
                stream_id,
                format!("The operator allowed {host} for this project. Try again."),
            )
            .map_err(internal)?;
        }
    }
    hosts(state, &req.project_id).map_err(internal)
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct HostsQuery {
    /// The project; "" for the Bakery's own, as its own threads meet it.
    pub(crate) project_id: String,
}

/// `GET /network/hosts`: a project's list and its open asks.
pub(crate) async fn list(
    State(st): State<AppState>,
    Query(q): Query<HostsQuery>,
) -> Result<Json<Hosts>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || hosts(&st, &q.project_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e))
}

/// `POST /network/host/decide`: the operator's answer.
pub(crate) async fn decide_host(
    State(st): State<AppState>,
    Json(req): Json<Decide>,
) -> Result<Json<Hosts>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || decide(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A CONNECT with `credential`, as the sandbox sees the answer: the
    /// refusal's status, then why.
    fn connect(
        state: &AppState,
        authority: &str,
        credential: Option<&(String, String)>,
    ) -> Result<(), String> {
        let mut headers = HeaderMap::new();
        if let Some((name, signed)) = credential {
            let basic =
                bakery_agent::tool_wire::encode_base64(format!("{name}:{signed}").as_bytes());
            headers.insert(
                header::PROXY_AUTHORIZATION,
                format!("Basic {basic}").parse().unwrap(),
            );
        }
        headers.insert(header::USER_AGENT, "Dart/3.9 (dart:io)".parse().unwrap());
        verdict(state, authority, &headers)
            .map_err(|(status, why)| format!("{} {why}", status.as_u16()))
    }

    fn status(state: &AppState, stream_id: &str) -> Option<String> {
        let heart = state.heart.lock().unwrap();
        crate::agents::conversation_status_locked(&heart, stream_id, false, 0, None).1
    }

    fn answer_it(
        state: &AppState,
        project_id: &str,
        host: &str,
        allow: Option<bool>,
    ) -> Result<Hosts, (StatusCode, String)> {
        decide(
            state,
            &Decide {
                project_id: project_id.to_string(),
                host: host.to_string(),
                allow,
            },
        )
    }

    /// A thread reaches the defaults and what its project's files point to;
    /// any other host asks the operator once, a card that needs them, and
    /// their answer holds for the project; forgotten, it asks again.
    #[test]
    fn a_new_host_asks_the_operator_once_and_the_answer_holds_for_the_project() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = work.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("pubspec.yaml"), "name: farm\n").unwrap();
        let root = || crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, a) = crate::catalog::create_project(&state, "farm", Some(root())).unwrap();
        let b = crate::catalog::attach_directory_root(&state, &project.project_id, "Login", root())
            .unwrap();
        let p = project.project_id.as_str();
        let (ta, tb) = (
            credential(&state, &a.stream_id, Kind::Container),
            credential(&state, &b.stream_id, Kind::Container),
        );
        assert_eq!(
            ta,
            credential(&state, &a.stream_id, Kind::Container),
            "one per thread"
        );

        assert!(
            connect(&state, "index.crates.io:443", Some(&ta)).is_ok(),
            "a default"
        );
        assert!(
            connect(&state, "pub.dev:443", Some(&ta)).is_ok(),
            "pubspec.yaml points to it"
        );

        let waits = connect(&state, "storage.googleapis.com:443", Some(&ta)).unwrap_err();
        assert!(waits.contains("waits on the operator"), "{waits}");
        assert_eq!(
            status(&state, &a.stream_id).as_deref(),
            Some("wants to reach storage.googleapis.com")
        );
        assert_eq!(
            connect(&state, "storage.googleapis.com:443", Some(&tb)).unwrap_err(),
            waits,
            "another thread's retry: the same answer"
        );
        let cards = |stream: &str| {
            let heart = state.heart.lock().unwrap();
            heart.events_of_family(stream, ASK).unwrap()
        };
        assert_eq!(
            (cards(&a.stream_id).len(), cards(&b.stream_id).len()),
            (1, 0),
            "one card"
        );
        let open = hosts(&state, p).unwrap().asks;
        assert_eq!(open.len(), 1);
        assert_eq!(
            (open[0].stream_id.as_str(), open[0].program.as_deref()),
            (a.stream_id.as_str(), Some("Dart/3.9"))
        );

        let list = answer_it(&state, p, "storage.googleapis.com", Some(true)).unwrap();
        assert!(list.asks.is_empty());
        let answers = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&a.stream_id, DECIDED)
            .unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(
            answers[0].causality,
            vec![cards(&a.stream_id)[0].event_hash.clone()]
        );
        assert_eq!(
            status(&state, &a.stream_id),
            None,
            "answered: no longer needs you"
        );
        let told = |stream: &str| {
            let heart = state.heart.lock().unwrap();
            heart
                .events_of_family(stream, crate::agent_control::FOLLOW_UP_QUEUED)
                .unwrap()
                .iter()
                .filter(|e| {
                    e.payload
                        .contains("allowed storage.googleapis.com for this project. Try again.")
                })
                .count()
        };
        assert_eq!(
            (told(&a.stream_id), told(&b.stream_id)),
            (1, 0),
            "the asker hears it"
        );
        assert!(
            connect(&state, "storage.googleapis.com:443", Some(&tb)).is_ok(),
            "for the project"
        );

        answer_it(&state, p, "storage.googleapis.com", None).unwrap();
        assert!(
            connect(&state, "storage.googleapis.com:443", Some(&tb)).is_err(),
            "forgotten: asks"
        );
        assert_eq!(cards(&b.stream_id).len(), 1, "a new card");
        answer_it(&state, p, "storage.googleapis.com", Some(false)).unwrap();
        assert_eq!(
            connect(&state, "storage.googleapis.com:443", Some(&tb)).unwrap_err(),
            "403 the operator said no to storage.googleapis.com for this project"
        );
        assert_eq!(status(&state, &b.stream_id), None, "a no answers it too");
        assert_eq!(told(&b.stream_id), 0, "a no isn't a go-ahead");

        // A default is the project's to turn off, like any other host.
        answer_it(&state, p, "index.crates.io", Some(false)).unwrap();
        assert!(connect(&state, "index.crates.io:443", Some(&ta))
            .unwrap_err()
            .contains("said no"));
        answer_it(&state, p, "index.crates.io", None).unwrap();
        assert!(connect(&state, "index.crates.io:443", Some(&ta)).is_ok());
        // Without a thread's credential, or with one forged, nothing opens.
        let none = connect(&state, "index.crates.io:443", None).unwrap_err();
        assert!(none.starts_with("407 "), "{none}");
        let forged = (ta.0.clone(), "0".repeat(64));
        assert!(connect(&state, "index.crates.io:443", Some(&forged))
            .unwrap_err()
            .starts_with("407 "));
        assert!(connect(&state, "example.com:8443", Some(&ta))
            .unwrap_err()
            .contains("not one of"));
        // Plain HTTP on a listed host: this Mac's doors only (a Preview's
        // http:// pages and ws://), never a container's gateway.
        assert!(connect(&state, "index.crates.io:80", Some(&ta))
            .unwrap_err()
            .contains("not one of"));
        let door = |authority: &str| {
            verdict_for(&state, authority, &a.stream_id, Kind::Mac, &HeaderMap::new()).is_ok()
        };
        assert!(door("index.crates.io:80") && door("index.crates.io:443"));
        assert!(!door("index.crates.io:8080"));
        assert!(connect(&state, "10.0.0.1:443", Some(&ta))
            .unwrap_err()
            .contains("not one of"));
        assert!(
            hosts(&state, p).unwrap().asks.is_empty(),
            "none of those asked"
        );
    }

    /// The list says where each host comes from, and a `*.domain` answer
    /// covers the hosts under it, not the domain itself.
    #[test]
    fn the_list_names_each_hosts_source_and_patterns_cover_subdomains() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = work.path().join("site");
        std::fs::create_dir_all(app.join("ios")).unwrap();
        std::fs::write(app.join("ios/Podfile"), "platform :ios\n").unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, a) = crate::catalog::create_project(&state, "site", Some(root)).unwrap();
        let p = project.project_id.as_str();
        let token = credential(&state, &a.stream_id, Kind::Container);
        answer_it(&state, p, "*.Example.org.", Some(true)).unwrap();
        assert!(connect(&state, "api.example.org:443", Some(&token)).is_ok());
        assert!(connect(&state, "example.org:443", Some(&token)).is_err());

        let list = hosts(&state, p).unwrap();
        let line = |host: &str| list.hosts.iter().find(|h| h.host == host).unwrap();
        assert_eq!(line("crates.io").source, Source::Default);
        assert_eq!(line("*.githubusercontent.com").source, Source::Default);
        let pods = line("cdn.cocoapods.org");
        assert_eq!(
            (pods.source, pods.because.as_deref()),
            (Source::Project, Some("ios/Podfile"))
        );
        let mine = line("*.example.org");
        assert!(mine.allow && mine.source == Source::Operator && mine.at.is_some());
        assert_eq!(list.asks.len(), 1, "example.org itself asked");
        assert_eq!(list.asks[0].host, "example.org");
    }

    /// The Bakery's own list (""): the operator's answers hold for its own
    /// threads, the Baker's and its workspaces', never a project's; nothing
    /// else changes for them: a new host is refused, never asked.
    #[tokio::test]
    async fn the_bakerys_own_list_holds_for_its_own_threads_and_never_asks() {
        use crate::testutil::*;
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let orc = crate::workspaces::create_workspace(&state, "Tidy", None, "human").unwrap().stream_id;
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (_, theirs) = crate::catalog::create_project(&state, "app", Some(root)).unwrap();
        let reach = |stream: &str, authority: &str| {
            verdict_for(&state, authority, stream, Kind::Mac, &HeaderMap::new()).map_err(|(_, why)| why)
        };
        let own = || HostsQuery { project_id: String::new() };
        let answer = |host: &str, allow: Option<bool>| {
            let req = Decide { project_id: String::new(), host: host.to_string(), allow };
            decide_host(State(state.clone()), Json(req))
        };

        let shown = list(State(state.clone()), Query(own())).await.unwrap().0;
        assert_eq!(shown.project_id, "");
        assert!(shown.hosts.iter().any(|h| h.host == "crates.io" && h.source == Source::Default));
        assert!(reach(&baker, "example.org:443").unwrap_err().contains("not one of"), "refused, as before");
        let asked = |stream: &str| state.heart.lock().unwrap().events_of_family(stream, ASK).unwrap().len();
        assert_eq!((asked(&baker), asked(crate::catalog::BAKERY_OWN)), (0, 0), "never asked");

        let shown = answer("example.org", Some(true)).await.unwrap().0;
        let line = shown.hosts.iter().find(|h| h.host == "example.org").unwrap();
        assert!(line.allow && line.source == Source::Operator);
        assert_eq!(list(State(state.clone()), Query(own())).await.unwrap().0.hosts.len(), shown.hosts.len());
        assert!(reach(&baker, "example.org:443").is_ok() && reach(&orc, "example.org:443").is_ok());
        assert!(reach(&theirs.stream_id, "example.org:443").unwrap_err().contains("waits on the operator"), "a project's own list");
        let crates = |shown: &Hosts| -> Vec<(bool, Source)> {
            shown.hosts.iter().filter(|h| h.host == "index.crates.io").map(|h| (h.allow, h.source)).collect()
        };
        let denied = answer("index.crates.io", Some(false)).await.unwrap().0;
        assert_eq!(crates(&denied), [(false, Source::Operator)], "the answer, in the default's place");
        assert_eq!(reach(&baker, "index.crates.io:443").unwrap_err(), "the operator said no to index.crates.io for the Bakery");
        assert!(reach(&theirs.stream_id, "index.crates.io:443").is_ok());
        let forgotten = answer("index.crates.io", None).await.unwrap().0;
        assert_eq!(crates(&forgotten), [(true, Source::Default)]);
        assert!(reach(&baker, "index.crates.io:443").is_ok(), "forgotten: the default again");
        let gone = HostsQuery { project_id: "project_gone".into() };
        assert_eq!(list(State(state.clone()), Query(gone)).await.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn entries_are_host_names_or_patterns_never_addresses() {
        assert_eq!(entry(" Pub.Dev. ").unwrap(), "pub.dev");
        assert_eq!(entry("*.googleapis.com").unwrap(), "*.googleapis.com");
        for bad in [
            "localhost",
            "*.com",
            "10.0.0.1",
            "[::1]",
            "a..b",
            "-a.com",
            "a b.com",
            "pub.dev:443",
        ] {
            assert!(entry(bad).is_err(), "{bad}");
        }
        assert!(covers("*.googleapis.com", "storage.googleapis.com"));
        assert!(!covers("*.googleapis.com", "googleapis.com"));
        assert!(!covers("*.googleapis.com", "evilgoogleapis.com"));
    }
}
