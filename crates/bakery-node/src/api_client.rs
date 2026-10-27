//! The node's typed client for the UI, generated from the route catalog
//! (`api_catalog`) into bakery-client/src/lib/api.generated.ts: one function
//! per route, named as the Baker's tool is, taking one request object, with
//! the request and response types the routes' own schemas describe. Calls
//! go through the hand-written `apiTransport.ts`.
//!
//! `BAKERY_WRITE_API_CLIENT=1 cargo test -p bakery-node api_client` writes
//! it; an ordinary test run fails while it is out of date.

use crate::api_catalog::{listing, Risk, UNTYPED};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt::Write as _;

const HEADER: &str = "\
// GENERATED from crates/bakery-node/src/api_catalog.rs — do not edit.
// Regenerate: BAKERY_WRITE_API_CLIENT=1 cargo test -p bakery-node api_client
// (cargo test fails while this file is out of date).
";

const UNTYPED_NOTE: &str = "Untyped JSON: no schema on the node describes it yet.";

fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

fn camel(snake: &str) -> String {
    let pascal = pascal(snake);
    let mut chars = pascal.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_lowercase().to_string() + chars.as_str()
    })
}

/// A shared type's name as a TypeScript identifier.
fn ident(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

fn doc(text: &str, pad: &str) -> String {
    let text = text.trim().replace("*/", "* /");
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    match lines.as_slice() {
        [] => String::new(),
        [one] => format!("{pad}/** {one} */\n"),
        many => {
            let mut out = format!("{pad}/**\n");
            for line in many {
                let gap = if line.is_empty() { "" } else { " " };
                let _ = writeln!(out, "{pad} *{gap}{line}");
            }
            out + &format!("{pad} */\n")
        }
    }
}

fn description(schema: &Value) -> &str {
    schema.get("description").and_then(Value::as_str).unwrap_or_default()
}

fn prop_key(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if plain {
        name.to_string()
    } else {
        literal(&Value::String(name.to_string()))
    }
}

fn literal(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "unknown".into())
}

/// A TypeScript type, and whether it is a union at its top level, which
/// needs parentheses inside an intersection.
struct Ts {
    text: String,
    union: bool,
}

impl Ts {
    fn atom(text: impl Into<String>) -> Self {
        Ts { text: text.into(), union: false }
    }

    fn grouped(self) -> String {
        if self.union {
            format!("({})", self.text)
        } else {
            self.text
        }
    }
}

fn union(types: Vec<Ts>) -> Ts {
    let mut seen = BTreeSet::new();
    let mut unique: Vec<Ts> = types.into_iter().filter(|t| seen.insert(t.text.clone())).collect();
    if unique.len() < 2 {
        return unique.pop().unwrap_or_else(|| Ts::atom("never"));
    }
    let text = unique.into_iter().map(|t| t.text).collect::<Vec<_>>().join(" | ");
    Ts { text, union: true }
}

fn intersection(mut parts: Vec<Ts>) -> Ts {
    if parts.len() < 2 {
        return parts.pop().unwrap_or_else(|| Ts::atom("unknown"));
    }
    Ts::atom(parts.into_iter().map(Ts::grouped).collect::<Vec<_>>().join(" & "))
}

fn object(obj: &Map<String, Value>, indent: usize) -> String {
    let props = obj.get("properties").and_then(Value::as_object);
    let required: BTreeSet<&str> = obj
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let extra = match obj.get("additionalProperties") {
        None | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some("unknown".to_string()),
        Some(schema) => Some(ts(schema, indent + 1).text),
    };
    match (props, extra) {
        (None, None) => "Record<string, unknown>".into(),
        (Some(props), None) if props.is_empty() => "Record<string, never>".into(),
        (None, Some(value)) => format!("Record<string, {value}>"),
        (Some(props), extra) => {
            let pad = "  ".repeat(indent + 1);
            let mut out = String::from("{\n");
            for (name, schema) in props {
                let ty = ts(schema, indent + 1).text;
                let note = if ty == "unknown" { UNTYPED_NOTE } else { "" };
                out += &doc(&format!("{}\n{note}", description(schema)), &pad);
                let optional = if required.contains(name.as_str()) { "" } else { "?" };
                let _ = writeln!(out, "{pad}{}{optional}: {ty};", prop_key(name));
            }
            if let Some(value) = extra {
                let _ = writeln!(out, "{pad}[key: string]: {value};");
            }
            out + &"  ".repeat(indent) + "}"
        }
    }
}

