    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn claude_temp_configs_are_unique_and_keep_secrets_private() {
        let env = vec![("ANTHROPIC_AUTH_TOKEN".to_string(), "secret".to_string())];
        let first = write_claude_config("provider/one", &env).expect("first temp config");
        let second = write_claude_config("provider/one", &env).expect("second temp config");

        assert_ne!(first, second);
        assert!(first
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("claude_provider_one_")));
        assert!(std::fs::read_to_string(&first)
            .expect("read temp config")
            .contains("secret"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first)
                    .expect("temp config metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        std::fs::remove_file(first).expect("remove first temp config");
        std::fs::remove_file(second).expect("remove second temp config");
    }

    #[cfg(unix)]
    fn set_test_executable(path: &Path, executable: bool) {
        use std::os::unix::fs::PermissionsExt;

        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .expect("fixture permissions should be set");
    }

    #[test]
    fn test_build_exec_line() {
        assert_eq!(build_exec_line("/bin/zsh", None), "exec '/bin/zsh' -l");
        assert_eq!(build_exec_line("/bin/bash", None), "exec '/bin/bash'");
        assert_eq!(
            build_exec_line("/opt/homebrew dir/bin/fish", None),
            "exec '/opt/homebrew dir/bin/fish'"
        );
        assert_eq!(build_exec_line("/bin/sh", None), "exec '/bin/sh'");
        assert_eq!(
            build_exec_line("/tmp/shell'quote/zsh", None),
            "exec '/tmp/shell'\"'\"'quote/zsh' -l"
        );
        assert_eq!(
            build_exec_line("/bin/zsh", Some(Path::new("/tmp/project"))),
            r#"exec '/bin/zsh' -lc 'cd '"'"'/tmp/project'"'"' || exit 1; exec '"'"'/bin/zsh'"'"' -i'"#
        );
    }

    #[test]
    fn test_build_provider_command_line_uses_user_shell_environment() {
        assert_eq!(
            build_provider_command_line("/bin/zsh", "/tmp/claude config.json", None),
            "'/bin/zsh' -lic 'claude --settings '\"'\"'/tmp/claude config.json'\"'\"''"
        );
        assert_eq!(
            build_provider_command_line(
                "/bin/bash",
                "/tmp/claude config.json",
                Some(Path::new("/tmp/project"))
            ),
            r#"'/bin/bash' -ic 'cd '"'"'/tmp/project'"'"' && claude --settings '"'"'/tmp/claude config.json'"'"''"#
        );
        assert_eq!(
            build_provider_command_line(
                "/bin/sh",
                "/tmp/claude config.json",
                Some(Path::new("/tmp/project O'Brien"))
            ),
            r#"'/bin/sh' -c 'cd '"'"'/tmp/project O'"'"'"'"'"'"'"'"'Brien'"'"' && claude --settings '"'"'/tmp/claude config.json'"'"''"#
        );
    }

    #[test]
    fn test_build_final_shell_cd_command() {
        assert_eq!(build_final_shell_cd_command("/bin/zsh", None), "");
        assert_eq!(
            build_final_shell_cd_command("/bin/zsh", Some(Path::new("/tmp/project"))),
            ""
        );
        assert_eq!(
            build_final_shell_cd_command("/bin/bash", Some(Path::new("/tmp/project O'Brien"))),
            "cd '/tmp/project O'\"'\"'Brien' || exit 1\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_get_user_shell_fallback() {
        // $SHELL 未设置时应按平台 fallback
        // 此测试验证 fallback 逻辑，但不验证环境变量值（取决于运行环境）
        let shell = get_user_shell();
        // 至少应返回一个合法的绝对路径
        assert!(valid_user_shell_path(&shell));
        // basename 应为合法 shell 名
        let basename = shell.rsplit('/').next().unwrap_or("sh");
        assert!(["sh", "bash", "zsh", "fish", "dash"].contains(&basename));
    }

    #[cfg(unix)]
    #[test]
    fn test_valid_user_shell_path() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let executable_zsh = temp.path().join("zsh");
        std::fs::write(&executable_zsh, "#!/usr/bin/env sh\n")
            .expect("shell fixture should be written");
        set_test_executable(&executable_zsh, true);

        let executable_fish_dir = temp.path().join("homebrew dir/bin");
        std::fs::create_dir_all(&executable_fish_dir)
            .expect("shell fixture directory should be created");
        let executable_fish = executable_fish_dir.join("fish");
        std::fs::write(&executable_fish, "#!/usr/bin/env sh\n")
            .expect("shell fixture should be written");
        set_test_executable(&executable_fish, true);

        let non_executable_bash = temp.path().join("bash");
        std::fs::write(&non_executable_bash, "#!/usr/bin/env sh\n")
            .expect("shell fixture should be written");
        set_test_executable(&non_executable_bash, false);

        assert!(valid_user_shell_path(&executable_zsh.to_string_lossy()));
        assert!(valid_user_shell_path(&executable_fish.to_string_lossy()));
        assert!(!valid_user_shell_path(""));
        assert!(!valid_user_shell_path("zsh"));
        assert!(!valid_user_shell_path(
            &temp.path().join("missing/zsh").to_string_lossy()
        ));
        assert!(!valid_user_shell_path(
            &non_executable_bash.to_string_lossy()
        ));
        assert!(!valid_user_shell_path(
            &temp.path().join("zsh; rm -rf /").to_string_lossy()
        ));
        assert!(!valid_user_shell_path(&format!(
            "{}\n/bin/bash",
            executable_zsh.to_string_lossy()
        )));
        assert!(!valid_user_shell_path("/usr/bin/powershell"));
    }

    #[test]
    fn test_extract_version() {
        assert_eq!(extract_version("claude 1.0.20"), "1.0.20");
        assert_eq!(extract_version("v2.3.4-beta.1"), "2.3.4-beta.1");
        assert_eq!(extract_version("no version here"), "no version here");
    }

    #[test]
    fn test_compare_semver() {
        use std::cmp::Ordering;
        assert_eq!(
            compare_semver("2.1.156", "2.1.154"),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_semver("2.1.154", "2.1.156"), Some(Ordering::Less));
        assert_eq!(compare_semver("2.1.156", "2.1.156"), Some(Ordering::Equal));
        // 预发布 < 同核心正式版
        assert_eq!(
            compare_semver("2.1.156-beta.1", "2.1.156"),
            Some(Ordering::Less)
        );
        // core 更高的预发布仍高于较低的正式版
        assert_eq!(
            compare_semver("0.45.0-nightly.1", "0.44.1"),
            Some(Ordering::Greater)
        );
        // 大 patch（codex 时间戳式）不溢出
        assert_eq!(
            compare_semver("0.1.2505172116", "0.135.0"),
            Some(Ordering::Less)
        );
        // 无法解析返回 None
        assert_eq!(compare_semver("false", "1.0.0"), None);
    }

    #[test]
    fn test_pick_latest_version() {
        use serde_json::json;
        let tags = json!({
            "latest": "2.1.154",
            "next": "2.1.156",
            "stable": "2.1.145"
        });
        let map = tags.as_object().unwrap();

        // 本地领先 latest（在 next 通道）→ 补查到 next，数字对齐
        assert_eq!(
            pick_latest_version(map, &["next"], Some("2.1.156")),
            Some("2.1.156".to_string())
        );
        // 本地等于 latest → 不补查，仍显示 latest
        assert_eq!(
            pick_latest_version(map, &["next"], Some("2.1.154")),
            Some("2.1.154".to_string())
        );
        // 本地落后 latest（稳定通道用户）→ 不补查，不被推向预发布版
        assert_eq!(
            pick_latest_version(map, &["next"], Some("2.1.145")),
            Some("2.1.154".to_string())
        );
        // 无预发布白名单 → 永远只看 latest（不解析 local，避免脏 local 触发）
        assert_eq!(
            pick_latest_version(map, &[], Some("2.1.156")),
            Some("2.1.154".to_string())
        );
        // 本地版本未知 → 保守只看 latest
        assert_eq!(
            pick_latest_version(map, &["next"], None),
            Some("2.1.154".to_string())
        );
    }

    #[test]
    fn test_pick_latest_version_filters_dirty_prerelease() {
        use serde_json::json;
        // 模拟 codex：beta 是低于 latest 的时间戳式脏版本
        let tags = json!({
            "latest": "0.135.0",
            "beta": "0.1.2505172116"
        });
        let map = tags.as_object().unwrap();
        // 即便本地领先 latest，低于 latest 的脏 beta 也不会被选
        assert_eq!(
            pick_latest_version(map, &["beta"], Some("0.200.0")),
            Some("0.135.0".to_string())
        );
    }

    /// `parent_dir` 是锚定层"由 bin 路径推导同目录绝对路径"的基石,跨平台共用——
    /// 这里固化 `\`/`/`/混合分隔符/根边界四种情况,避免未来重构悄悄改语义。
    mod parent_dir_cases {
        use super::super::*;

        #[test]
        fn unix_path() {
            assert_eq!(
                parent_dir("/Users/me/.volta/bin/codex"),
                "/Users/me/.volta/bin"
            );
        }

        #[test]
        fn windows_backslash() {
            assert_eq!(
                parent_dir("C:\\Users\\me\\AppData\\Local\\Volta\\bin\\codex.exe"),
                "C:\\Users\\me\\AppData\\Local\\Volta\\bin"
            );
        }

        #[test]
        fn mixed_separators_takes_rightmost() {
            // Windows 上 `Path::join` 与字符串拼接可能产出混合分隔符;取**两种之中最右
            // 出现**的位置,而非"优先 `\`"——后者在混合时会取错父目录。
            assert_eq!(
                parent_dir("C:\\Users\\me/Code/project\\codex.cmd"),
                "C:\\Users\\me/Code/project"
            );
        }

        #[test]
        fn no_separator_returns_empty() {
            // 无父目录 → 空串,锚定层据此返 None、回退静态命令。
            assert_eq!(parent_dir("codex"), "");
        }

        #[test]
        fn separator_at_root_returns_empty() {
            // `/codex`:根目录是 index 0,`i > 0` 不满足 → 空串。同款行为对 Windows
            // 上的 `\codex` 也成立(实际不会出现,但语义对齐)。
            assert_eq!(parent_dir("/codex"), "");
            assert_eq!(parent_dir("\\codex"), "");
        }
    }

    /// Windows-only 锚定升级回归(等价类压缩到 3 种 idiom:volta/pnpm/npm)。整块通过
    /// `cfg(target_os = "windows")` gate,在 macOS/Linux 上不参与 cargo test;Windows
    /// CI 跑全套验证。tempdir 模拟 sibling 入口存在/不存在,锁定"扩展名顺序优先级 +
    /// 含空格路径自动加双引号 + 探不到 sibling → None 退静态"三件事。
    #[cfg(target_os = "windows")]
    mod anchored_upgrade_windows {
        use super::super::*;

        /// 在 tempdir 下创建子目录 `subdir`(空字符串则用 tempdir 根),放入 `entry`
        /// 与若干 `siblings` 假文件。返回 `(TempDir, 子目录, 入口绝对路径)`——TempDir
        /// 必须保活,否则析构后 fs 文件消失、`is_file()` 失败,测试假绿。
        fn setup_sibling(
            subdir: &str,
            entry: &str,
            siblings: &[&str],
        ) -> (tempfile::TempDir, std::path::PathBuf, String) {
            let dir = tempfile::tempdir().unwrap();
            let sub = if subdir.is_empty() {
                dir.path().to_path_buf()
            } else {
                dir.path().join(subdir)
            };
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join(entry), "").unwrap();
            for s in siblings {
                std::fs::write(sub.join(s), "").unwrap();
            }
            let bin_path = sub.join(entry).to_string_lossy().to_string();
            (dir, sub, bin_path)
        }

        /// **必须与 `win_quote_path_for_batch` 主体保持镜像**——给 anchored 测试动态算
        /// expected,让用例在 temp 根目录含空格 / `&` / `(` / `%` 等特殊字符的开发机上
        /// 也能通过(默认 Windows `%TEMP%` = `C:\Users\<user>\AppData\Local\Temp`,
        /// 用户名带空格的机器整条 path 含空格、生产代码会正确加引号、测试硬编码无引号
        /// expected 会假失败)。
        ///
        /// 镜像引入"两边必须同步"的隐性依赖——回归防护层是 `win_quote_*` 那 7 个独立
        /// 单测,它们用硬编码字面值锁住 quoting 规则本身,即便此镜像漂移也会被那一组
        /// 测试 catch;反之亦然。
        fn expect_quoted_path(p: &str) -> String {
            let escaped = p.replace('%', "%%%%");
            let needs_quote = p
                .chars()
                .any(|c| matches!(c, ' ' | '&' | '(' | ')' | '^' | ';' | '<' | '>' | '|' | ','));
            if needs_quote {
                format!("\"{escaped}\"")
            } else {
                escaped
            }
        }

        #[test]
        fn volta_windows_uses_volta_install() {
            // tempdir 路径里不含 "volta" 子串,所以在 tempdir 下手建一个 `Volta` 子目录
            // 才能让 `infer_install_source` 通过路径 normalize 后命中 `/volta/` 分支。
            // sibling 候选顺序 `[exe, cmd]`——Volta 是 Rust 写的 native binary,首选 .exe。
            // expected 通过 `expect_quoted_path` 算出,以适应 temp 根目录含特殊字符的环境。
            let (_dir, sub, bin_path) = setup_sibling("Volta", "codex.cmd", &["volta.exe"]);
            let cmd = anchored_command_from_paths("codex", &bin_path, &bin_path);
            let volta_full = format!("{}\\volta.exe", sub.to_string_lossy());
            let expected = format!(
                "{} update || call {} install @openai/codex",
                expect_quoted_path(&bin_path),
                expect_quoted_path(&volta_full)
            );
            assert_eq!(cmd.as_deref(), Some(expected.as_str()));
        }

        #[test]
        fn pnpm_windows_uses_pnpm_add() {
            // bin_path 落 `%LOCALAPPDATA%\pnpm\codex.cmd`,sibling 有 `pnpm.cmd` → 锚定到
            // `<dir>\pnpm.cmd add -g @openai/codex@latest`。用 add+@latest 而非 update,
            // 兼容"之前没通过 pnpm 装过"的幂等性场景。
            let (_dir, sub, bin_path) = setup_sibling("pnpm", "codex.cmd", &["pnpm.cmd"]);
            let cmd = anchored_command_from_paths("codex", &bin_path, &bin_path);
            let pnpm_full = format!("{}\\pnpm.cmd", sub.to_string_lossy());
            let expected = format!(
                "{} update || call {} add -g @openai/codex@latest",
                expect_quoted_path(&bin_path),
                expect_quoted_path(&pnpm_full)
            );
            assert_eq!(cmd.as_deref(), Some(expected.as_str()));
        }

        #[test]
        fn npm_windows_default_branch() {
            // 任意 system 类路径(不命中 volta/pnpm)→ 兜底 sibling npm.cmd 锚定。
            // 模拟 nvm-windows 的实际形态:`<NVM_HOME>\v22.0.0\codex.cmd`。
            let (_dir, sub, bin_path) = setup_sibling("v22.0.0", "codex.cmd", &["npm.cmd"]);
            let cmd = anchored_command_from_paths("codex", &bin_path, &bin_path);
            let npm_full = format!("{}\\npm.cmd", sub.to_string_lossy());
            let expected = format!(
                "{} update || call {} i -g @openai/codex@latest",
                expect_quoted_path(&bin_path),
                expect_quoted_path(&npm_full)
            );
            assert_eq!(cmd.as_deref(), Some(expected.as_str()));
        }

        #[test]
        fn windows_no_sibling_uses_cli_update_without_package_fallback() {
            // sibling npm.cmd 不存在(纯独立二进制)时,仍可锚定到 CLI 自身跑官方 update。
            // 只是没有包管理器 fallback。
            let (_dir, _sub, bin_path) = setup_sibling("", "codex.cmd", &[]);
            let cmd = anchored_command_from_paths("codex", &bin_path, &bin_path);
            let expected = format!("{} update", expect_quoted_path(&bin_path));
            assert_eq!(cmd.as_deref(), Some(expected.as_str()));
        }

        #[test]
        fn windows_path_with_space_is_double_quoted() {
            // 含空格的路径(`C:\Program Files\...`)在生成命令时必须用双引号包,否则
            // bat / cmd /C 解析会把第一个空格当 token 分隔符,后续参数串错。**精确等值断言
            // 锁定引号位置**(starts_with+contains 会放过"双引号位置错了但仍能命中"的回归)。
            let (_dir, sub, bin_path) = setup_sibling("Program Files", "codex.cmd", &["npm.cmd"]);
            let cmd = anchored_command_from_paths("codex", &bin_path, &bin_path);
            let npm_full = format!("{}\\npm.cmd", sub.to_string_lossy());
            let expected = format!(
                "{} update || call {} i -g @openai/codex@latest",
                expect_quoted_path(&bin_path),
                expect_quoted_path(&npm_full)
            );
            assert_eq!(cmd.as_deref(), Some(expected.as_str()));
        }

        #[test]
        fn windows_full_batch_line_for_percent_path_uses_quadruple_escape() {
            // **完整生成的 batch 行**(`call ` + anchored cmd)对含字面 `%` 的路径必须
            // 4 倍转义 `%foo%` → `%%%%foo%%%%`:.bat parser 一轮还原为 `%%foo%%`,call
            // 二轮再还原为 `%foo%` 字面。helper 单测验证的是 `win_quote_path_for_batch`
            // 内部转义,这条 integration 测验证 anchored_command_from_paths 输出 + call
            // 包装后,**最终落到 .bat 的字符串**仍然闭合两轮 expansion。
            let (_dir, sub, bin_path) = setup_sibling("path%foo%", "codex.cmd", &["npm.cmd"]);
            let anchored = anchored_command_from_paths("codex", &bin_path, &bin_path).unwrap();
            // build_tool_action_line Windows 分支最终拼的就是 `call <anchored>`(中间
            // 没有其他变换),这里直接用 format! 复刻那一步,无需暴露内部 API。
            let batch_line = format!("call {anchored}");
            // 用 `expect_quoted_path` 算 npm 全路径的期望 quoting,**同时覆盖 temp 根
            // 含空格的环境**(否则 sub 本身含空格 + 子目录 `path%foo%` 触发 4 倍 `%` 转义
            // 会让 expected 漏引号、假失败)。
            let npm_full = format!("{}\\npm.cmd", sub.to_string_lossy());
            let expected = format!(
                "call {} update || call {} i -g @openai/codex@latest",
                expect_quoted_path(&bin_path),
                expect_quoted_path(&npm_full)
            );
            assert_eq!(batch_line, expected);
            // 双重锁定:确认 4 倍转义子串存在 + 不出现"残留的二倍转义或字面 `%foo%`"。
            assert!(
                batch_line.contains("%%%%foo%%%%"),
                "batch 行应含 4 倍转义 `%%%%foo%%%%`: {batch_line}"
            );
            assert!(
                !batch_line.contains("path%foo%"),
                "batch 行不应含未转义的字面 `%foo%`(会被 call 二次解析展开): {batch_line}"
            );
        }
    }

    /// Windows-only helpers 单测——在 macOS/Linux 上整块通过 cfg 排除,不参与 `cargo test`。
    /// Windows CI(或本机 Windows 跑 cargo test)会激活这些用例。覆盖:①双引号
    /// quoting 镜像 POSIX 版;②sibling_bin_with_ext 在 fs 上按 ext 顺序探到第一个存在的、
    /// 全部不存在/空 dir 时返 None。tempdir 提供干净 fs 沙盒。
    #[cfg(target_os = "windows")]
    mod windows_helpers {
        use super::super::*;

        #[test]
        fn win_quote_clean_path_stays_bare() {
            // 普通路径不含特殊字符 → 不加引号,命令展示干净。
            assert_eq!(
                win_quote_path_for_batch("C:\\Users\\me\\npm.cmd"),
                "C:\\Users\\me\\npm.cmd"
            );
        }

        #[test]
        fn win_quote_spaced_path_gets_quoted() {
            assert_eq!(
                win_quote_path_for_batch("C:\\Program Files\\nodejs\\npm.cmd"),
                "\"C:\\Program Files\\nodejs\\npm.cmd\""
            );
        }

        #[test]
        fn win_quote_ampersand_path_gets_quoted() {
            // `&` 是 cmd 命令分隔符,NTFS 允许在路径中出现;没有引号会让 `call C:\A&B\npm.cmd`
            // 被解析为 `call C:\A` + `B\npm.cmd` 两条命令,执行错乱。
            assert_eq!(
                win_quote_path_for_batch("C:\\Tools&Dev\\npm.cmd"),
                "\"C:\\Tools&Dev\\npm.cmd\""
            );
        }

        #[test]
        fn win_quote_parens_path_gets_quoted() {
            // `(` / `)` 在 .bat 中是代码块语义,引号内才是字面意义。
            assert_eq!(
                win_quote_path_for_batch("C:\\Foo(x86)\\npm.cmd"),
                "\"C:\\Foo(x86)\\npm.cmd\""
            );
        }

        #[test]
        fn win_quote_caret_path_gets_quoted() {
            // `^` 是 cmd 的 escape character;包引号后是字面意义。
            assert_eq!(
                win_quote_path_for_batch("C:\\foo^bar\\npm.cmd"),
                "\"C:\\foo^bar\\npm.cmd\""
            );
        }

        #[test]
        fn win_quote_percent_is_escaped_to_quadruple_percent() {
            // `%` 经历 .bat 一轮 + call 二轮 expansion,要让 call 最终看到字面 `%FOO%`
            // 需要源 .bat 里写 `%%%%FOO%%%%`(一轮 → `%%FOO%%`,二轮 → `%FOO%` 字面)。
            // 用 `%%` 二倍转义只在 echo / 直接执行场景对,call 调用时会被还原成 variable
            // reference 进而被替换。**这一条用例锁住"call 二次解析"必须被 4 倍转义闭合**。
            assert_eq!(
                win_quote_path_for_batch("C:\\path%foo%\\npm.cmd"),
                "C:\\path%%%%foo%%%%\\npm.cmd"
            );
        }

        #[test]
        fn win_quote_percent_with_space_gets_both() {
            // `%` 4 倍转义与外层引号正交——含空格触发引号、含 `%` 触发 `%%%%` 转义,叠加。
            assert_eq!(
                win_quote_path_for_batch("C:\\my %dir%\\npm.cmd"),
                "\"C:\\my %%%%dir%%%%\\npm.cmd\""
            );
        }

        #[test]
        fn win_quote_needs_quote_uses_original_path() {
            // 回归 guard:`needs_quote` 判定基于**原路径**,不能用 escape 后字符串——
            // 否则原本无 token 边界字符的路径(如 `C:\path%foo%\npm.cmd`)在 escape
            // 引入更多 `%` 后被错误识别成"需要引号"。这是实现 bug 的隐性入口。
            // 入参不含任何 token 边界字符 → 不应加外层引号、只做 `%` 4 倍转义。
            let out = win_quote_path_for_batch("C:\\foo%bar%\\npm.cmd");
            assert!(!out.starts_with('"'), "纯 `%` 路径不应加外层引号: {out}");
        }

        #[test]
        fn sibling_bin_picks_first_existing_extension() {
            // 同目录同时存在 `npm.cmd` 和 `npm.exe` 时,候选顺序 `[cmd, exe]` 应取 .cmd——
            // 这是 Node.js 官方 installer 装出来的 idiom(.cmd 是入口、.exe 是 wrapper)。
            let dir = tempfile::tempdir().unwrap();
            let cmd_path = dir.path().join("npm.cmd");
            let exe_path = dir.path().join("npm.exe");
            std::fs::write(&cmd_path, "").unwrap();
            std::fs::write(&exe_path, "").unwrap();

            let codex = dir.path().join("codex.cmd").to_string_lossy().to_string();
            let found = sibling_bin_with_ext(&codex, "npm", &["cmd", "exe"]).unwrap();
            assert_eq!(found, cmd_path.to_string_lossy());
        }

        #[test]
        fn sibling_bin_volta_prefers_exe() {
            // Volta 是 Rust 写的 native binary,扩展名顺序应是 [exe, cmd]——若只有 .exe
            // 存在(常见情形),探到的就是它。
            let dir = tempfile::tempdir().unwrap();
            let exe_path = dir.path().join("volta.exe");
            std::fs::write(&exe_path, "").unwrap();

            let codex = dir.path().join("codex.exe").to_string_lossy().to_string();
            let found = sibling_bin_with_ext(&codex, "volta", &["exe", "cmd"]).unwrap();
            assert_eq!(found, exe_path.to_string_lossy());
        }

        #[test]
        fn sibling_bin_returns_none_when_none_exist() {
            // 同目录下没有任何候选 → None,锚定层据此退到静态命令。
            let dir = tempfile::tempdir().unwrap();
            let codex = dir.path().join("codex.cmd").to_string_lossy().to_string();
            assert!(sibling_bin_with_ext(&codex, "npm", &["cmd", "exe"]).is_none());
        }

        #[test]
        fn sibling_bin_returns_none_when_no_parent() {
            // bin_path 没有目录部分(纯文件名) → parent_dir 空串 → 返 None。
            assert!(sibling_bin_with_ext("codex.cmd", "npm", &["cmd"]).is_none());
        }

        #[test]
        fn wsl_install_uses_posix_install_priority() {
            let claude =
                wsl_tool_action_shell_command("claude", ToolLifecycleAction::Install).unwrap();
            assert!(
                claude.starts_with("bash -c 'tmp=$(mktemp) && curl -fsSL https://claude.ai/install.sh ")
                    && claude.contains(" || npm i -g @anthropic-ai/claude-code@latest"),
                "WSL claude install should prefer native POSIX installer with npm fallback: {claude}"
            );
            assert!(!claude.contains("| bash"));

            let codex =
                wsl_tool_action_shell_command("codex", ToolLifecycleAction::Install).unwrap();
            assert_eq!(codex, "npm i -g @openai/codex@latest");
        }

        #[test]
        fn wsl_npm_tools_use_posix_update_chain_without_batch_call() {
            // WSL 内跑的是 POSIX shell,不能带 Windows batch 的 `call`。同时 update
            // fallback 仍应先尝试官方 CLI 自升级。
            let cmd = wsl_tool_action_shell_command("claude", ToolLifecycleAction::Update).unwrap();
            assert_eq!(
                cmd,
                "claude update || npm i -g @anthropic-ai/claude-code@latest"
            );
        }
    }

    /// `infer_install_source` 是判定锚定 idiom 的入口——nvm/homebrew/volta/pnpm/...
    /// 各对应不同的升级命令形态。函数内部已 `replace('\\','/').to_ascii_lowercase()`
    /// 归一化,Windows 反斜杠 + 大小写差异在此处不需要分平台。这里固化"哪条路径
    /// 算哪种来源"的归类断言,避免未来调整子串顺序时静默改变分类。
    mod install_source_classification {
        use super::super::*;
        use std::path::Path;

        #[test]
        fn macos_volta_with_dot_prefix() {
            assert_eq!(
                infer_install_source(Path::new("/Users/me/.volta/bin/codex")),
                "volta"
            );
        }

        #[test]
        fn windows_volta_localappdata_no_dot() {
            // `%LOCALAPPDATA%\Volta\bin\codex.exe` —— 没有前导点,靠兜底的 `/volta/`
            // 命中(归一化后小写)。如果只识别 `/.volta/`,Windows 这一类会落到 system。
            assert_eq!(
                infer_install_source(Path::new(
                    "C:\\Users\\me\\AppData\\Local\\Volta\\bin\\codex.exe"
                )),
                "volta"
            );
        }

        #[test]
        fn windows_pnpm_localappdata() {
            // `%LOCALAPPDATA%\pnpm\codex.cmd` —— pnpm 全局 bin 目录,识别为 pnpm 后
            // 锚定命令走 `pnpm add -g <pkg>@latest`,而不是 sibling npm。
            assert_eq!(
                infer_install_source(Path::new("C:\\Users\\me\\AppData\\Local\\pnpm\\codex.cmd")),
                "pnpm"
            );
        }

        #[test]
        fn windows_nvm_falls_back_to_system() {
            // nvm-windows 安装的工具路径不含 `.nvm`(它通常装在 `%APPDATA%\nvm` 或
            // `C:\Program Files\nodejs` symlink),刻意不识别成专属 source——锚定层
            // 会按 system → sibling npm.cmd 处理,跟 nvm-windows 的实际 idiom 一致
            // (它的全局包就是当前选中的 node 的 npm 装的)。
            assert_eq!(
                infer_install_source(Path::new(
                    "C:\\Users\\me\\AppData\\Roaming\\nvm\\v22.0.0\\codex.cmd"
                )),
                "system"
            );
        }

        #[test]
        fn windows_scoop_still_identified() {
            // Scoop 已有 `/scoop/` 分支;我们的 6 个工具都不是 scoop formula,所以这条
            // 实际不影响锚定决策(锚定层会用 sibling npm.cmd),但归类保留方便未来。
            assert_eq!(
                infer_install_source(Path::new("C:\\Users\\me\\scoop\\shims\\codex.cmd")),
                "scoop"
            );
        }
    }

    /// 锚定升级命令生成：用真实勘察到的安装路径固化为回归断言——
    /// 一台机器上 4 个工具恰好对应 4 种升级方式（原生 self-update / brew / nvm npm /
    /// homebrew npm），任何改动若打破其中一种都会立刻被这些用例拦下。
    #[cfg(not(target_os = "windows"))]
    mod anchored_upgrade {
        use super::super::*;
        use std::path::Path;

        fn inst(path: &str, is_default: bool) -> ToolInstallation {
            ToolInstallation {
                path: path.to_string(),
                version: None,
                runnable: true,
                error: None,
                source: infer_install_source(Path::new(path)).to_string(),
                is_path_default: is_default,
                // 测试场景下不需要走 fs canonicalize——POSIX 锚定测试关心的是
                // path/real 都被传给 anchored_command_from_paths 的纯字符串判定,
                // 已有用例(brew_formula_extraction / claude_native_*)是直接
                // 调 anchored_command_from_paths,不通过 installs_anchored_command,
                // 这里 real 是给上层 default_install + read 用,填同值即可。
                real: std::path::PathBuf::from(path),
            }
        }

        #[test]
        fn claude_native_installer_uses_self_update() {
            // ~/.local/bin/claude → 真身在 ~/.local/share/claude/versions/,自带 self-update;
            // 它不归 npm 管,且在 PATH 里比 nvm/homebrew 更靠前,用 npm 升级纯属白装。
            // **绝对路径调用 launcher** 避免 GUI 非登录 `bash -c` 时 PATH 没有
            // ~/.local/bin 导致 `claude: not found`(exit 127)而失败。
            let cmd = anchored_command_from_paths(
                "claude",
                "/Users/me/.local/bin/claude",
                "/Users/me/.local/share/claude/versions/2.1.146",
            );
            assert_eq!(cmd.as_deref(), Some("/Users/me/.local/bin/claude update"));
        }

        #[test]
        fn codex_homebrew_formula_uses_brew_not_self_update() {
            // Homebrew formula 归 brew 管理;即使 Codex 有 self-update,也不先改动
            // Cellar 内的安装内容。
            let cmd = anchored_command_from_paths(
                "codex",
                "/opt/homebrew/bin/codex",
                "/opt/homebrew/Cellar/codex/1.2.3/bin/codex",
            );
            assert_eq!(cmd.as_deref(), Some("/opt/homebrew/bin/brew upgrade codex"));
        }

        #[test]
        fn codex_nvm_anchors_to_that_npm() {
            // Codex 不走 self-update（`codex update` 在 npm 安装上只是裸 `npm install -g`，
            // 却会假成功掩盖平台二进制漏装）——直接锚定到同一个 node 的 npm，而非 PATH
            // 第一个 npm。损坏时的 uninstall+install 自愈见 codex_missing_platform_binary_*。
            let cmd = anchored_command_from_paths(
                "codex",
                "/Users/me/.nvm/versions/node/v22.14.0/bin/codex",
                "/Users/me/.nvm/versions/node/v22.14.0/lib/node_modules/@openai/codex/bin/codex.js",
            );
            assert_eq!(
                cmd.as_deref(),
                Some("/Users/me/.nvm/versions/node/v22.14.0/bin/npm i -g @openai/codex@latest")
            );
        }

        #[test]
        fn codex_volta_anchors_to_volta_install() {
            // codex 锚定到命令行命中的那份 volta，但不 self-update：纯 `volta install`。
            let cmd = anchored_command_from_paths(
                "codex",
                "/Users/me/.volta/bin/codex",
                "/Users/me/.volta/tools/image/packages/codex/lib/node_modules/@openai/codex",
            );
            assert_eq!(
                cmd.as_deref(),
                Some("/Users/me/.volta/bin/volta install @openai/codex")
            );
        }

        #[test]
        fn volta_path_with_space_is_quoted() {
            // volta 分支用 `<dir>/volta`,目录含空格时同样要 POSIX 引号包裹。
            let cmd = anchored_command_from_paths(
                "codex",
                "/Users/my name/.volta/bin/codex",
                "/Users/my name/.volta/tools/image/packages/codex/lib/node_modules/@openai/codex",
            );
            assert_eq!(
                cmd.as_deref(),
                Some("'/Users/my name/.volta/bin/volta' install @openai/codex")
            );
        }

        #[test]
        fn fnm_install_anchors_to_that_npm() {
            // fnm 是自带同级 npm 的 node 管理器 → 锚定到那处的 npm。
            let cmd = anchored_command_from_paths(
                "codex",
                "/Users/me/.local/share/fnm_multishells/12345_abc/bin/codex",
                "/Users/me/.local/share/fnm_multishells/12345_abc/lib/node_modules/@openai/codex/bin/codex.js",
            );
            assert_eq!(
                cmd.as_deref(),
                Some(
                    "/Users/me/.local/share/fnm_multishells/12345_abc/bin/npm i -g @openai/codex@latest"
                )
            );
        }

        #[test]
        fn path_with_space_is_quoted() {
            let cmd = anchored_command_from_paths(
                "codex",
                "/Users/my name/.nvm/versions/node/v22/bin/codex",
                "/Users/my name/.nvm/versions/node/v22/lib/node_modules/@openai/codex/bin/codex.js",
            );
            assert_eq!(
                cmd.as_deref(),
                Some("'/Users/my name/.nvm/versions/node/v22/bin/npm' i -g @openai/codex@latest")
            );
        }

        #[test]
        fn claude_native_path_with_space_is_quoted() {
            // claude 分支同样要 POSIX 引号包裹含空格的 bin_path,
            // 否则 `/Users/my name/.local/bin/claude update` 会被 shell 拆词。
            let cmd = anchored_command_from_paths(
                "claude",
                "/Users/my name/.local/bin/claude",
                "/Users/my name/.local/share/claude/versions/2.1.146",
            );
            assert_eq!(
                cmd.as_deref(),
                Some("'/Users/my name/.local/bin/claude' update")
            );
        }

        #[test]
        fn brew_path_with_space_is_quoted() {
            // brew 分支用 `<bin_path 同目录>/brew`,目录含空格时同样要引号包裹。
            let cmd = anchored_command_from_paths(
                "codex",
                "/opt/my brew/bin/codex",
                "/opt/my brew/Cellar/codex/1.2.3/bin/codex",
            );
            assert_eq!(
                cmd.as_deref(),
                Some("'/opt/my brew/bin/brew' upgrade codex")
            );
        }

        #[test]
        fn brew_formula_extraction() {
            assert_eq!(
                brew_formula_from_path("/opt/homebrew/Cellar/codex/1.2.3/bin/codex").as_deref(),
                Some("codex")
            );
            // node 全局包不在 Cellar 下 → 不是 formula。
            assert_eq!(
                brew_formula_from_path("/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js"),
                None
            );
            assert_eq!(
                brew_formula_from_path("/Users/me/.nvm/versions/node/v22/lib/node_modules/x"),
                None
            );
        }

        #[test]
        fn sibling_bin_returns_none_when_bin_path_has_no_directory() {
            // bin_path 不含 `/` → parent_dir 返回空 → sibling_bin 不能拼出绝对路径
            // → None,让上游 anchored_command_from_paths 整体退化为静态命令兜底,
            // 而不是悄悄拼出 `npm i -g <pkg>` 这种依赖 PATH 的指令(违背"必须绝对路径"
            // 不变量)。实际从 enumerate_tool_installations 走的 bin_path 都是绝对路径,
            // 这条防线不期望被触发,但闭合了 helper 与函数文档的语义一致。
            assert_eq!(sibling_bin("codex", "npm"), None);
            assert_eq!(sibling_bin("", "brew"), None);
            // 含 `/` 即可拼出绝对路径——这是常规路径。
            assert_eq!(
                sibling_bin("/opt/homebrew/bin/codex", "brew").as_deref(),
                Some("/opt/homebrew/bin/brew")
            );
        }

        #[test]
        fn default_install_prefers_path_default() {
            let installs = vec![
                inst("/opt/homebrew/bin/codex", false),
                inst("/Users/me/.nvm/versions/node/v22/bin/codex", true),
            ];
            assert_eq!(
                default_install(&installs).map(|i| i.path.as_str()),
                Some("/Users/me/.nvm/versions/node/v22/bin/codex")
            );
        }

        #[test]
        fn default_install_falls_back_to_sole_entry() {
            let installs = vec![inst("/opt/homebrew/bin/codex", false)];
            assert_eq!(
                default_install(&installs).map(|i| i.path.as_str()),
                Some("/opt/homebrew/bin/codex")
            );
        }

        #[test]
        fn default_install_none_when_ambiguous() {
            let installs = vec![
                inst("/opt/homebrew/bin/codex", false),
                inst("/Users/me/.nvm/versions/node/v22/bin/codex", false),
            ];
            assert!(default_install(&installs).is_none());
        }

        #[test]
        fn codex_missing_platform_binary_self_heals_via_uninstall_install() {
            // 平台二进制缺失 → `codex --version` 报 "Missing optional dependency" 退出非 0
            // → enumerate 标记 runnable=false。此状态下普通 `npm i -g @latest` 是 no-op 修不好,
            // 升级路径改用 uninstall+install 重装补回平台二进制（`|| true` 让 uninstall 在
            // set -e 下对半损坏包返回非 0 时仍继续 install）。
            let mut broken = inst("/Users/me/.nvm/versions/node/v22.14.0/bin/codex", true);
            broken.runnable = false;
            assert_eq!(
                installs_anchored_command("codex", &[broken]).as_deref(),
                Some("/Users/me/.nvm/versions/node/v22.14.0/bin/npm uninstall -g @openai/codex || true; /Users/me/.nvm/versions/node/v22.14.0/bin/npm i -g @openai/codex@latest")
            );
        }

        #[test]
        fn codex_runnable_uses_plain_npm_not_self_heal() {
            // 正常（runnable=true）的 codex 升级：锚定 npm，既不重装、也不跑会假成功
            // 掩盖损坏的 `codex update`。
            let healthy = inst("/Users/me/.nvm/versions/node/v22.14.0/bin/codex", true);
            let cmd = installs_anchored_command("codex", &[healthy]);
            assert_eq!(
                cmd.as_deref(),
                Some("/Users/me/.nvm/versions/node/v22.14.0/bin/npm i -g @openai/codex@latest")
            );
            assert!(!cmd.unwrap().contains("uninstall"));
        }

        #[test]
        fn codex_broken_homebrew_formula_uses_brew_not_npm_repair() {
            // brew formula 装的坏 codex（real 在 Cellar）：自愈门控必须收窄放行，回落到
            // `brew upgrade codex`——若误走 npm 重装，npm 够不到 Cellar 那份、反而旁路
            // 装第二份 npm 全局 codex 制造双安装。
            let broken = ToolInstallation {
                path: "/opt/homebrew/bin/codex".to_string(),
                version: None,
                runnable: false,
                error: None,
                source: "homebrew".to_string(),
                is_path_default: true,
                real: std::path::PathBuf::from("/opt/homebrew/Cellar/codex/1.2.3/bin/codex"),
            };
            assert_eq!(
                installs_anchored_command("codex", &[broken]).as_deref(),
                Some("/opt/homebrew/bin/brew upgrade codex")
            );
        }

        #[test]
        fn codex_broken_volta_uses_volta_install_not_npm_repair() {
            // volta 装的坏 codex：回落到 `volta install`，不走 npm 重装。
            let mut broken = inst("/Users/me/.volta/bin/codex", true);
            broken.runnable = false;
            assert_eq!(
                installs_anchored_command("codex", &[broken]).as_deref(),
                Some("/Users/me/.volta/bin/volta install @openai/codex")
            );
        }

        #[test]
        fn codex_broken_bun_uses_bun_add_not_phantom_npm() {
            // bun 装的坏 codex：回落到 `bun add`，且**绝不**拼出 `~/.bun/bin/npm`
            // （bun 目录下没有 npm，那条路径不存在、执行会直接失败）。
            let mut broken = inst("/Users/me/.bun/bin/codex", true);
            broken.runnable = false;
            let cmd = installs_anchored_command("codex", &[broken]);
            assert_eq!(
                cmd.as_deref(),
                Some("/Users/me/.bun/bin/bun add -g @openai/codex@latest")
            );
            assert!(!cmd.unwrap().contains("npm"));
        }

        #[test]
        fn first_abs_path_line_skips_shell_noise() {
            // 交互式 .zshrc 先打印欢迎语（如 powerlevel10k / 自定义提示），
            // command -v 的真实路径在其后 → 跳过噪音取真路径。
            assert_eq!(
                first_abs_path_line("🚀 Welcome back!\n/Users/me/.local/bin/claude\n"),
                Some("/Users/me/.local/bin/claude")
            );
            // 无噪音时取第一行。
            assert_eq!(
                first_abs_path_line("/opt/homebrew/bin/codex\n"),
                Some("/opt/homebrew/bin/codex")
            );
            // 输出里没有任何绝对路径 → None。
            assert_eq!(first_abs_path_line("welcome\nbye\n"), None);
        }

        #[test]
        fn is_conflicting_thresholds() {
            let make = |version: Option<&str>, runnable: bool| ToolInstallation {
                path: "/x".to_string(),
                version: version.map(str::to_string),
                runnable,
                error: None,
                source: "nvm".to_string(),
                is_path_default: false,
                real: std::path::PathBuf::from("/x"),
            };
            // 单处 → 不冲突。
            assert!(!is_conflicting(&[make(Some("1.0.0"), true)]));
            // 两处同版本、都能跑 → 不冲突（同版本装两遍不打扰）。
            assert!(!is_conflicting(&[
                make(Some("1.0.0"), true),
                make(Some("1.0.0"), true)
            ]));
            // 版本分歧 → 冲突。
            assert!(is_conflicting(&[
                make(Some("1.0.0"), true),
                make(Some("2.0.0"), true)
            ]));
            // 同版本但运行态混合（一个能跑、一个跑不起来）→ 冲突。
            assert!(is_conflicting(&[
                make(Some("1.0.0"), true),
                make(Some("1.0.0"), false)
            ]));
        }
    }

    /// install 端的"上游推荐 || npm 兜底"短路链:把工具→官方安装方式这一上游事实
    /// 固化为回归断言。任何方案改动若打破短路链结构或 URL,都会被这些用例拦下。
    #[cfg(not(target_os = "windows"))]
    mod install_strategy {
        use super::super::*;

        #[test]
        fn claude_install_prefers_native_with_npm_fallback() {
            // Anthropic 现在主推 native installer(claude.ai/install.sh),
            // 网络不通时短路到 npm 仍能装上;两段都得在,顺序也得对。
            let cmd = install_command_for("claude");
            assert!(
                cmd.contains("https://claude.ai/install.sh"),
                "should include official installer URL: {cmd}"
            );
            assert!(
                cmd.contains("@anthropic-ai/claude-code@latest"),
                "should keep npm package as fallback: {cmd}"
            );
            let parts: Vec<&str> = cmd.split("||").collect();
            assert_eq!(parts.len(), 2, "should be a two-step short-circuit chain");
            assert!(parts[0].contains("install.sh"), "native first: {cmd}");
            assert!(
                !parts[0].contains('|'),
                "native installer should avoid pipe: {cmd}"
            );
            assert!(parts[1].contains("npm i -g"), "npm second: {cmd}");
        }

        #[test]
        fn codex_install_keeps_static_npm() {
            // OpenAI 暂无独立 native installer,保持原裸 npm,不引入兜底链(无东西可兜底)。
            let cmd = install_command_for("codex");
            assert_eq!(cmd, "npm i -g @openai/codex@latest");
            assert!(!cmd.contains("||"));
        }

        #[test]
        fn update_fallbacks_use_official_cli_only_when_supported() {
            assert_eq!(
                static_fallback_command("claude"),
                "claude update || npm i -g @anthropic-ai/claude-code@latest"
            );
            assert_eq!(
                static_fallback_command("codex"),
                "npm i -g @openai/codex@latest"
            );
            assert!(!static_fallback_command("codex").contains("codex update"));
        }
    }

    #[cfg(target_os = "windows")]
    mod wsl_helpers {
        use super::super::*;

        #[test]
        fn test_is_valid_shell() {
            assert!(is_valid_shell("bash"));
            assert!(is_valid_shell("zsh"));
            assert!(is_valid_shell("sh"));
            assert!(is_valid_shell("fish"));
            assert!(is_valid_shell("dash"));
            assert!(is_valid_shell("/usr/bin/bash"));
            assert!(is_valid_shell("/bin/zsh"));
            assert!(!is_valid_shell("powershell"));
            assert!(!is_valid_shell("cmd"));
            assert!(!is_valid_shell(""));
        }

        #[test]
        fn test_is_valid_shell_flag() {
            assert!(is_valid_shell_flag("-c"));
            assert!(is_valid_shell_flag("-lc"));
            assert!(is_valid_shell_flag("-lic"));
            assert!(!is_valid_shell_flag("-x"));
            assert!(!is_valid_shell_flag(""));
            assert!(!is_valid_shell_flag("--login"));
        }

        #[test]
        fn test_default_flag_for_shell() {
            assert_eq!(default_flag_for_shell("sh"), "-c");
            assert_eq!(default_flag_for_shell("dash"), "-c");
            assert_eq!(default_flag_for_shell("/bin/dash"), "-c");
            assert_eq!(default_flag_for_shell("fish"), "-lc");
            assert_eq!(default_flag_for_shell("bash"), "-lic");
            assert_eq!(default_flag_for_shell("zsh"), "-lic");
            assert_eq!(default_flag_for_shell("/usr/bin/zsh"), "-lic");
        }

        #[test]
        fn test_is_valid_wsl_distro_name() {
            assert!(is_valid_wsl_distro_name("Ubuntu"));
            assert!(is_valid_wsl_distro_name("Ubuntu-22.04"));
            assert!(is_valid_wsl_distro_name("my_distro"));
            assert!(!is_valid_wsl_distro_name(""));
            assert!(!is_valid_wsl_distro_name("distro with spaces"));
            assert!(!is_valid_wsl_distro_name(&"a".repeat(65)));
        }
    }

    #[test]
    fn cli_path_env_search_paths_include_path_entries_and_dedupe() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).expect("first dir should be created");
        std::fs::create_dir_all(&second).expect("second dir should be created");

        let path_env = std::env::join_paths([first.clone(), second.clone(), first.clone()])
            .expect("test path env should be joinable");
        let mut paths = vec![first.clone()];

        extend_from_cli_path_env(&mut paths, Some(path_env));

        assert!(paths.contains(&second));
        assert_eq!(paths.iter().filter(|path| *path == &first).count(), 1);
    }

    #[test]
    fn child_search_paths_include_existing_children_with_suffix() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let base = temp.path().join("node");
        let bin = base.join("25.8.0").join("bin");
        std::fs::create_dir_all(&bin).expect("version bin should be created");

        let mut paths = Vec::new();
        extend_existing_child_search_paths(&mut paths, &base, Some("bin"));

        assert!(paths.contains(&bin));
    }

    #[test]
    fn env_child_dir_appends_child_and_dedupes() {
        let base = std::ffi::OsString::from("/custom/toolchain");
        let mut paths = Vec::new();

        push_env_child_dir(&mut paths, Some(base.clone()), "bin");
        push_env_child_dir(&mut paths, Some(base), "bin");

        assert_eq!(paths, vec![PathBuf::from("/custom/toolchain").join("bin")]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn cli_path_env_skips_windows_apps_alias_dir() {
        assert!(is_windows_app_execution_alias_dir(Path::new(
            r"C:\Users\tester\AppData\Local\Microsoft\WindowsApps"
        )));
        assert!(!is_windows_app_execution_alias_dir(Path::new(
            r"C:\Users\tester\AppData\Roaming\npm"
        )));
    }

    #[test]
    fn mise_node_search_paths_include_shims_and_installed_node_bins() {
        let temp = tempfile::tempdir().expect("temp dir should be created");
        let home = temp.path();
        let node_bin = home
            .join(".local/share/mise/installs/node/25.8.0")
            .join("bin");
        std::fs::create_dir_all(&node_bin).expect("node bin should be created");

        let mut paths = Vec::new();
        extend_mise_node_search_paths(&mut paths, home);

        assert!(paths.contains(&home.join(".local/share/mise/shims")));
        assert!(paths.contains(&node_bin));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn tool_executable_candidates_non_windows_uses_plain_binary_name() {
        let dir = PathBuf::from("/usr/local/bin");
        let candidates = tool_executable_candidates("codex", &dir);

        assert_eq!(candidates, vec![PathBuf::from("/usr/local/bin/codex")]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn tool_executable_candidates_windows_includes_cmd_exe_and_plain_name() {
        let dir = PathBuf::from("C:\\tools");
        let candidates = tool_executable_candidates("codex", &dir);

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("C:\\tools\\codex.cmd"),
                PathBuf::from("C:\\tools\\codex.exe"),
                PathBuf::from("C:\\tools\\codex"),
            ]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn tool_executable_candidates_windows_skips_shadowed_npm_unix_shim() {
        let dir = tempfile::tempdir().expect("temp dir should be created");
        let extensionless = dir.path().join("codex");
        let cmd = dir.path().join("codex.cmd");
        std::fs::write(&extensionless, "").expect("extensionless shim should be created");
        std::fs::write(&cmd, "").expect("cmd shim should be created");

        let candidates = tool_executable_candidates("codex", dir.path());

        assert_eq!(candidates, vec![cmd.clone(), dir.path().join("codex.exe")]);
        assert!(!candidates.contains(&extensionless));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_runnable_sibling_prefers_cmd_over_extensionless_tool() {
        let dir = tempfile::tempdir().expect("temp dir should be created");
        let extensionless = dir.path().join("codex");
        let cmd = dir.path().join("codex.cmd");
        std::fs::write(&extensionless, "").expect("extensionless shim should be created");
        std::fs::write(&cmd, "").expect("cmd shim should be created");

        let preferred = windows_runnable_sibling_for_extensionless_tool(&extensionless);

        assert_eq!(preferred.as_deref(), Some(cmd.as_path()));
    }

    #[test]
    fn resolve_launch_cwd_accepts_existing_directory() {
        let resolved =
            resolve_launch_cwd(Some(std::env::temp_dir().to_string_lossy().into_owned()))
                .expect("temp dir should resolve")
                .expect("temp dir should be present");

        assert!(resolved.is_dir());
    }

    #[test]
    fn resolve_launch_cwd_rejects_missing_directory() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let missing = std::env::temp_dir().join(format!("cc-switch-missing-{unique}"));

        let error = resolve_launch_cwd(Some(missing.to_string_lossy().into_owned()))
            .expect_err("missing directory should fail");

        assert!(error.contains("目录不存在"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn iterm2_applescript_cold_start_avoids_current_window_before_one_exists() {
        let script = build_macos_iterm2_applescript(Path::new("/tmp/cc_switch_launcher.sh"));

        let cold_start_branch = script
            .split("else\n        activate")
            .nth(1)
            .expect("cold start branch should be present")
            .split("    end if\n    tell current session")
            .next()
            .expect("cold start branch should end before writing command");

        assert!(cold_start_branch.contains("repeat while (count of windows) = 0"));
        assert!(cold_start_branch.contains("create window with default profile"));
        assert!(!cold_start_branch.contains("tell current window"));
        assert!(!cold_start_branch.contains("create tab with default profile"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn iterm2_applescript_keeps_new_tab_behavior_for_existing_windows() {
        let script = build_macos_iterm2_applescript(Path::new("/tmp/cc_switch_launcher.sh"));

        let running_branch = script
            .split("if was_running then")
            .nth(1)
            .expect("already-running branch should be present")
            .split("else\n        activate")
            .next()
            .expect("already-running branch should end before cold start branch");

        assert!(running_branch.contains("if (count of windows) = 0 then"));
        assert!(running_branch.contains("create window with default profile"));
        assert!(running_branch.contains("create tab with default profile"));
    }

    /// Terminal `activate` creates a default empty window on cold start; `launch` does not.
    #[cfg(target_os = "macos")]
    #[test]
    fn terminal_applescript_cold_start_uses_launch_before_do_script() {
        let script = build_macos_terminal_applescript(Path::new("/tmp/cc_switch_launcher.sh"));

        assert!(
            script.contains(r#"set was_running to application "Terminal" is running"#),
            "missing was_running detection:\n{script}"
        );
        // Cold launches avoid `activate` until after `do script`, so no default empty window is created first.
        assert!(
            script.contains(
                "else\n        launch\n        do script launcher_script\n        activate"
            ),
            "cold start should launch before activating:\n{script}"
        );
        // Already-running launches should create a fresh session.
        assert!(
            script.contains(
                "if was_running then\n        activate\n        do script launcher_script\n"
            ),
            "already-running branch should use bare do script:\n{script}"
        );
        assert!(
            script.contains(r#"set launcher_script to "exec sh '/tmp/cc_switch_launcher.sh'""#),
            "Terminal should replace the auto-created shell:\n{script}"
        );
    }

    /// Restored windows should not receive the launcher command.
    #[cfg(target_os = "macos")]
    #[test]
    fn terminal_applescript_does_not_hijack_restored_windows() {
        let script = build_macos_terminal_applescript(Path::new("/tmp/cc_switch_launcher.sh"));
        assert!(
            !script.contains(" in window 1"),
            "should not inject into an existing/restored Terminal window:\n{script}"
        );
        assert!(
            !script.contains("count of windows"),
            "should not infer restored-window safety from window count:\n{script}"
        );
    }

    /// Ghostty cold starts use `initial-command`; warm starts use the scripting dictionary.
    #[cfg(target_os = "macos")]
    #[test]
    fn ghostty_applescript_cold_start_uses_initial_command() {
        let script = build_macos_ghostty_applescript(Path::new("/tmp/cc_switch_launcher.sh"));

        // Warm launches execute through the AppleScript command property, not `open -na ... -e`.
        assert!(
            script.contains(r#"set launcher_command to "sh '/tmp/cc_switch_launcher.sh'""#),
            "missing launcher_command:\n{script}"
        );
        assert!(script.contains("if was_running then"));
        assert!(script.contains("new window with configuration {command:launcher_command}"));
        assert!(
            !script.contains(" --args -e"),
            "should not execute through open -na -e:\n{script}"
        );
        // Cold launches make Ghostty's first default surface execute the launcher.
        assert!(script.contains(r#"set was_running to application "Ghostty" is running"#));
        assert!(
            script.contains(
                r#"do shell script "open -na Ghostty --args --quit-after-last-window-closed=true " & quoted form of ("--initial-command=" & launcher_command)"#
            ),
            "cold start should use initial-command:\n{script}"
        );
        assert!(
            !script.contains("--initial-window=false"),
            "should not rely on initial-window=false:\n{script}"
        );
        assert!(
            !script.contains("delay 0.5"),
            "should not rely on a fixed delay:\n{script}"
        );
        assert!(
            !script.contains("old_ids"),
            "should not track default windows for closing:\n{script}"
        );
        assert!(
            !script.contains("close window"),
            "should not close a default window:\n{script}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dash_c_command_wraps_script_path_inside_quoted_arg() {
        // The script path must stay inside the `-c` string, not as a bare argv.
        let s = build_macos_dash_c_command(Path::new("/tmp/cc_switch_launcher_1.sh"));
        assert_eq!(s, "exec sh '/tmp/cc_switch_launcher_1.sh'");

        // Spaces and single quotes must stay shell-safe too.
        let s2 = build_macos_dash_c_command(Path::new("/Users/me/it's dir/x.sh"));
        assert_eq!(s2, r#"exec sh '/Users/me/it'"'"'s dir/x.sh'"#);
    }

    /// AppleScript launchers need both shell-path quoting and AppleScript string quoting.
    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_builders_safely_quote_special_paths() {
        // First shell-quote the path, then wrap the whole command as an AppleScript string.
        let expected = r#""sh '/Users/me/it'\"'\"'s dir/x.sh'""#;
        let p = Path::new("/Users/me/it's dir/x.sh");
        assert_eq!(applescript_launcher_command(p), expected);
        assert_eq!(
            applescript_exec_launcher_command(p),
            r#""exec sh '/Users/me/it'\"'\"'s dir/x.sh'""#
        );
        assert!(
            build_macos_terminal_applescript(p)
                .contains(r#""exec sh '/Users/me/it'\"'\"'s dir/x.sh'""#),
            "Terminal did not quote safely"
        );
        assert!(
            build_macos_iterm2_applescript(p)
                .contains(r#""exec sh '/Users/me/it'\"'\"'s dir/x.sh'""#),
            "iTerm2 did not quote safely"
        );
        assert!(
            build_macos_ghostty_applescript(p).contains(expected),
            "Ghostty did not keep the non-exec launcher"
        );
    }

    #[test]
    fn build_windows_cwd_command_str_uses_cd_for_drive_paths() {
        let command = build_windows_cwd_command_str(r"C:\work\repo");

        assert_eq!(command, "cd /d \"C:\\work\\repo\" || exit /b 1\r\n");
    }

    #[test]
    fn build_windows_cwd_command_str_uses_pushd_for_unc_paths() {
        let command = build_windows_cwd_command_str(r"\\wsl$\Ubuntu\home\coder\repo");

        assert_eq!(
            command,
            "pushd \"\\\\wsl$\\Ubuntu\\home\\coder\\repo\" || exit /b 1\r\n"
        );
    }

    #[test]
    fn build_windows_cwd_command_str_escapes_batch_metacharacters() {
        let command = build_windows_cwd_command_str(r"\\server\share\100%&(test)");

        assert_eq!(
            command,
            "pushd \"\\\\server\\share\\100%%^&^(test^)\" || exit /b 1\r\n"
        );
    }
