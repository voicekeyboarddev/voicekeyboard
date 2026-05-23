use crate::{settings::Settings, types::Action};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum SafetyTier {
    Allow,
    Confirm,
    Block,
}

#[derive(Debug, Clone, Serialize)]
pub struct SafetyDecision {
    pub tier: SafetyTier,
    pub reason: String,
}

pub fn evaluate(actions: &[Action], settings: &Settings) -> SafetyDecision {
    let text_chars: usize = actions
        .iter()
        .map(|action| match action {
            Action::Text { value } => value.chars().count(),
            Action::Shortcut { .. } => 0,
            Action::Prompt | Action::Agentic => 0,
        })
        .sum();
    if text_chars >= settings.confirm_large_text_chars {
        return SafetyDecision {
            tier: SafetyTier::Confirm,
            reason: format!("large text insertion: {text_chars} characters"),
        };
    }

    let mut destructive_count = 0;
    for action in actions {
        let Action::Shortcut { keys } = action else {
            continue;
        };
        let normalized: Vec<String> = keys.iter().map(|k| normalize(k)).collect();
        if is_blocked(&normalized) {
            return SafetyDecision {
                tier: SafetyTier::Block,
                reason: format!("blocked shortcut: {}", keys.join("+")),
            };
        }
        if is_close_shortcut(&normalized) && settings.confirm_close_shortcuts {
            return SafetyDecision {
                tier: SafetyTier::Confirm,
                reason: format!("close-window/tab shortcut: {}", keys.join("+")),
            };
        }
        if normalized
            .iter()
            .any(|k| matches!(k.as_str(), "delete" | "backspace"))
        {
            destructive_count += 1;
        }
    }

    if destructive_count >= 3 {
        SafetyDecision {
            tier: SafetyTier::Confirm,
            reason: "repeated destructive keys".to_string(),
        }
    } else {
        SafetyDecision {
            tier: SafetyTier::Allow,
            reason: "allowed".to_string(),
        }
    }
}

fn normalize(key: &str) -> String {
    match key.trim().to_ascii_lowercase().as_str() {
        "control" => "ctrl".to_string(),
        "windows" | "cmd" | "command" | "meta" => "win".to_string(),
        other => other.to_string(),
    }
}

fn has(keys: &[String], key: &str) -> bool {
    keys.iter().any(|k| k == key)
}

fn is_close_shortcut(keys: &[String]) -> bool {
    (has(keys, "alt") && has(keys, "f4")) || (has(keys, "ctrl") && has(keys, "w"))
}

fn is_blocked(keys: &[String]) -> bool {
    if has(keys, "win") && (has(keys, "l") || has(keys, "r") || has(keys, "x")) {
        return true;
    }
    if has(keys, "ctrl") && has(keys, "alt") && has(keys, "delete") {
        return true;
    }
    if has(keys, "alt") && has(keys, "tab") {
        return true;
    }
    keys.iter()
        .filter(|k| matches!(k.as_str(), "ctrl" | "alt" | "shift" | "win"))
        .count()
        >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_simple_text() {
        let decision = evaluate(
            &[Action::Text {
                value: "hello".to_string(),
            }],
            &Settings::default(),
        );
        assert_eq!(decision.tier, SafetyTier::Allow);
    }

    #[test]
    fn confirms_large_text() {
        let mut settings = Settings::default();
        settings.confirm_large_text_chars = 5;
        let decision = evaluate(
            &[Action::Text {
                value: "hello world".to_string(),
            }],
            &settings,
        );
        assert_eq!(decision.tier, SafetyTier::Confirm);
    }

    #[test]
    fn blocks_os_shortcuts() {
        let decision = evaluate(
            &[Action::Shortcut {
                keys: vec!["Win".to_string(), "L".to_string()],
            }],
            &Settings::default(),
        );
        assert_eq!(decision.tier, SafetyTier::Block);
    }
}
