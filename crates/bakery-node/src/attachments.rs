//! Node-owned composer attachments.
//!
//! Upload bytes never enter Heart. Heart records only opaque id + safe
//! metadata, while bytes live below the node's private base directory. This
//! uses a bounded data URL instead of a multipart dependency so the desktop
//! picker flow has no network-installed server dependency.

use crate::state::{append, context_for, AppState};
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

pub(crate) const ATTACHMENT_UPLOADED: &str = "agent.attachment.uploaded";
/// Largest file the composer uploads: as large as a file the provider's API
/// itself takes (Anthropic's Files API, 500 MB). A native runtime gets a
/// file by its path, so its size never meets a request's limit.
const MAX_ATTACHMENT_BYTES: usize = 500 * 1024 * 1024;
/// The upload body carries the bytes as a base64 data URL inside JSON.
pub(crate) const MAX_UPLOAD_BODY_BYTES: usize = MAX_ATTACHMENT_BYTES / 3 * 4 + 64 * 1024;
/// Text files inline into one turn up to this many bytes in all; past it a
/// file is cut, with a line saying so (and where the whole is, for a native
/// runtime) — never refused.
const MAX_TEXT_CONTEXT_BYTES: usize = 1024 * 1024;
/// The most files one turn sends as themselves (Anthropic's API takes 100
/// images a request); past it a file goes by its path alone.
const MAX_NATIVE_FILES: usize = 100;
/// What one turn sends as itself, in all, counted as it goes (base64, a
/// third bigger than the file): under its provider's request limit, past
/// which a file goes by its path alone (`inline_budget`).
const GEMINI_INLINE_BYTES: usize = 18 * 1024 * 1024;
const OTHER_INLINE_BYTES: usize = 24 * 1024 * 1024;

/// A provider's inline budget, by its model: Gemini's request takes 20 MB
/// inline, Claude's 32 MB, with room left for the rest of the request.
pub(crate) fn inline_budget(model: &str) -> usize {
    match model.to_ascii_lowercase().contains("gemini") {
        true => GEMINI_INLINE_BYTES,
        false => OTHER_INLINE_BYTES,
    }
}

/// A file handed to a provider as itself (an image, a PDF) stays under this,
/// inside every provider's request limit (Gemini's 20 MB is the lowest).
const MAX_NATIVE_BYTES: usize = 8 * 1024 * 1024;
/// Claude Code's own bound on an image it sends (its resizer's `maxWidth`,
/// `maxHeight` and `targetRawSize`): past 2000 px a side the API refuses an
/// image once a request carries more than 20, and past 5 MB of base64 always.
const IMAGE_MAX_EDGE: u32 = 2000;
const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024 / 4 * 3;
/// The image types whose size `image_size` reads.
const SIZED_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// How a turn's attachments open, for a turn and for a steer alike.
pub(crate) const ATTACHED_PREAMBLE: &str = "The operator attached the following. It is not a workspace file; do not edit it unless the operator separately asks you to create a workspace file.";

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AttachmentRef {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) media_type: String,
    pub(crate) bytes: usize,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct AttachmentUploadReq {
    stream_id: String,
    seat: crate::node::Seat,
    filename: String,
    #[serde(default)]
    media_type: String,
    data_url: String,
    /// The thread's own file it was read from (an @-mention, the selected
    /// file): a file the node keeps secret is refused, as agents never get one.
    #[serde(default)]
    source_path: Option<String>,
}

fn safe_filename(value: &str) -> String {
    let leaf = std::path::Path::new(value)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment");
    let out: String = leaf
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(160)
        .collect();
    if out.is_empty() {
        "attachment".to_string()
    } else {
        out
    }
}

fn safe_media_type(value: &str) -> String {
    let out: String = value
        .chars()
        .filter(|c| c.is_ascii_graphic() && *c != '"' && *c != '\\')
        .take(120)
        .collect();
    if out.is_empty() {
        "application/octet-stream".to_string()
    } else {
        out
    }
}

/// The folder a stream's runtimes read its attachments from: written by the
/// node, read-only to every room of the stream, on this Mac and in a
/// container alike (mounted at the same path).
pub(crate) fn room_dir(state: &AppState, stream_id: &str) -> std::path::PathBuf {
    let piece: String = stream_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    crate::state::cache_root(state).join("attachments").join(piece)
}

