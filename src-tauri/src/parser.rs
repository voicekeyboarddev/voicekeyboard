use crate::types::{Action, ParsedOutput};

pub fn parse_output(raw: &str, shortcuts_enabled: bool) -> ParsedOutput {
    let trimmed = raw.trim();
    if let Some(parsed) = parse_json(trimmed) {
        return filter_shortcuts(parsed, shortcuts_enabled);
    }
    if let Some(json) = extract_json_object(trimmed).and_then(parse_json) {
        return filter_shortcuts(json, shortcuts_enabled);
    }
    ParsedOutput {
        actions: parse_shortcut_tags(trimmed, shortcuts_enabled),
        confidence: None,
    }
}

fn parse_json(text: &str) -> Option<ParsedOutput> {
    let parsed: ParsedOutput = serde_json::from_str(text).ok()?;
    if parsed.actions.is_empty() {
        return None;
    }
    Some(parsed)
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then_some(&text[start..=end])
}

fn filter_shortcuts(mut parsed: ParsedOutput, shortcuts_enabled: bool) -> ParsedOutput {
    expand_text_shortcut_tags(&mut parsed, shortcuts_enabled);
    normalize_shortcut_keys(&mut parsed);
    parsed.actions.retain(|action| match action {
        Action::Text { value } => !value.trim().is_empty(),
        Action::Shortcut { keys } => !keys.is_empty(),
    });
    if !shortcuts_enabled {
        parsed
            .actions
            .retain(|action| matches!(action, Action::Text { .. }));
    }
    parsed
}

fn expand_text_shortcut_tags(parsed: &mut ParsedOutput, shortcuts_enabled: bool) {
    let mut expanded = Vec::new();
    for action in std::mem::take(&mut parsed.actions) {
        match action {
            Action::Text { value } => {
                let mut parts = parse_unbraced_control_text(&value, shortcuts_enabled)
                    .unwrap_or_else(|| parse_shortcut_tags(&value, shortcuts_enabled));
                if parts.is_empty() && !value.trim().is_empty() {
                    parts.push(Action::Text { value });
                }
                expanded.extend(parts);
            }
            other => expanded.push(other),
        }
    }
    parsed.actions = expanded;
}

fn normalize_shortcut_keys(parsed: &mut ParsedOutput) {
    for action in &mut parsed.actions {
        if let Action::Shortcut { keys } = action {
            let normalized = keys
                .iter()
                .flat_map(|key| key.split('+'))
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
                .collect();
            *keys = normalized;
        }
    }
}

fn parse_shortcut_tags(text: &str, shortcuts_enabled: bool) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut rest = text;
    loop {
        let Some((start, open_len, close)) = find_next_shortcut_tag(rest) else {
            push_text(&mut actions, rest);
            break;
        };
        push_text(&mut actions, &rest[..start]);
        let after_start = &rest[start + open_len..];
        let Some(end) = after_start.find(close) else {
            push_text(&mut actions, &rest[start..]);
            break;
        };
        let tag = after_start[..end].trim();
        if is_shortcut_tag(tag) {
            if shortcuts_enabled {
                actions.push(Action::Shortcut {
                    keys: tag
                        .split('+')
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                        .collect(),
                });
            }
        } else {
            push_text(
                &mut actions,
                &rest[start..start + open_len + end + close.len()],
            );
        }
        rest = &after_start[end + close.len()..];
    }
    merge_adjacent_text(&mut actions);
    actions
}

