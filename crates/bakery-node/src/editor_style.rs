//! The operator's editor (openvscode-server, the "Open in Code" escape
//! hatch) in the Bakery's look, as the operator chose it: its chrome hidden
//! behind one menu button, its own git panel, terminal and search out of
//! the way (the Bakery has its own; ⌘J and ⌘⇧F are the Bakery's), and the
//! colors of whichever scheme the app shows, live. It goes through the
//! editor's server-side ("Remote") settings file, which every
//! conversation's editor shares and which open editors follow at once; the
//! Bakery rewrites only the keys it owns, so whatever the operator sets
//! there carries across threads. (The editor keeps its "User" settings and
//! keybindings in the browser; the built-in Bakery extension in the image
//! closes the side panes at each open and takes back the shortcuts.)

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where every editor's `Machine` folder (its "Remote" settings) sits
/// inside its container: the editor's own, made a link to this mount at its
/// start.
pub(crate) const MOUNTED_AT: &str = "/bakery-code";

/// The folder mounted at [`MOUNTED_AT`]: its `Machine` folder holds the
/// settings every editor shares.
pub(crate) fn shared_dir(state: &AppState) -> PathBuf {
    state.base.join("code")
}

/// The code font: JetBrains Mono when the Mac has it, as the app's is, else
/// SF Mono.
const CODE_FONT: &str = "'JetBrains Mono', 'SF Mono', Menlo, monospace";

/// The screen the operator chose: one menu button (the browser build's
/// compact menu, in the title bar once the activity bar is hidden) reaches
/// what is hidden, the file tree waits behind ⌘B (the built-in Bakery
/// extension closes it at each open), and nothing of its own offers chat,
/// tips or a welcome page. Workspace trust and telemetry are the server's
/// flags (`seat_container::start_editor`): this file can't set them.
fn chrome() -> Value {
    json!({
        "workbench.activityBar.location": "hidden",
        "window.commandCenter": false,
        "workbench.layoutControl.enabled": false,
        "workbench.navigationControl.enabled": false,
        "workbench.secondarySideBar.defaultVisibility": "hidden",
        "workbench.startupEditor": "none",
        "workbench.editor.empty.hint": "hidden",
        "workbench.tips.enabled": false,
        "workbench.welcomePage.walkthroughs.openOnInstall": false,
        "workbench.statusBar.visible": true,
        "breadcrumbs.enabled": false,
        "editor.minimap.enabled": false,
        "chat.disableAIFeatures": true,
        "extensions.ignoreRecommendations": true,
        "terminal.integrated.hideOnStartup": "always",
        "window.title": "${activeEditorShort}${separator}${rootName}${separator}Code",
        "editor.fontFamily": CODE_FONT,
    })
}