/// `bytes` at `path`, whole or not at all: written beside it, then renamed
/// over it, so a crash never leaves half a file that a later reader trusts.
pub(crate) fn write_whole(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let partial = path.with_file_name(format!(".{name}.{}-{n}.partial", std::process::id()));
    let written = std::fs::write(&partial, bytes).and_then(|()| std::fs::rename(&partial, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written
}

/// Each file written into `room` once, in a folder named for its content and
/// under its own name: where each can be read, in order.
pub(crate) fn into_room(room: &std::path::Path, files: &[bakery_agent::NativeAttachment]) -> Vec<Option<String>> {
    files
        .iter()
        .map(|file| {
            let dir = room.join(&blake3::hash(&file.data).to_hex()[..16]);
            let path = dir.join(safe_filename(&file.filename));
            if !path.is_file() {
                std::fs::create_dir_all(&dir).ok()?;
                write_whole(&path, &file.data).ok()?;
            }
            Some(path.to_string_lossy().into_owned())
        })
        .collect()
}

fn attachment_path(state: &AppState, id: &str) -> Result<std::path::PathBuf, String> {
    if !id.starts_with("attach_") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("invalid attachment id".to_string());
    }
    Ok(state.base.join("attachments").join(id))
}

fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Strict, whitespace-free standard base64 decoder. Attachment URLs are
/// browser-generated, so accepting URL-safe variants or arbitrary whitespace
/// would only enlarge the parser surface without helping a native picker.
pub(crate) fn decode_base64(encoded: &str) -> Result<Vec<u8>, String> {
    let raw = encoded.as_bytes();
    if raw.is_empty() || raw.len() % 4 != 0 {
        return Err("attachment data is not valid base64".to_string());
    }
    let padding = raw.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 || raw[..raw.len() - padding].iter().any(|&b| b == b'=') {
        return Err("attachment data has invalid base64 padding".to_string());
    }
    let mut out = Vec::with_capacity(raw.len() / 4 * 3 - padding);
    for group in raw.chunks_exact(4) {
        let a = b64_value(group[0])
            .ok_or_else(|| "attachment data is not valid base64".to_string())?
            as u32;
        let b = b64_value(group[1])
            .ok_or_else(|| "attachment data is not valid base64".to_string())?
            as u32;
        let c = if group[2] == b'=' {
            0
        } else {
            b64_value(group[2]).ok_or_else(|| "attachment data is not valid base64".to_string())?
                as u32
        };
        let d = if group[3] == b'=' {
            0
        } else {
            b64_value(group[3]).ok_or_else(|| "attachment data is not valid base64".to_string())?
                as u32
        };
        let word = (a << 18) | (b << 12) | (c << 6) | d;
        out.push((word >> 16) as u8);
        if group[2] != b'=' {
            out.push((word >> 8) as u8);
        }
        if group[3] != b'=' {
            out.push(word as u8);
        }
    }
    Ok(out)
}

fn data_url_bytes(value: &str) -> Result<(Option<String>, Vec<u8>), String> {
    let rest = value
        .strip_prefix("data:")
        .ok_or_else(|| "attachment must be a data URL".to_string())?;
    let (header, body) = rest
        .split_once(',')
        .ok_or_else(|| "attachment data URL is incomplete".to_string())?;
    let media = header
        .strip_suffix(";base64")
        .ok_or_else(|| "attachment data URL must be base64 encoded".to_string())?;
    let bytes = decode_base64(body)?;
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!(
            "attachments are limited to {} each",
            human_bytes(MAX_ATTACHMENT_BYTES)
        ));
    }
    Ok(((!media.trim().is_empty()).then(|| media.to_string()), bytes))
}