fn find_next_shortcut_tag(text: &str) -> Option<(usize, usize, &'static str)> {
    let double = text.find("{{").map(|index| (index, 2, "}}"));
    let single = text.find('{').and_then(|index| {
        if text[index..].starts_with("{{") {
            None
        } else {
            Some((index, 1, "}"))
        }
    });
    match (double, single) {
        (Some(d), Some(s)) => Some(if d.0 <= s.0 { d } else { s }),
        (Some(d), None) => Some(d),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

fn is_shortcut_tag(tag: &str) -> bool {
    let keys: Vec<_> = tag
        .split('+')
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect();
    !keys.is_empty() && keys.iter().all(|key| is_known_key(key))
}

fn is_known_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "ctrl"
            | "control"
            | "shift"
            | "alt"
            | "win"
            | "windows"
            | "cmd"
            | "command"
            | "meta"
            | "enter"
            | "return"
            | "tab"
            | "escape"
            | "esc"
            | "space"
            | "backspace"
            | "delete"
            | "del"
            | "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "insert"
            | "ins"
            | "f1"
            | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
    ) || normalized.chars().count() == 1 && normalized.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn parse_unbraced_control_text(text: &str, shortcuts_enabled: bool) -> Option<Vec<Action>> {
    if !shortcuts_enabled {
        return None;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    if is_unbraced_control_key(trimmed) {
        return Some(vec![Action::Shortcut {
            keys: vec![canonical_control_key(trimmed).to_string()],
        }]);
    }

    for key in ["Enter", "Return", "Tab"] {
        let Some(prefix) = trimmed.strip_suffix(key) else {
            continue;
        };
        let prefix = prefix.trim_end();
        if looks_like_navigation_text(prefix) {
            return Some(vec![
                Action::Text {
                    value: prefix.to_string(),
                },
                Action::Shortcut {
                    keys: vec![canonical_control_key(key).to_string()],
                },
            ]);
        }
    }
    None
}

fn is_unbraced_control_key(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "enter"
            | "return"
            | "tab"
            | "escape"
            | "esc"
            | "backspace"
            | "delete"
            | "del"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "insert"
            | "ins"
            | "left"
            | "right"
            | "up"
            | "down"
    )
}

fn canonical_control_key(text: &str) -> &str {
    match text.trim().to_ascii_lowercase().as_str() {
        "return" => "Enter",
        "esc" => "Escape",
        "del" => "Delete",
        "ins" => "Insert",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "enter" => "Enter",
        "tab" => "Tab",
        "escape" => "Escape",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "home" => "Home",
        "end" => "End",
        "insert" => "Insert",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        _ => text.trim(),
    }
}

fn looks_like_navigation_text(text: &str) -> bool {
    let value = text.trim().to_ascii_lowercase();
    if value.is_empty() || value.contains(char::is_whitespace) {
        return false;
    }
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("www.")
        || [
            ".com", ".org", ".net", ".io", ".ai", ".dev", ".app", ".co", ".in",
        ]
        .iter()
        .any(|suffix| value.ends_with(suffix))
}

fn push_text(actions: &mut Vec<Action>, text: &str) {
    let value = text.trim();
    if !value.is_empty() {
        actions.push(Action::Text {
            value: value.to_string(),
        });
    }
}

fn merge_adjacent_text(actions: &mut Vec<Action>) {
    let mut merged = Vec::new();
    for action in std::mem::take(actions) {
        match (merged.last_mut(), action) {
            (Some(Action::Text { value: existing }), Action::Text { value }) => {
                if !existing.is_empty() && !value.is_empty() {
                    existing.push(' ');
                }
                existing.push_str(&value);
            }
            (_, action) => merged.push(action),
        }
    }
    *actions = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_text() {
        let parsed = parse_output("hello world", true);
        assert_eq!(
            parsed.actions,
            vec![Action::Text {
                value: "hello world".to_string()
            }]
        );
    }

    #[test]
    fn parses_shortcut_tags() {
        let parsed = parse_output("copy this {{Ctrl+C}} then enter {{Enter}}", true);
        assert_eq!(parsed.actions.len(), 4);
    }

    #[test]
    fn parses_json_actions() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"text","value":"hello"},{"type":"shortcut","keys":["Ctrl","C"]}],"confidence":0.9}"#,
            true,
        );
        assert_eq!(parsed.actions.len(), 2);
        assert_eq!(parsed.confidence, Some(0.9));
    }

    #[test]
    fn disables_shortcuts() {
        let parsed = parse_output("hello {{Ctrl+C}}", false);
        assert_eq!(
            parsed.actions,
            vec![Action::Text {
                value: "hello".to_string()
            }]
        );
    }

    #[test]
    fn normalizes_json_shortcut_plus_keys() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"shortcut","keys":["Ctrl+C"]}]}"#,
            true,
        );
        assert_eq!(
            parsed.actions,
            vec![Action::Shortcut {
                keys: vec!["Ctrl".to_string(), "C".to_string()]
            }]
        );
    }

    #[test]
    fn expands_shortcut_tags_inside_json_text_actions() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"text","value":"copy this {{Ctrl+C}} then {{Enter}}"}]}"#,
            true,
        );
        assert_eq!(parsed.actions.len(), 4);
        assert!(matches!(parsed.actions[1], Action::Shortcut { .. }));
    }

    #[test]
    fn expands_single_brace_shortcut_tokens_inside_json_text_actions() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"text","value":"youtube.com"},{"type":"text","value":"{Enter}"}]}"#,
            true,
        );
        assert_eq!(
            parsed.actions,
            vec![
                Action::Text {
                    value: "youtube.com".to_string()
                },
                Action::Shortcut {
                    keys: vec!["Enter".to_string()]
                }
            ]
        );
    }

    #[test]
    fn leaves_non_shortcut_braces_as_text() {
        let parsed = parse_output("write {name} literally", true);
        assert_eq!(
            parsed.actions,
            vec![Action::Text {
                value: "write {name} literally".to_string()
            }]
        );
    }

    #[test]
    fn converts_unbraced_enter_text_action_to_shortcut() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"text","value":"gmail.com"},{"type":"text","value":"Enter"}]}"#,
            true,
        );
        assert_eq!(
            parsed.actions,
            vec![
                Action::Text {
                    value: "gmail.com".to_string()
                },
                Action::Shortcut {
                    keys: vec!["Enter".to_string()]
                }
            ]
        );
    }

    #[test]
    fn splits_navigation_text_fused_with_enter() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"text","value":"gmail.comEnter"}]}"#,
            true,
        );
        assert_eq!(
            parsed.actions,
            vec![
                Action::Text {
                    value: "gmail.com".to_string()
                },
                Action::Shortcut {
                    keys: vec!["Enter".to_string()]
                }
            ]
        );
    }

    #[test]
    fn drops_whitespace_only_text_actions() {
        let parsed = parse_output(
            r#"{"actions":[{"type":"text","value":"google.com"},{"type":"text","value":"   "}]}"#,
            true,
        );
        assert_eq!(
            parsed.actions,
            vec![Action::Text {
                value: "google.com".to_string()
            }]
        );
    }
}
