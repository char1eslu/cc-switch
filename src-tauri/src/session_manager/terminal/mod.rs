use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn launch_command(target: &str, command: &str, cwd: Option<&str>) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("Terminal command is empty".to_string());
    }

    let script_file = write_launcher_script(command, cwd)?;
    let result = launch_target(target, &script_file);

    if result.is_err() {
        let _ = std::fs::remove_file(&script_file);
    }
    result
}

fn write_launcher_script(command: &str, cwd: Option<&str>) -> Result<PathBuf, String> {
    let mut script = tempfile::Builder::new()
        .prefix("cc_switch_launcher_")
        .suffix(".sh")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempfile()
        .map_err(|e| format!("Failed to create terminal launcher: {e}"))?;

    let cd = cwd
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("cd {} || exit 1; ", shell_quote(value)))
        .unwrap_or_default();
    let shell = user_shell();
    let invocation = format!(
        "exec {} -lic {}",
        shell_quote(&shell),
        shell_quote(&format!("{cd}{command}"))
    );

    write!(script, "#!/usr/bin/env sh\nrm -- \"$0\"\n{invocation}\n")
        .map_err(|e| format!("Failed to write terminal launcher: {e}"))?;
    script
        .flush()
        .map_err(|e| format!("Failed to flush terminal launcher: {e}"))?;

    script
        .into_temp_path()
        .keep()
        .map_err(|e| format!("Failed to preserve terminal launcher: {e}"))
}

fn launch_target(target: &str, script_file: &Path) -> Result<(), String> {
    let target = canonical_target(target);
    let result = match target {
        "terminal" => launch_terminal_app(script_file),
        "iterm2" => launch_iterm2(script_file),
        "ghostty" => launch_ghostty(script_file),
        "kitty" => launch_open_app("kitty", script_file, false),
        "wezterm" => launch_open_app("WezTerm", script_file, true),
        "kaku" => launch_open_app("Kaku", script_file, true),
        "alacritty" => launch_open_app("Alacritty", script_file, true),
        "warp" => launch_warp(script_file),
        _ => Err(format!("Unsupported terminal target: {target}")),
    };

    if result.is_err() && target != "terminal" {
        log::warn!(
            "Preferred terminal {target} failed, falling back to Terminal.app: {:?}",
            result.as_ref().err()
        );
        return launch_terminal_app(script_file);
    }

    result
}

fn canonical_target(target: &str) -> &str {
    match target {
        "iterm" | "iTerm" => "iterm2",
        other => other,
    }
}

fn applescript_string_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn launcher_command_literal(script_file: &Path, replace_shell: bool) -> String {
    let prefix = if replace_shell { "exec sh" } else { "sh" };
    applescript_string_literal(&format!(
        "{prefix} {}",
        shell_quote(&script_file.to_string_lossy())
    ))
}

fn build_terminal_applescript(script_file: &Path) -> String {
    format!(
        r#"set launcher_script to {launcher}
set was_running to application "Terminal" is running
tell application "Terminal"
    if was_running then
        activate
        do script launcher_script
    else
        launch
        do script launcher_script
        activate
    end if
end tell"#,
        launcher = launcher_command_literal(script_file, true)
    )
}

fn build_iterm2_applescript(script_file: &Path) -> String {
    format!(
        r#"set launcher_script to {launcher}
set was_running to application "iTerm" is running
tell application "iTerm"
    if was_running then
        activate
        if (count of windows) = 0 then
            create window with default profile
        else
            tell current window
                create tab with default profile
            end tell
        end if
    else
        activate
        set waited to 0
        repeat while (count of windows) = 0
            delay 0.1
            set waited to waited + 1
            if waited >= 30 then exit repeat
        end repeat
        if (count of windows) = 0 then
            create window with default profile
        end if
    end if
    tell current session of current window
        write text launcher_script
    end tell
end tell"#,
        launcher = launcher_command_literal(script_file, true)
    )
}

fn build_ghostty_applescript(script_file: &Path) -> String {
    format!(
        r#"set launcher_command to {launcher}
set was_running to application "Ghostty" is running
if was_running then
    tell application "Ghostty"
        new window with configuration {{command:launcher_command}}
    end tell
else
    do shell script "open -na Ghostty --args --quit-after-last-window-closed=true " & quoted form of ("--initial-command=" & launcher_command)
end if
"#,
        launcher = launcher_command_literal(script_file, false)
    )
}