/// `POST /agent/attachments`, supplied by the platform native picker. The
/// client sends a data URL only after the picker has converted its local Blob;
/// filesystem paths are never sent across the webview/node boundary.
pub(crate) async fn upload_attachment(
    State(state): State<AppState>,
    Json(req): Json<AttachmentUploadReq>,
) -> Result<Json<AttachmentRef>, (StatusCode, String)> {
    context_for(&state, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if let Some(source) = req.source_path.as_deref() {
        let jail = crate::state::jail_for(&state, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        let secret = jail
            .resolve_read(source)
            .map_or(true, |real| crate::machine_access::is_secret(&state, &real, jail.root()));
        if secret {
            return Err((
                StatusCode::FORBIDDEN,
                format!("{source} is a secret the node keeps from agents: it can't be attached"),
            ));
        }
    }
    let (from_url, bytes) =
        data_url_bytes(&req.data_url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "empty attachments are not supported".to_string(),
        ));
    }
    let reference = AttachmentRef {
        id: crate::node::mint_id("attach"),
        filename: safe_filename(&req.filename),
        media_type: safe_media_type(from_url.as_deref().unwrap_or(&req.media_type)),
        bytes: bytes.len(),
    };
    let path = attachment_path(&state, &reference.id).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    std::fs::create_dir_all(path.parent().expect("attachment path has parent")).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("create attachment store: {e}"),
        )
    })?;
    write_whole(&path, &bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("store attachment: {e}"),
        )
    })?;

    let agent_id = crate::node::agent_id(&req.stream_id, req.seat);
    append(
        &state,
        &req.stream_id,
        "human",
        ATTACHMENT_UPLOADED,
        serde_json::json!({
            "agent_id": agent_id,
            "seat": req.seat,
            "id": reference.id,
            "filename": reference.filename,
            "media_type": reference.media_type,
            "bytes": reference.bytes,
        })
        .to_string(),
        vec![],
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(reference))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct AttachmentQuery {
    pub(crate) stream_id: String,
    pub(crate) id: String,
}

/// `GET /attachment?stream_id&id` — an attachment sent on that thread, as
/// it was sent, with its media type: the operator's (phones too), to open a
/// sent file or show an image. Only an upload the thread itself recorded.
pub(crate) async fn get_attachment(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AttachmentQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "attachments open for the operator".to_string()));
    }
    let (media_type, filename, bytes) = crate::actor::spawn_blocking(move || attachment_bytes(&state, &q))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let disposition = format!("inline; filename=\"{}\"", filename.replace(['"', '\\'], "_"));
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, media_type)
        .header(axum::http::header::CONTENT_DISPOSITION, disposition)
        .header("x-content-type-options", "nosniff")
        // A sent page or SVG opens as a document of its own, never script
        // in the node's origin.
        .header(axum::http::header::CONTENT_SECURITY_POLICY, "sandbox")
        .body(axum::body::Body::from(bytes))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn attachment_bytes(state: &AppState, q: &AttachmentQuery) -> Result<(String, String, Vec<u8>), String> {
    let path = attachment_path(state, &q.id)?;
    let upload = state
        .heart
        .lock()
        .unwrap()
        .events_of_family(&q.stream_id, ATTACHMENT_UPLOADED)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).ok())
        .find(|p| p["id"] == q.id.as_str())
        .ok_or_else(|| format!("no attachment {} on this thread", q.id))?;
    let bytes = std::fs::read(&path).map_err(|_| format!("attachment {} is no longer kept", q.id))?;
    let text = |key: &str| upload[key].as_str().unwrap_or_default().to_string();
    Ok((safe_media_type(&text("media_type")), safe_filename(&text("filename")), bytes))
}

fn supports_text(media_type: &str) -> bool {
    let m = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    m.starts_with("text/")
        || matches!(
            m.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-yaml"
        )
}