fn array(obj: &Map<String, Value>, indent: usize) -> String {
    match (obj.get("prefixItems"), obj.get("items")) {
        (Some(Value::Array(items)), _) => {
            let items: Vec<String> = items.iter().map(|i| ts(i, indent).text).collect();
            format!("[{}]", items.join(", "))
        }
        (_, Some(items)) => format!("Array<{}>", ts(items, indent).text),
        _ => "Array<unknown>".into(),
    }
}

/// TypeScript for one JSON schema (draft 2020-12, as schemars writes it).
fn ts(schema: &Value, indent: usize) -> Ts {
    let Value::Object(obj) = schema else {
        return Ts::atom(if schema == &Value::Bool(false) { "never" } else { "unknown" });
    };
    if obj.contains_key(UNTYPED) {
        return Ts::atom("unknown");
    }
    if let Some(Value::String(target)) = obj.get("$ref") {
        return Ts::atom(ident(target.rsplit('/').next().unwrap_or(target)));
    }
    if let Some(value) = obj.get("const") {
        return Ts::atom(literal(value));
    }
    if let Some(Value::Array(values)) = obj.get("enum") {
        return union(values.iter().map(|v| Ts::atom(literal(v))).collect());
    }
    let mut parts: Vec<Ts> = Vec::new();
    for keyword in ["oneOf", "anyOf"] {
        if let Some(Value::Array(variants)) = obj.get(keyword) {
            parts.push(union(variants.iter().map(|v| ts(v, indent)).collect()));
        }
    }
    if let Some(Value::Array(all)) = obj.get("allOf") {
        parts.extend(all.iter().map(|v| ts(v, indent)));
    }
    let types: Vec<&str> = match obj.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !types.is_empty() {
        parts.push(union(
            types
                .iter()
                .map(|t| {
                    Ts::atom(match *t {
                        "string" => "string".to_string(),
                        "integer" | "number" => "number".to_string(),
                        "boolean" => "boolean".to_string(),
                        "null" => "null".to_string(),
                        "array" => array(obj, indent),
                        "object" => object(obj, indent),
                        _ => "unknown".to_string(),
                    })
                })
                .collect(),
        ));
    } else if obj.contains_key("properties") {
        parts.push(Ts::atom(object(obj, indent)));
    }
    intersection(parts)
}

fn grade(risk: Option<Risk>) -> String {
    risk.map_or("null".into(), |r| format!("\"{}\"", r.as_str()))
}