fn run_osascript(applescript: &str, label: &str) -> Result<(), String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(applescript)
        .output()
        .map_err(|e| format!("Failed to launch {label}: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed (exit code: {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn launch_terminal_app(script_file: &Path) -> Result<(), String> {
    run_osascript(&build_terminal_applescript(script_file), "Terminal.app")
}

fn launch_iterm2(script_file: &Path) -> Result<(), String> {
    run_osascript(&build_iterm2_applescript(script_file), "iTerm2")
}

fn launch_ghostty(script_file: &Path) -> Result<(), String> {
    match run_osascript(&build_ghostty_applescript(script_file), "Ghostty") {
        Ok(()) => Ok(()),
        Err(error) => {
            log::warn!("Ghostty AppleScript launch failed, falling back to open -na: {error}");
            launch_open_app("Ghostty", script_file, true)
        }
    }
}

fn dash_c_command(script_file: &Path) -> String {
    format!("exec sh {}", shell_quote(&script_file.to_string_lossy()))
}

fn launch_open_app(app_name: &str, script_file: &Path, use_e_flag: bool) -> Result<(), String> {
    let mut command = Command::new("open");
    command.arg("-na").arg(app_name).arg("--args");
    if use_e_flag {
        command.arg("-e");
    }
    command.arg("sh").arg("-c").arg(dash_c_command(script_file));

    let output = command
        .output()
        .map_err(|e| format!("Failed to launch {app_name}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{app_name} failed (exit code: {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn launch_warp(script_file: &Path) -> Result<(), String> {
    let mut wrapper = tempfile::Builder::new()
        .prefix("cc_switch_warp_")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempfile()
        .map_err(|e| format!("Failed to create Warp launcher: {e}"))?;

    writeln!(
        wrapper,
        "#!/usr/bin/env sh\nrm -- \"$0\"\nexec sh {}",
        shell_quote(&script_file.to_string_lossy())
    )
    .map_err(|e| format!("Failed to write Warp launcher: {e}"))?;
    wrapper
        .flush()
        .map_err(|e| format!("Failed to flush Warp launcher: {e}"))?;

    let wrapper_path = wrapper
        .into_temp_path()
        .keep()
        .map_err(|e| format!("Failed to preserve Warp launcher: {e}"))?;
    let mut warp_url = url::Url::parse("warp://action/new_tab")
        .map_err(|e| format!("Failed to create Warp URL: {e}"))?;
    warp_url
        .query_pairs_mut()
        .append_pair("path", &wrapper_path.to_string_lossy());

    let output = Command::new("open")
        .arg("-a")
        .arg("Warp")
        .arg(warp_url.as_str())
        .output()
        .map_err(|e| format!("Failed to launch Warp: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(wrapper_path);
        Err(format!(
            "Warp failed (exit code: {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|value| {
            value.starts_with('/')
                && !value.chars().any(char::is_control)
                && Path::new(value).is_file()
        })
        .unwrap_or_else(|| "/bin/zsh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_script_quotes_cwd_and_removes_itself() {
        let script = write_launcher_script("claude --resume abc", Some("/tmp/it's $(safe)"))
            .expect("launcher should be created");
        let content = std::fs::read_to_string(&script).expect("launcher should be readable");

        assert!(content.contains("rm -- \"$0\""));
        assert!(content.contains("$(safe)"));
        assert!(content.contains("claude --resume abc"));
        assert!(content.contains(" -lic "));
        std::fs::remove_file(script).expect("launcher should be removed");
    }

    #[test]
    fn terminal_cold_start_launches_before_activation() {
        let script = build_terminal_applescript(Path::new("/tmp/launcher.sh"));
        assert!(script
            .contains("else\n        launch\n        do script launcher_script\n        activate"));
        assert!(!script.contains(" in window 1"));
    }

    #[test]
    fn iterm2_cold_start_waits_for_a_window() {
        let script = build_iterm2_applescript(Path::new("/tmp/launcher.sh"));
        assert!(script.contains("repeat while (count of windows) = 0"));
        assert!(script.contains("create tab with default profile"));
    }

    #[test]
    fn legacy_iterm_names_remain_supported() {
        assert_eq!(canonical_target("iterm"), "iterm2");
        assert_eq!(canonical_target("iTerm"), "iterm2");
    }

    #[test]
    fn ghostty_cold_start_uses_initial_command() {
        let script = build_ghostty_applescript(Path::new("/tmp/launcher.sh"));
        assert!(script.contains("--initial-command="));
        assert!(!script.contains("--initial-window=false"));
    }

    #[test]
    fn command_literals_quote_special_paths_across_shell_and_applescript() {
        let path = Path::new("/Users/me/it's dir/launcher.sh");
        assert_eq!(
            dash_c_command(path),
            r#"exec sh '/Users/me/it'"'"'s dir/launcher.sh'"#
        );
        assert_eq!(
            launcher_command_literal(path, true),
            r#""exec sh '/Users/me/it'\"'\"'s dir/launcher.sh'""#
        );
    }
}
