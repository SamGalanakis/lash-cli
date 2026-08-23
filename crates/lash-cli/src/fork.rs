use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::session_log::SessionLogger;
use anyhow::{Context, Result, anyhow};

fn find_program_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_launchable_file(candidate))
}

fn is_launchable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}
pub fn resolve_resume_executable() -> Result<PathBuf> {
    if std::env::var_os("LASH_DEV_LAUNCH_CWD").is_some()
        && let Some(exe) = find_program_on_path("lash")
    {
        return Ok(exe);
    }

    match std::env::current_exe() {
        Ok(exe) if is_launchable_file(&exe) => Ok(exe),
        Ok(exe) => find_program_on_path("lash").ok_or_else(|| {
            anyhow!(
                "current executable `{}` is not launchable and `lash` was not found on PATH",
                exe.display()
            )
        }),
        Err(err) => find_program_on_path("lash")
            .ok_or_else(|| anyhow!("launcher lookup failed: {err}; `lash` was not found on PATH")),
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DetectedTerminal {
    Ghostty,
    Kitty,
    WezTerm,
    Alacritty,
    AppleTerminal,
    ITerm2,
}

#[cfg(any(target_os = "macos", test))]
fn detect_terminal_from_hints(
    term_program: Option<&str>,
    has_kitty: bool,
    has_wezterm: bool,
    has_iterm: bool,
) -> Option<DetectedTerminal> {
    match term_program {
        Some("ghostty") => Some(DetectedTerminal::Ghostty),
        Some("kitty") => Some(DetectedTerminal::Kitty),
        Some("WezTerm") | Some("wezterm") => Some(DetectedTerminal::WezTerm),
        Some("alacritty") | Some("Alacritty") => Some(DetectedTerminal::Alacritty),
        Some("Apple_Terminal") => Some(DetectedTerminal::AppleTerminal),
        Some("iTerm.app") => Some(DetectedTerminal::ITerm2),
        _ if has_kitty => Some(DetectedTerminal::Kitty),
        _ if has_wezterm => Some(DetectedTerminal::WezTerm),
        _ if has_iterm => Some(DetectedTerminal::ITerm2),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn detect_current_terminal() -> Option<DetectedTerminal> {
    let term_program = std::env::var("TERM_PROGRAM").ok();
    detect_terminal_from_hints(
        term_program.as_deref(),
        std::env::var_os("KITTY_PID").is_some(),
        std::env::var_os("WEZTERM_PANE").is_some(),
        std::env::var_os("ITERM_SESSION_ID").is_some(),
    )
}

fn spawn_command(mut cmd: std::process::Command) -> bool {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().is_ok()
}

#[cfg(target_os = "macos")]
fn run_command(mut cmd: std::process::Command) -> std::result::Result<(), String> {
    cmd.stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    match cmd.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit status {}", output.status)
            };
            Err(detail)
        }
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_with_launchers(exe: &Path, args: &[String], launchers: Vec<(&str, Vec<&str>)>) -> bool {
    for (launcher, prefix) in launchers {
        let Some(program) = find_program_on_path(launcher) else {
            continue;
        };
        let mut cmd = std::process::Command::new(program);
        cmd.args(prefix);
        cmd.arg(exe);
        cmd.args(args);
        if spawn_command(cmd) {
            return true;
        }
    }
    false
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_in_new_terminal_platform(exe: &Path, args: &[String]) -> Result<()> {
    let mut launchers: Vec<(&str, Vec<&str>)> = Vec::new();
    if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
        match term_program.as_str() {
            "ghostty" => launchers.push(("ghostty", vec!["-e"])),
            "kitty" => launchers.push(("kitty", vec![])),
            "WezTerm" | "wezterm" => {
                launchers.push(("wezterm", vec!["start", "--always-new-process", "--"]))
            }
            _ => {}
        }
    }
    launchers.extend([
        ("ghostty", vec!["-e"]),
        ("kitty", vec![]),
        ("wezterm", vec!["start", "--always-new-process", "--"]),
        ("alacritty", vec!["-e"]),
        ("foot", vec![]),
        ("gnome-terminal", vec!["--"]),
        ("konsole", vec!["-e"]),
        ("xterm", vec!["-e"]),
    ]);
    if spawn_with_launchers(exe, args, launchers) {
        return Ok(());
    }
    Err(anyhow!(
        "Could not find a supported terminal launcher (tried ghostty, kitty, wezterm, alacritty, foot, gnome-terminal, konsole, xterm)"
    ))
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(target_os = "macos")]
fn applescript_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('\"', "\\\""))
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacLauncher {
    GhosttyAppleScript,
    GhosttyCli,
    KittyCli,
    WezTermCli,
    AlacrittyCli,
    TerminalAppAppleScript,
    ITerm2AppleScript,
}

#[cfg(target_os = "macos")]
fn macos_command_line(exe: &Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_quote(&exe.display().to_string()));
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

#[cfg(target_os = "macos")]
fn spawn_with_single_launcher(
    exe: &Path,
    args: &[String],
    launcher: &str,
    prefix: &[&str],
) -> std::result::Result<(), String> {
    let Some(program) = find_program_on_path(launcher) else {
        return Err(format!("`{launcher}` is not available"));
    };
    let mut cmd = std::process::Command::new(program);
    cmd.args(prefix);
    cmd.arg(exe);
    cmd.args(args);
    if spawn_command(cmd) {
        Ok(())
    } else {
        Err(format!("failed to launch `{launcher}`"))
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos_ghostty(exe: &Path, args: &[String]) -> std::result::Result<(), String> {
    let Some(program) = find_program_on_path("osascript") else {
        return Err("`osascript` is not available".to_string());
    };
    let command_line = macos_command_line(exe, args);

    let mut cmd = std::process::Command::new(program);
    cmd.arg("-e")
        .arg("tell application \"Ghostty\"")
        .arg("-e")
        .arg("activate")
        .arg("-e")
        .arg("set cfg to new surface configuration")
        .arg("-e")
        .arg(format!(
            "set command of cfg to {}",
            applescript_quote(&command_line)
        ))
        .arg("-e")
        .arg("set win to new window with configuration cfg")
        .arg("-e")
        .arg("activate window win")
        .arg("-e")
        .arg("end tell");
    run_command(cmd)
}

#[cfg(target_os = "macos")]
fn spawn_macos_iterm2(exe: &Path, args: &[String]) -> std::result::Result<(), String> {
    let Some(program) = find_program_on_path("osascript") else {
        return Err("`osascript` is not available".to_string());
    };
    let command_line = macos_command_line(exe, args);

    let mut cmd = std::process::Command::new(program);
    cmd.arg("-e")
        .arg("tell application \"iTerm2\"")
        .arg("-e")
        .arg("activate")
        .arg("-e")
        .arg(format!(
            "create window with default profile command {}",
            applescript_quote(&command_line)
        ))
        .arg("-e")
        .arg("end tell");
    run_command(cmd)
}

#[cfg(target_os = "macos")]
fn spawn_macos_terminal_app(exe: &Path, args: &[String]) -> std::result::Result<(), String> {
    let Some(program) = find_program_on_path("osascript") else {
        return Err("`osascript` is not available".to_string());
    };
    let command_line = macos_command_line(exe, args);

    let mut cmd = std::process::Command::new(program);
    cmd.arg("-e")
        .arg("tell application \"Terminal\"")
        .arg("-e")
        .arg("activate")
        .arg("-e")
        .arg(format!("do script {}", applescript_quote(&command_line)))
        .arg("-e")
        .arg("end tell");
    run_command(cmd)
}

#[cfg(target_os = "macos")]
fn push_unique_launcher(launchers: &mut Vec<MacLauncher>, launcher: MacLauncher) {
    if !launchers.contains(&launcher) {
        launchers.push(launcher);
    }
}

#[cfg(target_os = "macos")]
fn macos_launcher_label(launcher: MacLauncher) -> &'static str {
    match launcher {
        MacLauncher::GhosttyAppleScript => "Ghostty AppleScript",
        MacLauncher::GhosttyCli => "ghostty",
        MacLauncher::KittyCli => "kitty",
        MacLauncher::WezTermCli => "wezterm",
        MacLauncher::AlacrittyCli => "alacritty",
        MacLauncher::TerminalAppAppleScript => "Terminal.app AppleScript",
        MacLauncher::ITerm2AppleScript => "iTerm2 AppleScript",
    }
}

#[cfg(target_os = "macos")]
fn macos_detected_terminal_label(terminal: DetectedTerminal) -> &'static str {
    match terminal {
        DetectedTerminal::Ghostty => "Ghostty",
        DetectedTerminal::Kitty => "kitty",
        DetectedTerminal::WezTerm => "WezTerm",
        DetectedTerminal::Alacritty => "Alacritty",
        DetectedTerminal::AppleTerminal => "Terminal.app",
        DetectedTerminal::ITerm2 => "iTerm2",
    }
}

#[cfg(target_os = "macos")]
fn macos_launcher_order(current: Option<DetectedTerminal>) -> Vec<MacLauncher> {
    let mut launchers = Vec::new();
    if let Some(current) = current {
        match current {
            DetectedTerminal::Ghostty => {
                push_unique_launcher(&mut launchers, MacLauncher::GhosttyAppleScript);
                push_unique_launcher(&mut launchers, MacLauncher::GhosttyCli);
            }
            DetectedTerminal::Kitty => push_unique_launcher(&mut launchers, MacLauncher::KittyCli),
            DetectedTerminal::WezTerm => {
                push_unique_launcher(&mut launchers, MacLauncher::WezTermCli)
            }
            DetectedTerminal::Alacritty => {
                push_unique_launcher(&mut launchers, MacLauncher::AlacrittyCli)
            }
            DetectedTerminal::AppleTerminal => {
                push_unique_launcher(&mut launchers, MacLauncher::TerminalAppAppleScript)
            }
            DetectedTerminal::ITerm2 => {
                push_unique_launcher(&mut launchers, MacLauncher::ITerm2AppleScript)
            }
        }
    }

    for launcher in [
        MacLauncher::GhosttyAppleScript,
        MacLauncher::GhosttyCli,
        MacLauncher::KittyCli,
        MacLauncher::WezTermCli,
        MacLauncher::TerminalAppAppleScript,
    ] {
        push_unique_launcher(&mut launchers, launcher);
    }

    launchers
}

#[cfg(target_os = "macos")]
fn spawn_macos_launcher(
    launcher: MacLauncher,
    exe: &Path,
    args: &[String],
) -> std::result::Result<(), String> {
    match launcher {
        MacLauncher::GhosttyAppleScript => spawn_macos_ghostty(exe, args),
        MacLauncher::GhosttyCli => spawn_with_single_launcher(exe, args, "ghostty", &["-e"]),
        MacLauncher::KittyCli => spawn_with_single_launcher(exe, args, "kitty", &[]),
        MacLauncher::WezTermCli => spawn_with_single_launcher(
            exe,
            args,
            "wezterm",
            &["start", "--always-new-process", "--"],
        ),
        MacLauncher::AlacrittyCli => spawn_with_single_launcher(exe, args, "alacritty", &["-e"]),
        MacLauncher::TerminalAppAppleScript => spawn_macos_terminal_app(exe, args),
        MacLauncher::ITerm2AppleScript => spawn_macos_iterm2(exe, args),
    }
}

#[cfg(target_os = "macos")]
fn spawn_in_new_terminal_platform(exe: &Path, args: &[String]) -> Result<()> {
    let current_terminal = detect_current_terminal();
    let mut errors = Vec::new();

    for launcher in macos_launcher_order(current_terminal) {
        match spawn_macos_launcher(launcher, exe, args) {
            Ok(()) => return Ok(()),
            Err(err) => errors.push(format!(
                "{} failed: {}",
                macos_launcher_label(launcher),
                err
            )),
        }
    }

    let detected = current_terminal
        .map(|terminal| {
            format!(
                "Detected current terminal: {}. ",
                macos_detected_terminal_label(terminal)
            )
        })
        .unwrap_or_default();
    Err(anyhow!(
        concat!(
            "Could not launch a supported macOS terminal. ",
            "{}{} ",
            "If macOS Automation blocks terminal scripting, allow the launcher to control the terminal app in System Settings."
        ),
        detected,
        errors.join(" ")
    ))
}

#[cfg(target_os = "windows")]
fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(target_os = "windows")]
fn spawn_windows_terminal(exe: &Path, args: &[String]) -> bool {
    let mut cmd = std::process::Command::new("wt.exe");
    cmd.arg("new-window");
    cmd.arg(exe);
    cmd.args(args);
    spawn_command(cmd)
}

#[cfg(target_os = "windows")]
fn spawn_windows_powershell(exe: &Path, args: &[String]) -> bool {
    let exe = powershell_quote(&exe.display().to_string());
    let script = if args.is_empty() {
        format!("Start-Process -FilePath {exe}")
    } else {
        let arg_list = args
            .iter()
            .map(|arg| powershell_quote(arg))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Start-Process -FilePath {exe} -ArgumentList {arg_list}")
    };

    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.arg("-NoProfile").arg("-Command").arg(script);
    spawn_command(cmd)
}

#[cfg(target_os = "windows")]
fn spawn_windows_cmd(exe: &Path, args: &[String]) -> bool {
    let mut cmd = std::process::Command::new("cmd.exe");
    cmd.arg("/C").arg("start").arg("").arg(exe);
    cmd.args(args);
    spawn_command(cmd)
}

#[cfg(target_os = "windows")]
fn spawn_in_new_terminal_platform(exe: &Path, args: &[String]) -> Result<()> {
    if spawn_windows_terminal(exe, args)
        || spawn_windows_powershell(exe, args)
        || spawn_windows_cmd(exe, args)
    {
        return Ok(());
    }
    Err(anyhow!(
        "Could not launch a supported Windows terminal (tried wt.exe, PowerShell Start-Process, cmd.exe start)"
    ))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn spawn_in_new_terminal_platform(_exe: &Path, _args: &[String]) -> Result<()> {
    Err(anyhow!(
        "Opening a new terminal is not supported on this platform"
    ))
}

pub fn spawn_in_new_terminal(exe: &Path, args: &[String]) -> Result<()> {
    spawn_in_new_terminal_platform(exe, args)
}

pub struct ForkedSession {
    pub session_id: String,
    pub session_name: String,
}

pub async fn fork_current_session(
    session: Option<&lash::LashSession>,
    opener: &crate::startup::session::CliSessionOpener,
    logger: &SessionLogger,
    configured_model: &str,
) -> Result<ForkedSession> {
    let session = session.ok_or_else(|| anyhow!("No active session to fork"))?;
    let node_id = session
        .read_view()
        .session_graph()
        .leaf_node_id
        .clone()
        .ok_or_else(|| anyhow!("The session has no committed node to fork"))?;
    let child_session_id = uuid::Uuid::new_v4().to_string();
    opener
        .fork_at(&node_id, &child_session_id)
        .await
        .context("fork session through Lash")?;
    let child_session_name =
        crate::generate_session_name(&crate::session_log::sessions_dir()).await;
    crate::session_log::save_roster_entry(&crate::session_log::RosterEntry {
        session_id: child_session_id.clone(),
        session_name: child_session_name.clone(),
        model: configured_model.to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        cwd: std::env::current_dir().ok(),
        parent_session_id: Some(logger.session_id.clone()),
        message_count: session
            .read_view()
            .messages()
            .iter()
            .filter(|message| message.role == lash_core::MessageRole::User)
            .count(),
        first_message: String::new(),
        inputs: Vec::new(),
    })?;
    if let Ok(config) = crate::session_log::load_host_config(&logger.session_id) {
        crate::session_log::save_host_config(&child_session_id, &config)?;
    }
    Ok(ForkedSession {
        session_id: child_session_id,
        session_name: child_session_name,
    })
}

#[cfg(test)]
mod tests {
    use super::{DetectedTerminal, detect_terminal_from_hints};

    #[test]
    fn detect_terminal_prefers_term_program() {
        assert_eq!(
            detect_terminal_from_hints(Some("ghostty"), true, true, true),
            Some(DetectedTerminal::Ghostty)
        );
        assert_eq!(
            detect_terminal_from_hints(Some("Apple_Terminal"), false, false, false),
            Some(DetectedTerminal::AppleTerminal)
        );
        assert_eq!(
            detect_terminal_from_hints(Some("iTerm.app"), false, false, false),
            Some(DetectedTerminal::ITerm2)
        );
    }

    #[test]
    fn detect_terminal_falls_back_to_terminal_specific_envs() {
        assert_eq!(
            detect_terminal_from_hints(None, true, false, false),
            Some(DetectedTerminal::Kitty)
        );
        assert_eq!(
            detect_terminal_from_hints(None, false, true, false),
            Some(DetectedTerminal::WezTerm)
        );
        assert_eq!(
            detect_terminal_from_hints(None, false, false, true),
            Some(DetectedTerminal::ITerm2)
        );
        assert_eq!(detect_terminal_from_hints(None, false, false, false), None);
    }
}