fn attachment_refs_for_agent(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
) -> Result<std::collections::HashMap<String, AttachmentRef>, String> {
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;
    let mut found = std::collections::HashMap::<String, AttachmentRef>::new();
    for event in events {
        if event.family != ATTACHMENT_UPLOADED {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload) else {
            continue;
        };
        if payload.get("agent_id").and_then(|v| v.as_str()) != Some(agent_id) {
            continue;
        }
        let (Some(id), Some(filename), Some(media_type)) = (
            payload.get("id").and_then(|v| v.as_str()),
            payload.get("filename").and_then(|v| v.as_str()),
            payload.get("media_type").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let bytes = payload.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        found.insert(
            id.to_string(),
            AttachmentRef {
                id: id.to_string(),
                filename: filename.to_string(),
                media_type: media_type.to_string(),
                bytes,
            },
        );
    }
    Ok(found)
}

/// Resolve opaque, agent-scoped ids to in-memory native content immediately
/// before dispatch.  No host path crosses the model seam, and Heart continues
/// to contain metadata only.
pub(crate) fn native_attachments_for_turn(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
    ids: &[String],
) -> Result<Vec<bakery_agent::NativeAttachment>, String> {
    let found = attachment_refs_for_agent(state, stream_id, agent_id)?;
    ids.iter()
        .map(|id| {
            let attachment = found
                .get(id)
                .ok_or_else(|| format!("attachment {id} is not available for this agent"))?;
            let data = std::fs::read(attachment_path(state, id)?)
                .map_err(|e| format!("read attachment {}: {e}", attachment.filename))?;
            if data.len() != attachment.bytes || data.len() > MAX_ATTACHMENT_BYTES {
                return Err(format!(
                    "attachment {} changed or exceeds the {} limit",
                    attachment.filename,
                    human_bytes(MAX_ATTACHMENT_BYTES)
                ));
            }
            Ok(bakery_agent::NativeAttachment {
                filename: attachment.filename.clone(),
                mime_type: attachment.media_type.clone(),
                data,
            })
        })
        .collect()
}

/// What a turn's attachments become for one model.
#[derive(Debug, Default)]
pub(crate) struct TurnAttachments {
    /// What the model reads as text: each text file inline, a line for each
    /// file sent as itself, and a line naming every file it cannot open —
    /// so no attachment is ever silently dropped.
    pub(crate) text: String,
    /// Files handed to the provider as they are (images; PDFs where taken).
    pub(crate) native: Vec<bakery_agent::NativeAttachment>,
}

impl TurnAttachments {
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty() && self.native.is_empty()
    }
}

/// "1.2 MB"-style size for a note a model reads.
fn human_bytes(bytes: usize) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} bytes"),
    }
}

/// The media type without parameters, lowercased: what the native lists name.
fn media_essence(media_type: &str) -> String {
    media_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
}

/// A file reads as text when its type says so or, typeless as code and logs
/// often arrive, when its bytes are UTF-8 with no NUL in their first 8 KiB.
fn text_of<'a>(media_type: &str, data: &'a [u8]) -> Option<&'a str> {
    let text = std::str::from_utf8(data).ok()?;
    (supports_text(media_type) || !data[..data.len().min(8192)].contains(&0)).then_some(text)
}

/// Whether a file a model takes goes to it as itself: a PNG, JPEG, GIF or
/// WebP when it reads as one within Claude Code's image bound (a larger one
/// still reaches a native runtime by its path), anything else within the
/// request bound.
fn fits_as_itself(media: &str, data: &[u8]) -> bool {
    if !SIZED_IMAGES.contains(&media) {
        return data.len() <= MAX_NATIVE_BYTES;
    }
    data.len() <= IMAGE_MAX_BYTES && image_size(data).is_some_and(|(w, h)| w.max(h) <= IMAGE_MAX_EDGE)
}