/// The whole generated file.
pub(crate) fn generate() -> String {
    // One generator for every route, so a type two routes share is one named type.
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let rows: Vec<_> = listing()
        .into_iter()
        .map(|(route, name, risk)| {
            let input = (route.input)(&mut generator);
            let output = (route.output)(&mut generator);
            (route, name, risk, input, output)
        })
        .collect();
    let methods: BTreeSet<&str> = rows.iter().map(|row| row.0.method.as_str()).collect();

    let mut out = String::from(HEADER);
    out += "\nimport { request } from \"./apiTransport.ts\";\n\n";
    let methods = union(methods.iter().map(|m| Ts::atom(format!("\"{m}\""))).collect());
    let _ = writeln!(out, "export type Method = {};", methods.text);
    out += "export type Risk = \"read\" | \"write\" | \"run\" | \"destructive\";\n";

    out += "\n// ── Shared types ──\n\n";
    for (name, schema) in generator.definitions() {
        out += &doc(description(schema), "");
        let _ = writeln!(out, "export type {} = {};\n", ident(name), ts(schema, 0).text);
    }

    out += "// ── Routes ──\n";
    let mut listed = String::new();
    for (route, name, risk, input, output) in &rows {
        let _ = writeln!(
            listed,
            "  {{ name: \"{}\", method: \"{}\", path: \"{}\", risk: {}, hidden: {}, summary: {} }},",
            camel(name),
            route.method.as_str(),
            route.path,
            grade(*risk),
            risk.is_none(),
            literal(&Value::String(route.summary.to_string())),
        );
        // A WebSocket upgrade has no request/response call to generate.
        let Some(output) = output else { continue };
        let ty = pascal(name);
        let params: Vec<&str> = route
            .path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .collect();
        let mut request_ty: Vec<Ts> = input.iter().map(|input| ts(input, 0)).collect();
        if !params.is_empty() {
            let fields: Vec<String> = params.iter().map(|p| format!("{}: string", prop_key(p))).collect();
            request_ty.push(Ts::atom(format!("{{ {} }}", fields.join("; "))));
        }
        let request_ty = if request_ty.is_empty() {
            "Record<string, never>".to_string()
        } else {
            intersection(request_ty).text
        };
        let who = risk.map_or("operator only".to_string(), |r| format!("risk: {}", r.as_str()));
        out += "\n";
        out += &doc(&format!("{}\n{} {} · {who}", route.summary, route.method.as_str(), route.path), "");
        let _ = writeln!(out, "export type {ty}Request = {request_ty};");
        if output.get(UNTYPED).is_some() {
            let _ = writeln!(out, "/** {UNTYPED_NOTE} */");
        }
        let _ = writeln!(out, "export type {ty}Response = {};", ts(output, 0).text);
        let default = if input.is_none() && params.is_empty() { " = {}" } else { "" };
        let _ = writeln!(
            out,
            "export function {}(req: {ty}Request{default}, init?: RequestInit): Promise<{ty}Response> {{",
            camel(name)
        );
        let method = route.method.as_str();
        if params.is_empty() {
            let _ = writeln!(out, "  return request<{ty}Response>(\"{method}\", \"{}\", req, init);", route.path);
        } else {
            let mut path = route.path.to_string();
            for p in &params {
                path = path.replace(&format!("{{{p}}}"), &format!("${{encodeURIComponent({p})}}"));
            }
            let _ = writeln!(out, "  const {{ {}, ...rest }} = req;", params.join(", "));
            let _ = writeln!(out, "  return request<{ty}Response>(\"{method}\", `{path}`, rest, init);");
        }
        out += "}\n";
    }

    out += "\n/** Every node route: its client function, its risk grade for the Baker\n";
    out += " * (`null`: operator only, never offered to an agent) and what it does. */\n";
    out += "export const ROUTES: ReadonlyArray<{\n  name: string;\n  method: Method;\n  path: string;\n";
    out += "  risk: Risk | null;\n  hidden: boolean;\n  summary: string;\n}> = [\n";
    out += &listed;
    out += "];\n";
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn target() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bakery-client/src/lib/api.generated.ts")
    }

    #[test]
    fn the_generated_ui_client_is_current() {
        let generated = generate();
        if std::env::var_os("BAKERY_WRITE_API_CLIENT").is_some() {
            std::fs::write(target(), &generated).unwrap();
            return;
        }
        let current = std::fs::read_to_string(target()).unwrap_or_default();
        assert!(
            current == generated,
            "bakery-client/src/lib/api.generated.ts is out of date: run \
             BAKERY_WRITE_API_CLIENT=1 cargo test -p bakery-node api_client"
        );
    }

    #[test]
    fn every_route_but_the_websockets_gets_one_client_function() {
        let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
        let mut names = BTreeSet::new();
        for (route, name, _) in listing() {
            let call = format!("{} {}", route.method.as_str(), route.path);
            assert!(names.insert(camel(&name)), "{call} reuses the client name of {name}");
            let websocket = matches!(route.path, "/events" | "/terminal/ws" | "/desktop/ws");
            assert_eq!(
                (route.output)(&mut generator).is_some(),
                !websocket,
                "{call} needs .returns(schema::<T>) or .returns(untyped)"
            );
        }
    }

    #[test]
    fn schemas_become_typescript() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "who"},
                "count": {"type": ["integer", "null"]},
                "mode": {"enum": ["manual", "auto"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "kind": {"oneOf": [{"const": "a"}, {"$ref": "#/$defs/Other"}]},
                "data": true,
            },
            "required": ["id"],
        });
        let out = ts(&schema, 0).text;
        for want in [
            "  /** who */\n  id: string;",
            "  count?: number | null;",
            "  mode?: \"manual\" | \"auto\";",
            "  tags?: Array<string>;",
            "  env?: Record<string, string>;",
            "  kind?: \"a\" | Other;",
            "  /** Untyped JSON: no schema on the node describes it yet. */\n  data?: unknown;",
        ] {
            assert!(out.contains(want), "{want:?} missing from:\n{out}");
        }
        let both = serde_json::json!({"allOf": [
            {"anyOf": [{"$ref": "#/$defs/A"}, {"type": "null"}]},
            {"$ref": "#/$defs/B"},
        ]});
        assert_eq!(ts(&both, 0).text, "(A | null) & B");
        assert_eq!(ts(&serde_json::json!({UNTYPED: true}), 0).text, "unknown");
        assert_eq!(camel("git_status"), "gitStatus");
    }
}
