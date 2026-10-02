mod support;

use cc_switch_lib::{AppType, Prompt, PromptService};
use std::fs;
use support::{create_test_state, ensure_test_home, reset_test_fs, test_mutex};

/// 保存停用条目不能清空 live 文件；只有「停用最后一条启用中的提示词」才清空。
///
/// 上游 `370d089b` 的测试覆盖 Claude / Codex / Hermes 三个应用。fork 裁掉了 Hermes
/// （`AppType` 只有 Claude / ClaudeDesktop / Codex，`AppSettings` 也没有
/// `hermes_config_dir`），所以这里只跑前两个；被测的 `clear_live` 分支与 Hermes 无关。
#[test]
fn saving_inactive_prompts_keeps_a_hand_written_file_until_the_last_enabled_one_is_disabled() {
    let _guard = test_mutex().lock().unwrap();
    reset_test_fs();
    let home = ensure_test_home();
    let state = create_test_state().unwrap();

    for (app, path) in [
        (AppType::Claude, home.join(".claude/CLAUDE.md")),
        (AppType::Codex, home.join(".codex/AGENTS.md")),
    ] {
        let hand_written = "hand-written instructions\n";
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, hand_written).unwrap();

        // Adding a new prompt from the UI saves it disabled.
        let draft = Prompt {
            id: "draft".into(),
            name: "Draft".into(),
            content: "draft content".into(),
            description: None,
            enabled: false,
            created_at: Some(1),
            updated_at: Some(1),
        };
        PromptService::upsert_prompt(&state, app.clone(), &draft.id, draft.clone()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), hand_written);

        // Re-saving an inactive prompt leaves the file alone as well.
        let edited = Prompt {
            content: "edited draft".into(),
            ..draft.clone()
        };
        PromptService::upsert_prompt(&state, app.clone(), "draft", edited).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), hand_written);

        // Importing copies the file into the database without emptying it.
        let imported_id = PromptService::import_from_file(&state, app.clone()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), hand_written);
        assert_eq!(
            state.db.get_prompts(app.as_str()).unwrap()[&imported_id].content,
            hand_written
        );

        // A deeplink saves disabled and then enables: the hand-written file
        // must still be readable when enable_prompt backs it up.
        fs::write(&path, "fresh hand-written\n").unwrap();
        let linked = Prompt {
            id: "linked".into(),
            content: "linked content".into(),
            ..draft.clone()
        };
        PromptService::upsert_prompt(&state, app.clone(), &linked.id, linked.clone()).unwrap();
        PromptService::enable_prompt(&state, app.clone(), &linked.id).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "linked content");
        assert!(state
            .db
            .get_prompts(app.as_str())
            .unwrap()
            .values()
            .any(|prompt| !prompt.enabled && prompt.content == "fresh hand-written\n"));

        // Disabling the last enabled prompt still empties the file.
        let disabled = Prompt {
            enabled: false,
            ..state.db.get_prompts(app.as_str()).unwrap()["linked"].clone()
        };
        PromptService::upsert_prompt(&state, app.clone(), "linked", disabled).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }
}