/// Puts the Bakery's keys in the shared settings before an editor starts,
/// and returns the folder to mount. The colors stay as last set.
pub(crate) fn prepare(state: &AppState) -> Result<PathBuf, String> {
    let dir = shared_dir(state);
    write_settings(&dir, None)?;
    Ok(dir)
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct AppearanceReq {
    /// Whether the scheme's surfaces are dark.
    pub(crate) dark: bool,
    /// The scheme's colors as the app resolves them, by token
    /// (`--bg-app`: `#0d0e11` or `rgb(…)`).
    pub(crate) colors: BTreeMap<String, String>,
    /// The scheme's code font, when it sets one (`--font-mono`).
    #[serde(default)]
    pub(crate) mono_font: Option<String>,
}

/// `POST /code/appearance`: the scheme the app shows now, for every editor
/// (open ones follow at once). The app sends it when its scheme or the Mac's
/// light or dark mode changes. Hidden from agents.
pub(crate) async fn appearance(
    State(st): State<AppState>,
    Json(req): Json<AppearanceReq>,
) -> Result<Json<Value>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || write_settings(&shared_dir(&st), Some(&req)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(|()| Json(json!({ "ok": true })))
        .map_err(|e| (StatusCode::CONFLICT, e))
}

fn write_settings(dir: &Path, scheme: Option<&AppearanceReq>) -> Result<(), String> {
    let path = dir.join("Machine").join("settings.json");
    let mut settings = read_jsonc(&path, json!({}))?;
    let Some(map) = settings.as_object_mut() else {
        return Err(format!("{}: not a settings object", path.display()));
    };
    for (key, value) in chrome().as_object().into_iter().flatten() {
        map.insert(key.clone(), value.clone());
    }
    if let Some(scheme) = scheme {
        for (key, value) in scheme_settings(scheme) {
            map.insert(key, value);
        }
    }
    write_json(&path, &settings)
}

/// The settings a scheme sets: a base theme of its lightness, its colors on
/// the editor's parts, its syntax colors, and its code font.
fn scheme_settings(scheme: &AppearanceReq) -> Vec<(String, Value)> {
    let token = |name: &str| scheme.colors.get(name).and_then(|v| hex(v));
    let mut colors = Map::new();
    for (part, name) in PARTS {
        if let Some(color) = token(name) {
            colors.insert(part.to_string(), json!(color));
        }
    }
    let rules: Vec<Value> = SYNTAX
        .iter()
        .filter_map(|(scopes, name)| {
            token(name).map(|color| json!({ "scope": scopes, "settings": { "foreground": color } }))
        })
        .collect();
    let font = scheme
        .mono_font
        .as_deref()
        .filter(|f| plain_font(f))
        .map(|f| format!("{f}, {CODE_FONT}"))
        .unwrap_or_else(|| CODE_FONT.to_string());
    let theme = if scheme.dark {
        "Default Dark Modern"
    } else {
        "Default Light Modern"
    };
    vec![
        ("workbench.colorTheme".into(), json!(theme)),
        (
            "workbench.colorCustomizations".into(),
            Value::Object(colors),
        ),
        (
            "editor.tokenColorCustomizations".into(),
            json!({ "textMateRules": rules }),
        ),
        ("editor.fontFamily".into(), json!(font)),
    ]
}

/// The editor's parts, by the app token that colors them.
const PARTS: &[(&str, &str)] = &[
    ("editor.background", "--bg-app"),
    ("editor.foreground", "--fg-body"),
    ("editorLineNumber.foreground", "--fg-linenum"),
    ("editorLineNumber.activeForeground", "--fg-dim"),
    ("editorCursor.foreground", "--accent"),
    ("editor.selectionBackground", "--accent-bg-2"),
    ("editor.lineHighlightBackground", "--accent-sel"),
    ("editor.findMatchBackground", "--accent-bg-2"),
    ("editorIndentGuide.background1", "--line-soft"),
    ("editorIndentGuide.activeBackground1", "--line-strong"),
    ("editorRuler.foreground", "--line-soft"),
    ("editorWidget.background", "--bg-raised"),
    ("editorWidget.border", "--line"),
    ("editorError.foreground", "--bad"),
    ("editorWarning.foreground", "--warn"),
    ("editorInfo.foreground", "--accent-fg"),
    ("diffEditor.insertedLineBackground", "--diff-add-bg"),
    ("diffEditor.removedLineBackground", "--diff-del-bg"),
    ("diffEditor.insertedTextBackground", "--diff-add-bg-editor"),
    ("diffEditor.removedTextBackground", "--diff-del-bg-editor"),
    ("titleBar.activeBackground", "--bg-chrome"),
    ("titleBar.inactiveBackground", "--bg-chrome"),
    ("titleBar.activeForeground", "--fg"),
    ("titleBar.inactiveForeground", "--fg-dim"),
    ("titleBar.border", "--line-chrome"),
    ("sideBar.background", "--bg-chrome"),
    ("sideBar.foreground", "--fg-2"),
    ("sideBar.border", "--line-chrome"),
    ("sideBarSectionHeader.background", "--bg-chrome"),
    ("sideBarSectionHeader.foreground", "--fg-fainter"),
    ("editorGroupHeader.tabsBackground", "--bg-chrome"),
    ("editorGroupHeader.tabsBorder", "--line-chrome"),
    ("tab.activeBackground", "--bg-app"),
    ("tab.inactiveBackground", "--bg-chrome"),
    ("tab.activeForeground", "--fg"),
    ("tab.inactiveForeground", "--fg-dim"),
    ("tab.border", "--line-chrome"),
    ("tab.activeBorderTop", "--accent"),
    ("statusBar.background", "--bg-rail"),
    ("statusBar.foreground", "--fg-dim"),
    ("statusBar.border", "--line-chrome"),
    ("statusBar.noFolderBackground", "--bg-rail"),
    ("panel.background", "--bg-panel"),
    ("panel.border", "--line-chrome"),
    ("input.background", "--bg-app"),
    ("input.foreground", "--fg"),
    ("input.border", "--line"),
    ("dropdown.background", "--bg-raised"),
    ("dropdown.border", "--line"),
    ("quickInput.background", "--bg-raised"),
    ("menu.background", "--bg-raised"),
    ("menu.foreground", "--fg"),
    ("menu.selectionBackground", "--accent-bg"),
    ("menu.border", "--line"),
    ("list.activeSelectionBackground", "--accent-bg"),
    ("list.activeSelectionForeground", "--fg"),
    ("list.inactiveSelectionBackground", "--accent-sel"),
    ("list.hoverBackground", "--bg-chip"),
    ("focusBorder", "--accent-border"),
    ("button.background", "--accent-solid"),
    ("button.hoverBackground", "--accent-solid-h"),
    ("button.foreground", "--fg-on-solid"),
    ("badge.background", "--accent-solid"),
    ("badge.foreground", "--fg-on-solid"),
    ("scrollbarSlider.hoverBackground", "--scrollbar-thumb-hover"),
    ("textLink.foreground", "--accent-fg"),
    ("gitDecoration.modifiedResourceForeground", "--warn"),
    ("gitDecoration.addedResourceForeground", "--ok"),
    ("gitDecoration.untrackedResourceForeground", "--ok"),
    ("gitDecoration.deletedResourceForeground", "--bad"),
];

/// Syntax scopes, by the app token that colors them.
const SYNTAX: &[(&str, &str)] = &[
    (
        "keyword, storage.type, storage.modifier",
        "--syntax-keyword",
    ),
    ("string, string.quoted", "--syntax-string"),
    (
        "comment, punctuation.definition.comment",
        "--syntax-comment",
    ),
    ("constant.numeric, constant.language", "--syntax-number"),
    (
        "entity.name.type, support.type, entity.name.class",
        "--syntax-type",
    ),
];

/// A CSS color as the editor takes it ("#rrggbbaa"): hex, `rgb()`/`rgba()`
/// or `color(srgb …)` (what a browser resolves color-mix to). Anything else
/// leaves that part to the base theme.
fn hex(css: &str) -> Option<String> {
    let css = css.trim().to_ascii_lowercase();
    let byte = |v: f64| (v.clamp(0.0, 255.0)).round() as u8;
    let (rgb, alpha): ([u8; 3], f64) = if let Some(h) = css.strip_prefix('#') {
        let digits: Vec<u8> = h
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let full: Vec<u8> = match digits.len() {
            3 | 4 => digits.iter().map(|d| d * 17).collect(),
            6 | 8 => digits.chunks(2).map(|p| p[0] * 16 + p[1]).collect(),
            _ => return None,
        };
        let alpha = full.get(3).map_or(1.0, |a| *a as f64 / 255.0);
        ([full[0], full[1], full[2]], alpha)
    } else {
        let (srgb, inner) = if let Some(rest) = css.strip_prefix("color(srgb") {
            (true, rest.strip_suffix(')')?)
        } else {
            let open = css.find('(')?;
            if !matches!(&css[..open], "rgb" | "rgba") {
                return None;
            }
            (false, css[open + 1..].strip_suffix(')')?)
        };
        let nums: Vec<f64> = inner
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .map(|s| match s.strip_suffix('%') {
                Some(p) => p.parse::<f64>().map(|p| p / 100.0),
                None => s.parse::<f64>(),
            })
            .collect::<Result<_, _>>()
            .ok()?;
        if nums.len() < 3 {
            return None;
        }
        let scale = if srgb { 255.0 } else { 1.0 };
        (
            [
                byte(nums[0] * scale),
                byte(nums[1] * scale),
                byte(nums[2] * scale),
            ],
            nums.get(3).copied().unwrap_or(1.0),
        )
    };
    Some(format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        rgb[0],
        rgb[1],
        rgb[2],
        byte(alpha * 255.0)
    ))
}

