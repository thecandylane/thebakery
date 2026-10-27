//! Questions an agent asks the human — one shape for every runtime.
//!
//! An HTTP seat asks through its `ask_user` tool; a native runtime asks
//! through an ACP form elicitation (Claude's AskUserQuestion arrives that
//! way). Both become the same `agent.ask.pending` questions and are answered
//! through the same route, so the panel renders one card for all of them.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Bounds on what one ask may carry; the payload lands in Heart.
const MAX_QUESTIONS: usize = 10;
const MAX_OPTIONS: usize = 20;
const MAX_TEXT: usize = 4000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "OptionWire")]
pub(crate) struct AskOption {
    pub(crate) label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// What a form expects back when it differs from the label shown — e.g.
    /// `retry_fallback` behind "Retry with Opus".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<String>,
}

/// Options were plain strings before they carried descriptions; both read.
#[derive(Deserialize)]
#[serde(untagged)]
enum OptionWire {
    Label(String),
    Full {
        label: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        value: Option<String>,
    },
}

impl From<OptionWire> for AskOption {
    fn from(wire: OptionWire) -> Self {
        match wire {
            OptionWire::Label(label) => AskOption { label, description: None, value: None },
            OptionWire::Full { label, description, value } => AskOption { label, description, value },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AskQuestion {
    pub(crate) question: String,
    /// A short label for the question, shown as its tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) header: Option<String>,
    #[serde(default)]
    pub(crate) options: Vec<AskOption>,
    #[serde(default)]
    pub(crate) multi_select: bool,
    /// The elicitation form field this question fills, for a native runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) field: Option<String>,
    /// Its free-text companion field, when the form has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) other_field: Option<String>,
}

/// One question's answer: the options picked, and/or the human's own words.
/// Neither means that question was skipped.
#[derive(schemars::JsonSchema, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AskAnswer {
    #[serde(default)]
    pub(crate) picks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) other: Option<String>,
}

impl AskAnswer {
    fn other_text(&self) -> Option<&str> {
        self.other.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
}

fn clean(text: &str) -> String {
    text.trim().chars().take(MAX_TEXT).collect()
}

/// An HTTP seat's `ask_user` arguments, checked and normalized. The error is
/// for the model, so it can fix its call.
pub(crate) fn questions_from_tool_args(args: &Value) -> Result<Vec<AskQuestion>, String> {
    let raw = args.get("questions").ok_or("ask_user needs \"questions\"")?;
    let mut questions: Vec<AskQuestion> = serde_json::from_value(raw.clone()).map_err(|e| {
        format!("ask_user \"questions\" must be an array of {{question, header?, options?, multiSelect?}}: {e}")
    })?;
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(format!("ask_user takes 1 to {MAX_QUESTIONS} questions"));
    }
    for q in &mut questions {
        q.question = clean(&q.question);
        if q.question.is_empty() {
            return Err("every ask_user question needs its text".to_string());
        }
        q.header = q.header.as_deref().map(clean).filter(|h| !h.is_empty());
        if q.options.len() > MAX_OPTIONS {
            return Err(format!("an ask_user question takes at most {MAX_OPTIONS} options"));
        }
        for option in &mut q.options {
            option.label = clean(&option.label);
            option.description = option.description.as_deref().map(clean).filter(|d| !d.is_empty());
            option.value = None;
        }
        let mut labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
        labels.sort_unstable();
        if labels.iter().any(|l| l.is_empty()) || labels.windows(2).any(|w| w[0] == w[1]) {
            return Err("ask_user options need distinct, non-empty labels".to_string());
        }
        // Only an elicitation names form fields; a model never does.
        q.field = None;
        q.other_field = None;
    }
    Ok(questions)
}

/// The questions in an ACP form elicitation's `requestedSchema`. Claude's
/// AskUserQuestion sends `question_<n>` select fields (a `oneOf` enum, or an
/// array of `anyOf` for multi-select) each with a free-text
/// `question_<n>_custom` companion; any other string or enum field becomes a
/// question too. `None` when the form holds something this card cannot ask.
// ponytail: number/boolean form fields decline the whole form; map them when an MCP server needs one.
pub(crate) fn questions_from_elicitation(message: &str, schema: &Value) -> Option<Vec<AskQuestion>> {
    let props = schema.get("properties")?.as_object()?;
    let is_companion = |key: &str| {
        key.strip_suffix("_custom")
            .is_some_and(|base| props.contains_key(base))
    };
    let index = |key: &str| key.strip_prefix("question_").and_then(|n| n.parse::<usize>().ok());
    let mut fields: Vec<(&String, &Value)> = props.iter().filter(|(k, _)| !is_companion(k)).collect();
    fields.sort_by_key(|(k, _)| index(k).unwrap_or(usize::MAX));
    if fields.is_empty() || fields.len() > MAX_QUESTIONS {
        return None;
    }
    let single = fields.len() == 1;
    fields
        .into_iter()
        .map(|(key, prop)| {
            let text = |name: &str| prop.get(name).and_then(Value::as_str).map(clean).filter(|s| !s.is_empty());
            let enum_options = |values: &Value| -> Option<Vec<AskOption>> {
                let list = values.as_array()?;
                list.iter()
                    .map(|v| match v {
                        Value::String(s) => Some(AskOption { label: s.clone(), description: None, value: None }),
                        Value::Object(o) => {
                            let value = o.get("const")?.as_str()?.to_string();
                            let title = o.get("title").and_then(Value::as_str).map(clean).filter(|t| !t.is_empty());
                            Some(AskOption {
                                label: title.clone().unwrap_or_else(|| value.clone()),
                                description: o.get("description").and_then(Value::as_str).map(clean),
                                value: title.is_some_and(|t| t != value).then_some(value),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            };
            let (options, multi_select) = match prop.get("type").and_then(Value::as_str)? {
                "string" => (
                    prop.get("oneOf")
                        .or_else(|| prop.get("enum"))
                        .map(enum_options)
                        .unwrap_or(Some(Vec::new()))?,
                    false,
                ),
                "array" => {
                    let items = prop.get("items")?;
                    (enum_options(items.get("anyOf").or_else(|| items.get("enum"))?)?, true)
                }
                _ => return None,
            };
            if options.len() > MAX_OPTIONS {
                return None;
            }
            let question = text("description")
                .or_else(|| single.then(|| clean(message)).filter(|m| !m.is_empty()))
                .or_else(|| text("title"))
                .unwrap_or_else(|| key.clone());
            let other_field = format!("{key}_custom");
            Some(AskQuestion {
                question,
                header: text("title"),
                options,
                multi_select,
                other_field: props.contains_key(&other_field).then_some(other_field),
                field: Some(key.clone()),
            })
        })
        .collect()
}

/// Checks a structured answer against the questions it answers.
pub(crate) fn validate_answers(questions: &[AskQuestion], answers: &[AskAnswer]) -> Result<(), String> {
    if answers.len() != questions.len() {
        return Err(format!("expected {} answers, got {}", questions.len(), answers.len()));
    }
    for (q, a) in questions.iter().zip(answers) {
        if !q.multi_select && a.picks.len() > 1 {
            return Err(format!("\"{}\" takes one option", q.question));
        }
        if let Some(pick) = a.picks.iter().find(|p| !q.options.iter().any(|o| &o.label == *p)) {
            return Err(format!("\"{pick}\" is not an option of \"{}\"", q.question));
        }
        if a.other.as_deref().is_some_and(|o| o.chars().count() > MAX_TEXT) {
            return Err(format!("an answer is limited to {MAX_TEXT} characters"));
        }
    }
    Ok(())
}

fn answer_text(q: &AskQuestion, a: &AskAnswer) -> Option<String> {
    let other = a.other_text();
    let picks = a.picks.join(", ");
    match (picks.is_empty(), other) {
        (true, None) => None,
        (true, Some(o)) => Some(o.to_string()),
        (false, None) => Some(picks),
        (false, Some(o)) if q.multi_select => Some(format!("{picks}, {o}")),
        (false, Some(o)) => Some(format!("{picks} (note: {o})")),
    }
}

/// The answers as one line of text — what an HTTP seat's model reads, and
/// what an older client shows.
pub(crate) fn answer_summary(questions: &[AskQuestion], answers: &[AskAnswer]) -> String {
    if questions.len() == 1 {
        return answer_text(&questions[0], &answers[0]).unwrap_or_else(|| "(skipped)".to_string());
    }
    questions
        .iter()
        .zip(answers)
        .map(|(q, a)| {
            let answer = answer_text(q, a).unwrap_or_else(|| "(skipped)".to_string());
            format!("\"{}\" = \"{answer}\"", q.question)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The elicitation reply for a native runtime: `accept` with each answered
/// field, or `decline` when the human skipped everything — the runtime then
/// tells its model the questions went unanswered.
pub(crate) fn elicitation_reply(questions: &[AskQuestion], answers: &[AskAnswer]) -> Value {
    let mut content = serde_json::Map::new();
    for (q, a) in questions.iter().zip(answers) {
        let Some(field) = &q.field else { continue };
        let other = a.other_text();
        if q.options.is_empty() {
            if let Some(o) = other {
                content.insert(field.clone(), json!(o));
            }
            continue;
        }
        let values: Vec<&str> = a
            .picks
            .iter()
            .filter_map(|pick| q.options.iter().find(|o| &o.label == pick))
            .map(|o| o.value.as_deref().unwrap_or(&o.label))
            .collect();
        if q.multi_select && !values.is_empty() {
            content.insert(field.clone(), json!(values));
        } else if let Some(value) = values.first() {
            content.insert(field.clone(), json!(value));
        }
        if let (Some(other_field), Some(o)) = (&q.other_field, other) {
            content.insert(other_field.clone(), json!(o));
        }
    }
    if content.is_empty() {
        json!({ "action": "decline" })
    } else {
        json!({ "action": "accept", "content": content })
    }
}

/// An answer an older client sent as one string: an exact option label of a
/// lone question is that pick; anything else is the human's own words.
pub(crate) fn answers_from_text(questions: &[AskQuestion], text: &str) -> Vec<AskAnswer> {
    let mut answers = vec![AskAnswer::default(); questions.len()];
    if let (Some(q), Some(a)) = (questions.first(), answers.first_mut()) {
        if questions.len() == 1 && q.options.iter().any(|o| o.label == text) {
            a.picks = vec![text.to_string()];
        } else {
            a.other = Some(text.to_string());
        }
    }
    answers
}

/// The questions recorded on an `agent.ask.pending` payload.
pub(crate) fn pending_questions(payload: &Value) -> Vec<AskQuestion> {
    payload
        .get("questions")
        .cloned()
        .and_then(|q| serde_json::from_value(q).ok())
        .unwrap_or_default()
}

pub(crate) enum AskOutcome {
    Answered(Vec<AskAnswer>),
    Cancelled,
}

/// How the question at `event_hash` was settled, read from Heart.
pub(crate) fn outcome(
    state: &AppState,
    stream_id: &str,
    event_hash: &str,
    questions: &[AskQuestion],
) -> Option<AskOutcome> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    events.iter().rev().find_map(|e| {
        if !e.causality.iter().any(|h| h == event_hash) {
            return None;
        }
        match e.family.as_str() {
            "agent.ask.answered" => {
                let p: Value = serde_json::from_str(&e.payload).unwrap_or(Value::Null);
                let answers = p
                    .get("answers")
                    .cloned()
                    .and_then(|a| serde_json::from_value::<Vec<AskAnswer>>(a).ok())
                    .filter(|a| a.len() == questions.len())
                    .unwrap_or_else(|| {
                        answers_from_text(questions, p.get("answer").and_then(Value::as_str).unwrap_or(""))
                    });
                Some(AskOutcome::Answered(answers))
            }
            "agent.approval.cancelled" => Some(AskOutcome::Cancelled),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask_user_question_form() -> Value {
        json!({
            "type": "object",
            "properties": {
                "question_0": {
                    "type": "string", "title": "Signup", "description": "How do friends get in?",
                    "oneOf": [
                        {"const": "Invite codes", "title": "Invite codes", "description": "One per friend"},
                        {"const": "Shared password", "title": "Shared password"}
                    ]
                },
                "question_0_custom": {"type": "string", "title": "Other"},
                "question_1": {
                    "type": "array", "title": "Roadmap", "description": "What next?",
                    "items": {"anyOf": [
                        {"const": "Backups", "title": "Backups"},
                        {"const": "Who's online", "title": "Who's online"}
                    ]}
                },
                "question_1_custom": {"type": "string", "title": "Other"}
            }
        })
    }

    #[test]
    fn claude_questions_survive_the_round_trip_through_a_form() {
        let questions = questions_from_elicitation("Please answer.", &ask_user_question_form()).unwrap();
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].question, "How do friends get in?");
        assert_eq!(questions[0].header.as_deref(), Some("Signup"));
        assert_eq!(questions[0].options[0].description.as_deref(), Some("One per friend"));
        assert!(!questions[0].multi_select && questions[1].multi_select);
        assert_eq!(questions[1].other_field.as_deref(), Some("question_1_custom"));

        let answers = vec![
            AskAnswer { picks: vec!["Invite codes".into()], other: Some("Steam too".into()) },
            AskAnswer { picks: vec!["Backups".into(), "Who's online".into()], other: None },
        ];
        validate_answers(&questions, &answers).unwrap();
        assert_eq!(
            elicitation_reply(&questions, &answers),
            json!({"action": "accept", "content": {
                "question_0": "Invite codes",
                "question_0_custom": "Steam too",
                "question_1": ["Backups", "Who's online"]
            }})
        );
        assert_eq!(
            answer_summary(&questions, &answers),
            "\"How do friends get in?\" = \"Invite codes (note: Steam too)\"\n\"What next?\" = \"Backups, Who's online\""
        );
        let skipped = vec![AskAnswer::default(), AskAnswer::default()];
        assert_eq!(elicitation_reply(&questions, &skipped), json!({"action": "decline"}));
    }

    /// The adapter's "retry on the fallback model?" form: readable titles in
    /// the card, the form's own values in the reply.
    #[test]
    fn a_form_shows_option_titles_but_answers_with_their_values() {
        let form = json!({"type": "object", "properties": {"choice": {"type": "string", "oneOf": [
            {"const": "retry_fallback", "title": "Retry with Opus", "description": "The session continues on Opus."},
            {"const": "cancelled", "title": "Keep the refusal"}
        ]}}});
        let questions = questions_from_elicitation("Fable declined this request. Retry with Opus?", &form).unwrap();
        assert_eq!(questions[0].question, "Fable declined this request. Retry with Opus?");
        assert_eq!(questions[0].options[0].label, "Retry with Opus");
        let answers = vec![AskAnswer { picks: vec!["Retry with Opus".into()], other: None }];
        assert_eq!(
            elicitation_reply(&questions, &answers),
            json!({"action": "accept", "content": {"choice": "retry_fallback"}})
        );
    }

    #[test]
    fn answers_must_be_options_of_their_question() {
        let questions = questions_from_elicitation("", &ask_user_question_form()).unwrap();
        let two_picks = vec![
            AskAnswer { picks: vec!["Invite codes".into(), "Shared password".into()], other: None },
            AskAnswer::default(),
        ];
        assert!(validate_answers(&questions, &two_picks).is_err());
        let invented = vec![AskAnswer { picks: vec!["Discord".into()], other: None }, AskAnswer::default()];
        assert!(validate_answers(&questions, &invented).is_err());
        assert!(validate_answers(&questions, &[AskAnswer::default()]).is_err());
    }

    #[test]
    fn tool_questions_accept_plain_and_described_options() {
        let questions = questions_from_tool_args(&json!({"questions": [
            {"question": " Which branch? ", "options": ["main", "dev"]},
            {"question": "What else?", "header": "Extras", "multiSelect": true,
             "options": [{"label": "Docs", "description": "write them"}]}
        ]}))
        .unwrap();
        assert_eq!(questions[0].question, "Which branch?");
        assert_eq!(questions[0].options[1].label, "dev");
        assert_eq!(questions[1].options[0].description.as_deref(), Some("write them"));
        assert!(questions_from_tool_args(&json!({"questions": []})).is_err());
        assert!(questions_from_tool_args(&json!({"questions": [{"question": "x", "options": ["a", "a"]}]})).is_err());
        assert_eq!(
            answers_from_text(&questions[..1], "main"),
            vec![AskAnswer { picks: vec!["main".into()], other: None }]
        );
    }
}
