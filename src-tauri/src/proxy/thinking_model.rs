//! Claude model thinking capability helpers used by protocol transforms.

pub(crate) fn uses_adaptive_thinking(model: &str) -> bool {
    let normalized = normalize_model_name(model);
    [
        "fable-5",
        "mythos-5",
        "mythos-preview",
        "sonnet-5",
        "opus-5",
        "opus-4-8",
        "opus-4-7",
        "opus-4-6",
        "sonnet-4-6",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

pub(crate) fn adaptive_thinking_is_default(model: &str) -> bool {
    let normalized = normalize_model_name(model);
    ["fable-5", "mythos-5", "mythos-preview", "sonnet-5"]
        .iter()
        .any(|needle| normalized.contains(needle))
}

pub(crate) fn thinking_cannot_be_disabled(model: &str) -> bool {
    let normalized = normalize_model_name(model);
    ["fable-5", "mythos-5"]
        .iter()
        .any(|needle| normalized.contains(needle))
}

fn normalize_model_name(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['.', '_'], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_adaptive_model_aliases() {
        assert!(uses_adaptive_thinking("anthropic.claude-opus-4_8"));
        assert!(uses_adaptive_thinking("claude-opus-5"));
        assert!(adaptive_thinking_is_default("fable.5"));
        assert!(thinking_cannot_be_disabled("mythos_5"));
        assert!(!thinking_cannot_be_disabled("claude-opus-4-8"));
    }
}
