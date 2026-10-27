//! Shared prompt + response helpers for model impls (Claude, Grok, …).

pub const SYSTEM_EDIT_INSTRUCTIONS: &str = r#"You are inside The Bakery harness. Propose ONE file edit.

Rules:
- Output the COMPLETE new file contents only (full file replacement).
- Do not wrap the file in markdown fences unless the file itself contains them.
- Do not include commentary, diffs, or explanations outside the file body.
- If you must note a brief rationale, put it on the FIRST line as a comment appropriate to the file type, prefixed with "BAKERY:".
- Preserve existing style and only change what the instruction requires."#;

pub fn user_edit_prompt(instruction: &str, file_path: &str, current_content: &str) -> String {
    format!(
        "Instruction:\n{}\n\nTarget file path: {}\n\nCurrent file contents (may be empty if new):\n```\n{}\n```\n\nReturn the full new file contents only.",
        instruction.trim(),
        file_path.trim(),
        current_content
    )
}

/// Human label for a reasoning-effort level id (07d-A3D1 Phase B) — shared so
/// Claude/Grok (the two providers whose ids overlap: `low`/`medium`/`high`/
/// `xhigh`/`max`) don't each invent their own capitalization. Falls back to a
/// plain first-letter capitalization for any id outside that known set,
/// which only happens if a provider starts reporting a level name this
/// table doesn't know about yet — never a hard error over a labeling gap.
pub fn effort_label(id: &str) -> String {
    match id {
        "none" => "None".to_string(),
        "low" => "Low".to_string(),
        "medium" => "Medium".to_string(),
        "high" => "High".to_string(),
        "xhigh" => "Extra high".to_string(),
        "max" => "Max".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => other.to_string(),
            }
        }
    }
}

/// Turns a `ureq::get(..).call()` result into the response body text — on a
/// status >= 400, `ureq` itself already returns `Err(Error::Status(status,
/// response))` rather than `Ok`, and `Error`'s own `Display` impl renders
/// only `"<url>: status code <n>"`, dropping whatever diagnostic body the
/// provider actually sent (07d-A3D1 review: a naive `.map_err(|e|
/// format!("... request: {e}"))?` immediately after `.call()` never reaches
/// the code that would otherwise build a body snippet — that code is
/// unreachable dead code for every real HTTP error). `Error::Status` still
/// carries the original `Response`, which is the only place left to read
/// the body from.
pub fn read_models_response(
    result: Result<ureq::Response, ureq::Error>,
    provider_label: &str,
) -> Result<String, String> {
    match result {
        Ok(resp) => resp.into_string().map_err(|e| format!("{provider_label} models body: {e}")),
        Err(ureq::Error::Status(status, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            let snippet: String = text.chars().take(400).collect();
            Err(format!("{provider_label} models API {status}: {snippet}"))
        }
        Err(e) => Err(format!("{provider_label} models request: {e}")),
    }
}

pub fn strip_outer_fence(s: &str) -> String {
    let t = s.trim();
    if !t.starts_with("```") {
        return t.to_string();
    }
    let mut lines: Vec<&str> = t.lines().collect();
    if lines.first().is_some_and(|l| l.starts_with("```")) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|l| l.trim() == "```") {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fence() {
        let s = "```rust\nfn main() {}\n```";
        assert_eq!(strip_outer_fence(s), "fn main() {}");
    }

    #[test]
    fn effort_label_covers_the_known_ids() {
        assert_eq!(effort_label("low"), "Low");
        assert_eq!(effort_label("medium"), "Medium");
        assert_eq!(effort_label("high"), "High");
        assert_eq!(effort_label("xhigh"), "Extra high");
        assert_eq!(effort_label("max"), "Max");
        assert_eq!(effort_label("none"), "None");
    }

    #[test]
    fn effort_label_falls_back_to_capitalizing_an_unknown_id() {
        assert_eq!(effort_label("turbo"), "Turbo");
        assert_eq!(effort_label(""), "");
    }

    /// 07d-A3D1 review: a naive `.map_err(Display)?` right after `.call()`
    /// loses the real provider error body — this proves the fix actually
    /// surfaces it, against a real (fake, local) HTTP 401 response, not just
    /// a hand-built `ureq::Error` value.
    #[test]
    fn read_models_response_surfaces_the_real_error_body_on_a_4xx_status() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let body = r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
            let response = tiny_http::Response::from_string(body)
                .with_status_code(401)
                .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
            let _ = request.respond(response);
        });
        let result = ureq::get(&format!("http://{addr}")).call();
        let err = read_models_response(result, "Test").map(|_| ()).unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("invalid x-api-key"), "the real provider error body must survive: {err}");
        handle.join().unwrap();
    }

    #[test]
    fn read_models_response_reads_the_body_on_success() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let _ = request.respond(tiny_http::Response::from_string(r#"{"data":[]}"#));
        });
        let result = ureq::get(&format!("http://{addr}")).call();
        let text = read_models_response(result, "Test").unwrap();
        assert_eq!(text, r#"{"data":[]}"#);
        handle.join().unwrap();
    }
}