/// An image's width and height from its header (PNG, JPEG, GIF, WebP).
fn image_size(data: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes(data.get(i..i + 2)?.try_into().ok()?) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes(data.get(i..i + 2)?.try_into().ok()?) as u32);
    let le24 = |i: usize| data.get(i..i + 3).map(|b| b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16);
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let be32 = |i: usize| Some(u32::from_be_bytes(data.get(i..i + 4)?.try_into().ok()?));
        return Some((be32(16)?, be32(20)?));
    }
    if data.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        return match data.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let bits = u32::from_le_bytes(data.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            _ => None,
        };
    }
    if data.starts_with(&[0xff, 0xd8]) {
        // Segment by segment to the frame header (SOF0–SOF15 but DHT, JPG
        // and DAC), which holds height then width.
        let mut i = 2;
        loop {
            while data.get(i..i + 2) == Some(&[0xff, 0xff]) {
                i += 1;
            }
            let marker = *data.get(i + 1).filter(|_| data[i] == 0xff)?;
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            if marker == 0xda {
                return None;
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

/// Turn files into model input: a type `is_native` names goes as itself, a
/// text file inline within the turn's text budget (cut past it), and any
/// other file as a line naming it. Where a file can be read (`at`, for a
/// native runtime), its line says so, so nothing is out of its reach. Every
/// file is accepted; none fails a turn or a steer.
#[cfg(test)]
pub(crate) fn resolve_files(
    files: Vec<bakery_agent::NativeAttachment>,
    at: &[Option<String>],
    is_native: &dyn Fn(&str) -> bool,
) -> TurnAttachments {
    resolve_files_within(files, at, is_native, GEMINI_INLINE_BYTES)
}

/// [`resolve_files`], within a provider's `inline` budget (`inline_budget`).
pub(crate) fn resolve_files_within(
    files: Vec<bakery_agent::NativeAttachment>,
    at: &[Option<String>],
    is_native: &dyn Fn(&str) -> bool,
    inline: usize,
) -> TurnAttachments {
    let mut out = TurnAttachments::default();
    let mut budget = MAX_TEXT_CONTEXT_BYTES;
    let mut native_bytes = 0;
    for (i, mut file) in files.into_iter().enumerate() {
        let media = media_essence(&file.mime_type);
        let size = file.data.len();
        let path = at.get(i).cloned().flatten();
        let at_path = path.as_deref().map(|p| format!(" — at {p}")).unwrap_or_default();
        let as_itself = is_native(&media) && fits_as_itself(&media, &file.data);
        let encoded = size.div_ceil(3) * 4;
        let room = out.native.len() < MAX_NATIVE_FILES && native_bytes + encoded <= inline;
        if as_itself && room {
            native_bytes += encoded;
            let kind = if media.starts_with("image/") { "image" } else { "file" };
            out.text.push_str(&format!(
                "\n\n[Attached {kind}: {}{at_path} — sent with this message]",
                file.filename
            ));
            file.mime_type = media;
            out.native.push(file);
        } else if let Some(text) = text_of(&media, &file.data) {
            out.text.push_str(&format!("\n\n[Attached file: {}{at_path}]\n", file.filename));
            if text.len() <= budget {
                out.text.push_str(text);
                budget -= text.len();
            } else {
                let mut cut = budget;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.text.push_str(&text[..cut]);
                let whole = path.as_deref().map(|p| format!("; read the whole from {p}")).unwrap_or_else(|| " and was cut".to_string());
                out.text.push_str(&format!(
                    "\n[… the rest of {} ({}) is past this turn's {} of attached text{whole}]",
                    file.filename,
                    human_bytes(text.len() - cut),
                    human_bytes(MAX_TEXT_CONTEXT_BYTES)
                ));
                budget = 0;
            }
        } else if let Some(path) = &path {
            out.text.push_str(&format!(
                "\n\n[Attached file: {} ({media}, {}) — at {path}: open it with your tools]",
                file.filename,
                human_bytes(size)
            ));
        } else {
            let why = if !is_native(&media) {
                "This model cannot open this kind of file directly.".to_string()
            } else if as_itself {
                format!(
                    "It is past what one message sends as itself ({MAX_NATIVE_FILES} files, {} in all).",
                    human_bytes(inline / 4 * 3)
                )
            } else if SIZED_IMAGES.contains(&media.as_str()) {
                format!(
                    "It is larger than a model takes as an image ({IMAGE_MAX_EDGE} px a side, {}), or does not read as one.",
                    human_bytes(IMAGE_MAX_BYTES)
                )
            } else {
                format!("It is too large to send as a file (over {}).", human_bytes(MAX_NATIVE_BYTES))
            };
            out.text.push_str(&format!(
                "\n\n[Attached file: {} ({media}, {}). {why} Its content is not included; if you need it, ask the operator for a text or image version.]",
                file.filename,
                human_bytes(size)
            ));
        }
    }
    out
}

/// Resolve queued attachment ids *at turn time* — queue recovery stays safe
/// without user bytes in Heart — into what this model reads (see
/// [`resolve_files`]). A file the store no longer has is named, not fatal.
pub(crate) fn turn_attachments(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
    ids: &[String],
    is_native: &dyn Fn(&str) -> bool,
    inline: usize,
) -> TurnAttachments {
    if ids.is_empty() {
        return TurnAttachments::default();
    }
    let found = attachment_refs_for_agent(state, stream_id, agent_id).unwrap_or_default();
    let mut files = Vec::new();
    let mut missing = String::new();
    for id in ids {
        let read = found.get(id).ok_or_else(|| "it is not in this conversation's store".to_string()).and_then(|a| {
            let data = std::fs::read(attachment_path(state, id)?).map_err(|e| e.to_string())?;
            Ok(bakery_agent::NativeAttachment {
                filename: a.filename.clone(),
                mime_type: a.media_type.clone(),
                data,
            })
        });
        match read {
            Ok(file) => files.push(file),
            Err(why) => missing.push_str(&format!("\n\n[An attached file ({id}) could not be read: {why}.]")),
        }
    }
    let mut out = resolve_files_within(files, &[], is_native, inline);
    out.text.push_str(&missing);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::{make_repo, open_req, state_with, StubModel};
    use std::sync::Arc;

    #[test]
    fn strict_data_url_decoder_accepts_picker_style_text_and_rejects_bad_padding() {
        let (media, bytes) = data_url_bytes("data:text/plain;base64,aGVsbG8=").unwrap();
        assert_eq!(media.as_deref(), Some("text/plain"));
        assert_eq!(bytes, b"hello");
        assert!(supports_text("application/json;charset=utf-8"));
        assert!(data_url_bytes("data:text/plain;base64,aGVsbG8").is_err());
        assert!(data_url_bytes("data:text/plain;base64,aG=sbG8=").is_err());
    }

    #[tokio::test]
    async fn uploaded_text_is_retrievable_by_opaque_id_but_never_stored_in_heart_payload() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "attachment"))
            .unwrap()
            .context
            .stream_id;
        let reference = upload_attachment(
            State(state.clone()),
            Json(AttachmentUploadReq {
                stream_id: stream_id.clone(),
                seat: crate::node::Seat::Orchestrator,
                filename: "notes.txt".to_string(),
                media_type: "text/plain".to_string(),
                data_url: "data:text/plain;base64,cHJpdmF0ZSBub3Rl".to_string(),
                source_path: Some("f.txt".to_string()),
            }),
        )
        .await
        .unwrap()
        .0;
        // A file the node keeps secret never goes to an agent this way either.
        std::fs::write(repo.join(".env"), "TOKEN=synthetic").unwrap();
        let secret = upload_attachment(
            State(state.clone()),
            Json(AttachmentUploadReq {
                stream_id: stream_id.clone(),
                seat: crate::node::Seat::Orchestrator,
                filename: ".env".to_string(),
                media_type: "text/plain".to_string(),
                data_url: "data:text/plain;base64,VE9LRU49c3ludGhldGlj".to_string(),
                source_path: Some(".env".to_string()),
            }),
        )
        .await;
        assert_eq!(secret.unwrap_err().0, StatusCode::FORBIDDEN);
        let agent_id = crate::node::agent_id(&stream_id, crate::node::Seat::Orchestrator);
        let context =
            turn_attachments(&state, &stream_id, &agent_id, &[reference.id.clone()], &|_| false, GEMINI_INLINE_BYTES);
        assert!(context.text.contains("private note"));
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let upload = events
            .iter()
            .find(|event| event.family == ATTACHMENT_UPLOADED)
            .unwrap();
        assert!(upload.payload.contains(&reference.id));
        assert!(!upload.payload.contains("private note"));

        // The operator opens it as sent, from its own thread only.
        let query = |stream: &str| AttachmentQuery { stream_id: stream.to_string(), id: reference.id.clone() };
        let opened = get_attachment(State(state.clone()), axum::extract::Query(query(&stream_id))).await.unwrap();
        assert_eq!(opened.headers()["content-type"], "text/plain");
        assert_eq!(opened.headers()["content-security-policy"], "sandbox");
        let body = axum::body::to_bytes(opened.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&body[..], b"private note");
        let elsewhere = get_attachment(State(state.clone()), axum::extract::Query(query("wt:other"))).await;
        assert_eq!(elsewhere.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    fn file(name: &str, media: &str, data: &[u8]) -> bakery_agent::NativeAttachment {
        bakery_agent::NativeAttachment {
            filename: name.to_string(),
            mime_type: media.to_string(),
            data: data.to_vec(),
        }
    }

    /// A file is written whole, then in place: nothing partial stays beside it.
    #[test]
    fn a_file_is_written_whole_or_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kept.json");
        write_whole(&path, b"first").unwrap();
        write_whole(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no partial file left");
        assert!(write_whole(&dir.path().join("missing/kept.json"), b"x").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// A PNG header of `w` × `h`: all `image_size` reads.
    fn png(w: u32, h: u32) -> Vec<u8> {
        [&b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"[..], &w.to_be_bytes(), &h.to_be_bytes()].concat()
    }

    /// An image goes as itself only when it reads as one within Claude Code's
    /// own bound; past it a native runtime still has the whole by its path,
    /// and a model with no path hears why.
    #[test]
    fn an_image_goes_as_itself_only_within_claude_codes_bound() {
        assert_eq!(image_size(&png(2000, 10)), Some((2000, 10)));
        assert_eq!(image_size(b"GIF89a\x20\x03\x58\x02"), Some((800, 600)));
        let jpeg = [&[0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc2, 0, 11, 8][..], &3000u16.to_be_bytes(), &4000u16.to_be_bytes()].concat();
        assert_eq!(image_size(&jpeg), Some((4000, 3000)));
        let webp = [&b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0"[..], &[0xcf, 0x07, 0, 0x0f, 0, 0]].concat();
        assert_eq!(image_size(&webp), Some((2000, 16)));

        let room = tempfile::tempdir().unwrap();
        let files = vec![
            file("wide.png", "image/png", &png(2001, 10)),
            file("broken.png", "image/png", b"\x89PNG"),
            file("ok.png", "image/png", &png(2000, 2000)),
        ];
        let at = into_room(room.path(), &files);
        let out = resolve_files(files, &at, &|m| m == "image/png");
        assert_eq!(out.native.iter().map(|f| f.filename.as_str()).collect::<Vec<_>>(), ["ok.png"]);
        assert!(out.text.contains(&format!("wide.png (image/png, 24 bytes) — at {}", at[0].as_deref().unwrap())), "{}", out.text);
        assert!(out.text.contains(&format!("broken.png (image/png, 4 bytes) — at {}", at[1].as_deref().unwrap())), "{}", out.text);
        let out = resolve_files(vec![file("wide.png", "image/png", &png(2001, 10))], &[], &|m| m == "image/png");
        assert!(out.text.contains("larger than a model takes as an image (2000 px a side"), "{}", out.text);
    }

    /// The operator's rule: files, images and screenshots are always accepted.
    /// Nothing here fails: a screenshot goes as itself to a model that reads
    /// images and is named to one that does not; typeless code reads as
    /// text; a PDF or an archive the model cannot open is named, never
    /// dropped and never fatal.
    #[test]
    fn every_kind_of_file_resolves_and_none_is_refused() {
        let reads_png = |media: &str| media == "image/png";
        let out = resolve_files(
            vec![
                file("shot.png", "image/png", &png(1, 1)),
                file("main.rs", "application/octet-stream", b"fn main() {}\n"),
                file("notes.md", "text/markdown; charset=utf-8", b"# notes"),
                file("spec.pdf", "application/pdf", b"%PDF-1.7\x00\x01"),
                file("bundle.zip", "application/zip", b"PK\x03\x04\x00"),
            ],
            &[],
            &reads_png,
        );
        assert_eq!(out.native.len(), 1);
        assert_eq!(out.native[0].filename, "shot.png");
        assert!(out.text.contains("[Attached image: shot.png — sent with this message]"), "{}", out.text);
        assert!(out.text.contains("[Attached file: main.rs]\nfn main() {}"), "{}", out.text);
        assert!(out.text.contains("# notes"), "{}", out.text);
        assert!(out.text.contains("spec.pdf (application/pdf, 10 bytes). This model cannot open"), "{}", out.text);
        assert!(out.text.contains("bundle.zip (application/zip"), "{}", out.text);

        // The same screenshot to a text-only model: named, not refused.
        let text_only = resolve_files(vec![file("shot.png", "image/png", &png(1, 1))], &[], &|_| false);
        assert!(text_only.native.is_empty());
        assert!(text_only.text.contains("shot.png (image/png"), "{}", text_only.text);
    }

    /// Past the turn's text budget a file is cut with a line saying so — the
    /// turn still runs (it used to fail outright).
    #[test]
    fn text_past_the_turn_budget_is_cut_not_refused() {
        let big = "a".repeat(MAX_TEXT_CONTEXT_BYTES + 10);
        let out = resolve_files(
            vec![file("big.log", "text/plain", big.as_bytes()), file("after.txt", "text/plain", b"later")],
            &[],
            &|_| false,
        );
        assert!(out.text.contains("the rest of big.log (10 bytes) is past this turn's 1.0 MB of attached text and was cut"), "{}", &out.text[out.text.len() - 300..]);
        assert!(out.text.contains("[Attached file: after.txt]"), "the next file is still named");
        assert!(out.text.len() < MAX_TEXT_CONTEXT_BYTES + 1_000);
    }

    #[test]
    fn a_native_file_over_the_request_bound_is_named_instead() {
        let huge = vec![0u8; MAX_NATIVE_BYTES + 1];
        let out = resolve_files(vec![file("scan.pdf", "application/pdf", &huge)], &[], &|m| m == "application/pdf");
        assert!(out.native.is_empty());
        assert!(out.text.contains("too large to send as a file"), "{}", out.text);
    }

    /// A turn sends files as themselves only up to its provider's budget,
    /// counted as base64; the rest are named with the path to open them by.
    #[test]
    fn one_turn_sends_at_most_its_byte_budget_as_itself() {
        let room = tempfile::tempdir().unwrap();
        let big = vec![0u8; 4 * 1024 * 1024];
        let files: Vec<_> = (0..6).map(|n| file(&format!("scan{n}.pdf"), "application/pdf", &[&big[..], &[n as u8]].concat())).collect();
        let at = into_room(room.path(), &files);
        let out = resolve_files(files, &at, &|m| m == "application/pdf");
        assert_eq!(out.native.len(), 3, "4 MiB each, 5.3 MiB as base64: at most 18 MiB of it for Gemini");
        let files: Vec<_> = (0..6).map(|n| file(&format!("scan{n}.pdf"), "application/pdf", &[&big[..], &[n as u8]].concat())).collect();
        assert_eq!(resolve_files_within(files, &at, &|m| m == "application/pdf", inline_budget("claude-native")).native.len(), 4, "24 MiB of it for Claude");
        assert!(out.text.contains("scan5.pdf (application/pdf") && out.text.contains("open it with your tools"), "{}", out.text);
    }

    /// For a native runtime every file is written where its room reads it,
    /// and its line gives the path: a PDF or a notebook it would otherwise
    /// only hear named, the whole of a text past the budget, and an image
    /// sent as itself too.
    #[test]
    fn a_native_runtime_gets_every_file_by_its_path() {
        let room = tempfile::tempdir().unwrap();
        let big = "b".repeat(MAX_TEXT_CONTEXT_BYTES + 10);
        let files = vec![
            file("shot.png", "image/png", &png(1, 1)),
            file("spec.pdf", "application/pdf", b"%PDF-1.7\x00\x01"),
            file("run.ipynb", "application/x-ipynb+json", b"{\"cells\": []}"),
            file("big.log", "text/plain", big.as_bytes()),
        ];
        let at = into_room(room.path(), &files);
        assert!(at.iter().all(|p| p.as_ref().is_some_and(|p| std::path::Path::new(p).is_file())), "each written: {at:?}");
        assert_eq!(std::fs::read(at[1].as_ref().unwrap()).unwrap(), b"%PDF-1.7\x00\x01", "as it was");
        let out = resolve_files(files, &at, &|m| m == "image/png");
        assert_eq!(out.native.len(), 1, "the image still goes as itself");
        for (i, name) in ["shot.png", "spec.pdf", "run.ipynb"].into_iter().enumerate() {
            assert!(out.text.contains(&format!("{name}") ) && out.text.contains(at[i].as_deref().unwrap()), "{name}: {}", out.text);
        }
        assert!(out.text.contains(&format!("read the whole from {}", at[3].as_deref().unwrap())), "the rest of a long text");
        assert_eq!(into_room(room.path(), &[file("spec.pdf", "application/pdf", b"%PDF-1.7\x00\x01")])[0], at[1], "the same file, once");
    }
}