/// A font list of names and quotes only, never anything else CSS takes.
fn plain_font(font: &str) -> bool {
    !font.trim().is_empty()
        && font.len() <= 200
        && font
            .chars()
            .all(|c| c.is_alphanumeric() || " ,'\"-_.".contains(c))
}

/// A settings file as the editor reads it: JSON with comments and trailing
/// commas. A missing file is `empty`; one that still won't read is left as
/// it is and the call fails, so the operator's file is never replaced.
fn read_jsonc(path: &Path, empty: Value) -> Result<Value, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(empty),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(empty);
    }
    serde_json::from_str(&plain_json(&text))
        .map_err(|e| format!("{} doesn't read as settings: {e}", path.display()))
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Steps past a comment that `chars` starts inside (after its `/`), if it
/// is one: `true` when it was.
fn skip_comment(chars: &mut Chars) -> bool {
    match chars.peek() {
        Some('/') => {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            true
        }
        Some('*') => {
            chars.next();
            let mut last = ' ';
            for c in chars.by_ref() {
                if last == '*' && c == '/' {
                    break;
                }
                last = c;
            }
            true
        }
        _ => false,
    }
}

/// JSON with comments and trailing commas made plain JSON.
fn plain_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars: Chars = text.chars().peekable();
    let (mut in_string, mut escaped) = (false, false);
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if skip_comment(&mut chars) => out.push(' '),
            ',' => {
                // A comma before a closing bracket (only whitespace and
                // comments between) is dropped.
                let mut ahead = chars.clone();
                let next = loop {
                    match ahead.next() {
                        Some(c) if c.is_whitespace() => {}
                        Some('/') if skip_comment(&mut ahead) => {}
                        other => break other,
                    }
                };
                if !matches!(next, Some('}' | ']')) {
                    out.push(',');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    let temp = path.with_extension("json.bakery-tmp");
    std::fs::write(&temp, text)
        .and_then(|_| std::fs::rename(&temp, path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_operators_own_settings_stay_and_the_bakerys_keys_win() {
        let dir = tempfile::tempdir().unwrap();
        let machine = dir.path().join("Machine");
        std::fs::create_dir_all(&machine).unwrap();
        std::fs::write(
            machine.join("settings.json"),
            "{\n  // mine\n  \"editor.fontSize\": 15,\n  \"workbench.activityBar.location\": \"default\", /* theirs */\n  \"url\": \"http://x//y\",\n}\n",
        )
        .unwrap();
        write_settings(dir.path(), None).unwrap();
        write_settings(dir.path(), None).unwrap();

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(machine.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["editor.fontSize"], 15, "the operator's own stays");
        assert_eq!(
            settings["url"], "http://x//y",
            "a // inside a string isn't a comment"
        );
        assert_eq!(settings["workbench.activityBar.location"], "hidden");
        assert_eq!(settings["chat.disableAIFeatures"], true);

        // A file that still won't read is left alone, never replaced.
        std::fs::write(machine.join("settings.json"), "{ not json").unwrap();
        assert!(write_settings(dir.path(), None).is_err());
        assert_eq!(
            std::fs::read_to_string(machine.join("settings.json")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn a_scheme_colors_the_editor_and_its_syntax_live() {
        let dir = tempfile::tempdir().unwrap();
        let scheme = AppearanceReq {
            dark: false,
            colors: BTreeMap::from([
                ("--bg-app".to_string(), "#fbf7ef".to_string()),
                (
                    "--accent-bg-2".to_string(),
                    "rgba(91, 140, 255, 0.24)".to_string(),
                ),
                (
                    "--syntax-keyword".to_string(),
                    "color(srgb 0.5 0 1 / 0.5)".to_string(),
                ),
                ("--fg".to_string(), "var(--elsewhere)".to_string()),
            ]),
            mono_font: Some("'Berkeley Mono'".to_string()),
        };
        write_settings(dir.path(), Some(&scheme)).unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("Machine/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["workbench.colorTheme"], "Default Light Modern");
        let colors = &settings["workbench.colorCustomizations"];
        assert_eq!(colors["editor.background"], "#fbf7efff");
        assert_eq!(colors["tab.activeBackground"], "#fbf7efff");
        assert_eq!(colors["editor.selectionBackground"], "#5b8cff3d");
        assert!(
            colors.get("titleBar.activeForeground").is_none(),
            "an unread color stays the theme's"
        );
        let rules = &settings["editor.tokenColorCustomizations"]["textMateRules"];
        assert_eq!(rules[0]["settings"]["foreground"], "#8000ff80");
        assert_eq!(
            settings["editor.fontFamily"],
            format!("'Berkeley Mono', {CODE_FONT}")
        );
        assert_eq!(settings["workbench.activityBar.location"], "hidden");
    }

    #[test]
    fn colors_read_as_the_editor_takes_them_and_settings_read_as_it_does() {
        assert_eq!(hex("#abc").as_deref(), Some("#aabbccff"));
        assert_eq!(hex("#0D0E11").as_deref(), Some("#0d0e11ff"));
        assert_eq!(hex("#5b8cff24").as_deref(), Some("#5b8cff24"));
        assert_eq!(hex("rgb(255, 0, 10)").as_deref(), Some("#ff000aff"));
        assert_eq!(hex("rgba(0,0,0,.65)").as_deref(), Some("#000000a6"));
        assert_eq!(hex("rgb(0 0 0 / 50%)").as_deref(), Some("#00000080"));
        for bad in [
            "",
            "red",
            "#12",
            "url(x)",
            "rgb(1,2)",
            "expression(alert(1))",
        ] {
            assert_eq!(hex(bad), None, "{bad}");
        }
        assert!(plain_font("'JetBrains Mono', monospace"));
        assert!(!plain_font("x; } body { background: url(evil)"));
        // Many commas read in one pass, trailing ones dropped.
        let many = format!("[{}]", "1, ".repeat(5000));
        let read: Value = serde_json::from_str(&plain_json(&many)).unwrap();
        assert_eq!(read.as_array().unwrap().len(), 5000);
        assert_eq!(
            plain_json("{\"a\": \"/* not */\", /* yes */ }"),
            "{\"a\": \"/* not */\"   }"
        );
    }
}
