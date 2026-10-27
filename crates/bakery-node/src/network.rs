//! Node-side network adapter for HTTP seats: `web_fetch` / `web_search` run
//! here, in bakery-node, as one explicit adapter, not as a second ad-hoc
//! `ureq` call site.
//!
//! They reach the web as their thread's own commands do ([`Wall`]): a thread
//! in the Mac sandbox or a container only the hosts its project's list
//! holds, a new one asked of the operator, as its door asks
//! (`host_list::verdict_for`); and every thread public addresses only, never
//! this Mac, its network or the tailnet (`seat_gateway::public`). Every
//! connection a fetch makes is judged, a redirect's too.

use crate::host_list::Kind;
use crate::machine_access::Reach;
use crate::state::AppState;
use serde_json::Value;
use std::io::Read;
use std::net::{SocketAddr, ToSocketAddrs};

const MAX_BODY_BYTES: u64 = 64 * 1024;
const FETCH_TIMEOUT_SECS: u64 = 15;

/// A web tool's call for `stream_id`'s agent; `None` for any other tool.
pub(crate) fn call(
    state: &AppState,
    stream_id: &str,
    name: &str,
    args: &Value,
) -> Option<Result<String, String>> {
    let run: fn(&Wall, &Value) -> Result<String, String> = match name {
        "web_fetch" => fetch_args,
        "web_search" => search_args,
        _ => return None,
    };
    Some(run(&Wall::of(state, stream_id), args))
}

/// What a thread's web tools may reach: what its own commands may.
#[derive(Clone)]
pub(crate) struct Wall {
    state: AppState,
    stream_id: String,
    /// The list a thread in the Mac sandbox or a container is held to; none
    /// where its commands reach the web freely.
    list: Option<Kind>,
}

impl Wall {
    pub(crate) fn of(state: &AppState, stream_id: &str) -> Wall {
        let list = match crate::machine_access::reach_for(state, stream_id) {
            Reach::Container | Reach::Locked => Some(Kind::Container),
            Reach::HostRoom => crate::mac_sandbox::mac_project(state, stream_id).map(|_| Kind::Mac),
            Reach::HostFull => None,
        };
        Wall {
            state: state.clone(),
            stream_id: stream_id.to_string(),
            list,
        }
    }

    /// Where a connection to `netloc` (`host:port`) may go: its host asked
    /// of the thread's list first, as its door asks, then its public
    /// addresses alone, IPv4 first, as the door dials them.
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let refused = |why: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, why);
        if let Some(kind) = self.list {
            // The operator's card names the tool that tried.
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::USER_AGENT,
                axum::http::HeaderValue::from_static("web_fetch"),
            );
            crate::host_list::verdict_for(&self.state, netloc, &self.stream_id, kind, &headers)
                .map_err(|(_, why)| refused(why))?;
        }
        let mut addrs: Vec<SocketAddr> = netloc
            .to_socket_addrs()?
            .filter(|a| crate::seat_gateway::public(a.ip()))
            .collect();
        addrs.sort_by_key(SocketAddr::is_ipv6);
        if addrs.is_empty() {
            return Err(refused(format!(
                "{netloc} has no public address: web tools never reach this Mac, its network or the tailnet"
            )));
        }
        Ok(addrs)
    }
}

pub(crate) fn fetch(wall: &Wall, url: &str) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("web_fetch only allows http(s) URLs".into());
    }
    let to = wall.clone();
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        // Through a proxy, the resolver would judge the proxy, not the host.
        .try_proxy_from_env(false)
        .resolver(move |netloc: &str| to.resolve(netloc))
        .build();
    let resp = agent
        .get(url)
        .set("User-Agent", "TheBakery/0.1 (web_fetch)")
        .call()
        .map_err(|e| match &e {
            // The wall's own words, not ureq's "Dns Failed" around them.
            ureq::Error::Transport(t) if t.kind() == ureq::ErrorKind::Dns => {
                std::error::Error::source(t)
                    .map_or_else(|| e.to_string(), |why| format!("{url}: {why}"))
            }
            _ => e.to_string(),
        })?;
    let mut body = String::new();
    resp.into_reader()
        .take(MAX_BODY_BYTES)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    Ok(body)
}

fn fetch_args(wall: &Wall, args: &Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("web_fetch needs \"url\"")?;
    fetch(wall, url)
}

