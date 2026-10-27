//! The two programs a room runs in place of a push and of `gh`
//! (`remote_ask`): this node's own binary, started as `git-remote-bakery`
//! by git or as `gh` from the room's PATH. Each asks at its thread's door
//! (`BAKERY_DOOR_URL`) and says what the node answered.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn ask_node(body: &Value) -> Result<Value, String> {
    let url = std::env::var(crate::doors::URL_ENV).map_err(|_| "not in a Bakery room: nothing to ask".to_string())?;
    // A fresh process: its first connection waits out the Mac's check of it.
    crate::doors::first_connection(&url, crate::doors::FIRST_CONNECTION_WAIT)?;
    // Longer than a card waits for the operator (`api_tools::MCP_APPROVAL_WAIT`).
    let answer = ureq::post(&format!("{url}/remote/ask")).timeout(std::time::Duration::from_secs(120)).send_json(body.clone());
    match answer {
        Ok(response) => response.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(_, response)) => Err(response.into_string().unwrap_or_default()),
        Err(e) => Err(format!("the Bakery didn't answer: {e}")),
    }
}

/// The repository the room's git or gh works in, by its top folder.
fn repo_top() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// git's remote-helper protocol, for pushes only: git lists nothing from the
/// remote, sends its push lines, and gets one answer per ref.
pub(crate) fn git_remote(input: impl BufRead, mut output: impl Write, repo: &str, ask: &dyn Fn(&Value) -> Result<Value, String>) -> std::io::Result<()> {
    let mut lines = input.lines();
    while let Some(line) = lines.next() {
        let line = line?;
        match line.split_once(' ').map_or((line.as_str(), ""), |(a, b)| (a, b)) {
            ("capabilities", _) => output.write_all(b"push\noption\n\n")?,
            ("option", _) => output.write_all(b"unsupported\n")?,
            ("list", _) => output.write_all(b"\n")?,
            ("push", first) => {
                let mut specs = vec![first.to_string()];
                for next in lines.by_ref() {
                    let next = next?;
                    match next.strip_prefix("push ") {
                        Some(spec) => specs.push(spec.to_string()),
                        None => break,
                    }
                }
                let refs: Vec<Value> = specs
                    .iter()
                    .map(|spec| {
                        let (src, dst) = spec.split_once(':').unwrap_or((spec, spec));
                        json!({ "src": src.trim_start_matches('+'), "dst": dst, "force": src.starts_with('+') })
                    })
                    .collect();
                let answer = ask(&json!({ "kind": "push", "repo": repo, "refs": refs }));
                for (spec, r) in specs.iter().zip(&refs) {
                    let dst = r["dst"].as_str().unwrap_or(spec);
                    let one = answer.as_ref().ok().and_then(|a| a["refs"].as_array()?.iter().find(|x| x["dst"] == dst).cloned());
                    match (&answer, one) {
                        (_, Some(one)) if one["ok"] == true => {
                            eprintln!("bakery: {}", one["message"].as_str().unwrap_or_default());
                            writeln!(output, "ok {dst}")?;
                        }
                        (_, Some(one)) => writeln!(output, "error {dst} {}", one_line(one["message"].as_str().unwrap_or("refused")))?,
                        (Err(e), None) => writeln!(output, "error {dst} {}", one_line(e))?,
                        (Ok(a), None) => writeln!(output, "error {dst} {}", one_line(a["message"].as_str().unwrap_or("refused")))?,
                    }
                }
                output.write_all(b"\n")?;
            }
            ("", _) => break,
            _ => output.write_all(b"\n")?,
        }
        output.flush()?;
    }
    Ok(())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `gh`'s arguments with a body file read into --body, as the room sees it.
pub(crate) fn gh_args(args: &[String], read: &dyn Fn(&str) -> Result<String, String>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let file = match arg.as_str() {
            "-F" | "--body-file" => Some(it.next().ok_or_else(|| format!("{arg} needs a file"))?.clone()),
            other => other.strip_prefix("--body-file=").map(str::to_string),
        };
        match file {
            Some(file) => out.extend(["--body".to_string(), read(&file)?]),
            None => out.push(arg.clone()),
        }
    }
    Ok(out)
}

/// `git-remote-bakery <remote> <url>`, as git runs it for a push.
pub(crate) fn git_remote_main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo_top();
    let stdin = std::io::stdin();
    git_remote(stdin.lock(), std::io::stdout().lock(), &repo, &ask_node)?;
    Ok(())
}

/// `gh …` in a room.
pub(crate) fn gh_main() -> Result<(), Box<dyn std::error::Error>> {
    let given: Vec<String> = std::env::args().skip(1).collect();
    let read = |file: &str| -> Result<String, String> {
        let text = if file == "-" {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map(|_| text)
        } else {
            std::fs::read_to_string(file)
        };
        text.map_err(|e| format!("{file}: {e}"))
    };
    let answer = gh_args(&given, &read).and_then(|args| ask_node(&json!({ "kind": "gh", "repo": repo_top(), "args": args })));
    match answer {
        Ok(a) if a["ok"] == true => {
            println!("{}", a["url"].as_str().or(a["message"].as_str()).unwrap_or_default());
            Ok(())
        }
        Ok(a) => {
            eprintln!("{}", a["message"].as_str().unwrap_or("refused"));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn the_helper_speaks_gits_protocol_for_pushes() {
        let asked = RefCell::new(None);
        let ask = |body: &Value| {
            *asked.borrow_mut() = Some(body.clone());
            Ok(json!({ "ok": false, "refs": [
                { "dst": "refs/heads/w1/x", "ok": true, "message": "pushed" },
                { "dst": "refs/heads/main", "ok": false, "message": "waiting for the operator\nto approve" }
            ]}))
        };
        let input = "capabilities\noption progress true\nlist for-push\npush refs/heads/w1/x:refs/heads/w1/x\npush +HEAD:refs/heads/main\n\n\n";
        let mut out = Vec::new();
        git_remote(input.as_bytes(), &mut out, "/w", &ask).unwrap();
        let said = String::from_utf8(out).unwrap();
        assert_eq!(said, "push\noption\n\nunsupported\n\nok refs/heads/w1/x\nerror refs/heads/main waiting for the operator to approve\n\n");
        let asked = asked.into_inner().unwrap();
        assert_eq!(asked["repo"], "/w");
        assert_eq!(asked["refs"][1], json!({ "src": "HEAD", "dst": "refs/heads/main", "force": true }));
    }

    #[test]
    fn a_body_file_goes_in_as_the_body() {
        let read = |file: &str| if file == "notes.md" { Ok("the text".to_string()) } else { Err(format!("{file}: missing")) };
        let args: Vec<String> = ["pr", "create", "--title", "t", "-F", "notes.md"].map(String::from).to_vec();
        assert_eq!(gh_args(&args, &read).unwrap(), ["pr", "create", "--title", "t", "--body", "the text"]);
        let inline: Vec<String> = ["issue", "create", "--body-file=notes.md"].map(String::from).to_vec();
        assert_eq!(gh_args(&inline, &read).unwrap(), ["issue", "create", "--body", "the text"]);
        assert!(gh_args(&["-F".to_string(), "gone".to_string()], &read).is_err());
    }
}