/// Live HTML search via DuckDuckGo's public HTML endpoint. No API key.
pub(crate) fn search(wall: &Wall, query: &str) -> Result<String, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("web_search needs \"query\"".into());
    }
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding_lite(q)
    );
    let html = fetch(wall, &url)?;
    let results = parse_ddg_html(&html);
    if results.is_empty() {
        return Ok(format!("no results for {q:?}"));
    }
    Ok(results.join("\n"))
}

fn search_args(wall: &Wall, args: &Value) -> Result<String, String> {
    let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    search(wall, q)
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Pull title + href pairs from DuckDuckGo HTML (`result__a` anchors).
pub fn parse_ddg_html(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find("result__a") {
        rest = &rest[idx..];
        let Some(href_at) = rest.find("href=\"") else {
            break;
        };
        let after = &rest[href_at + 6..];
        let Some(href_end) = after.find('"') else {
            break;
        };
        let href = &after[..href_end];
        let Some(gt) = after.find('>') else { break };
        let after_gt = &after[gt + 1..];
        let Some(lt) = after_gt.find('<') else { break };
        let title = strip_tags(&after_gt[..lt]);
        if !title.is_empty() && !href.is_empty() {
            out.push(format!("{title}\n  {href}"));
        }
        rest = &after[href_end..];
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_unescape(out.trim())
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ddg_html_extracts_result_anchors() {
        let html = r#"<div><a rel="nofollow" class="result__a" href="https://example.com/rust">The Rust book</a></div>
<a class="result__a" href="https://doc.rust-lang.org">docs</a>"#;
        let hits = parse_ddg_html(html);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits[0].contains("The Rust book"), "{hits:?}");
        assert!(hits[0].contains("https://example.com/rust"), "{hits:?}");
        assert!(hits[1].contains("docs"), "{hits:?}");
    }

    /// A node of its own, and the wall of a thread on it with no project.
    fn open_wall(dir: &std::path::Path) -> Wall {
        let state = crate::testutil::state_with(dir, std::sync::Arc::new(bakery_agent::StubModel));
        Wall::of(&state, "wt:web")
    }

    #[test]
    fn fetch_rejects_non_http() {
        let dir = tempfile::tempdir().unwrap();
        let wall = open_wall(dir.path());
        assert!(fetch(&wall, "file:///etc/passwd").is_err());
        assert!(fetch(&wall, "ftp://example.com").is_err());
    }

    #[test]
    fn search_rejects_empty_query() {
        let dir = tempfile::tempdir().unwrap();
        assert!(search(&open_wall(dir.path()), "  ")
            .unwrap_err()
            .contains("query"));
    }

    /// A thread whose commands reach the web freely still never reaches
    /// this Mac through its web tools, by address or by name.
    #[test]
    fn an_open_threads_web_tools_never_reach_this_mac() {
        let dir = tempfile::tempdir().unwrap();
        let wall = open_wall(dir.path());
        assert_eq!(
            wall.list, None,
            "no project: its commands reach the web freely"
        );
        let here = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        here.set_nonblocking(true).unwrap();
        let port = here.local_addr().unwrap().port();
        for url in [
            format!("http://127.0.0.1:{port}/"),
            format!("http://localhost:{port}/"),
        ] {
            let refused = fetch(&wall, &url).unwrap_err();
            assert!(refused.contains("no public address"), "{url}: {refused}");
        }
        assert!(here.accept().is_err(), "nothing reached this Mac");
    }

    /// A thread in the Mac sandbox reaches through its web tools only what
    /// its project's list holds: a new host waits on the operator, a card in
    /// the thread naming the tool, as its door would ask.
    #[test]
    fn a_mac_sandbox_threads_new_host_asks_the_operator() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (_, thread) = crate::catalog::create_project(&state, "shop", Some(root)).unwrap();
        let wall = Wall::of(&state, &thread.stream_id);
        assert_eq!(
            wall.list,
            Some(Kind::Mac),
            "a new project runs in the Mac sandbox"
        );
        let held = fetch(&wall, "https://offlist.example/").unwrap_err();
        assert!(held.contains("waits on the operator"), "{held}");
        let asks = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&thread.stream_id, crate::host_list::ASK)
            .unwrap();
        assert_eq!(asks.len(), 1, "one card in the thread");
        let card: serde_json::Value = serde_json::from_str(&asks[0].payload).unwrap();
        assert_eq!(
            (card["host"].as_str(), card["program"].as_str()),
            (Some("offlist.example"), Some("web_fetch"))
        );
    }
}
