//! `/sessions`: the terminal handoff to the full-screen session manager
//! (`pyharness sessions --ui`, the app in `tools/sessions/`). The manager runs
//! in pick mode: resuming a pyharness session there writes the session file to
//! a temp file and exits, and the chat then switches to it like
//! `/resume <file>`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Tells the manager where to write the picked session (and to pick instead
/// of resuming in place).
pub(crate) const PICK_FILE_ENV: &str = "PYHARNESS_SESSIONS_PICK_FILE";

/// Run the manager and return the session file the user picked, if any.
/// The caller hands the terminal over before and takes it back after.
pub(crate) async fn pick() -> Result<Option<PathBuf>> {
    let exe = own_executable()?;
    let pick_file = std::env::temp_dir().join(format!(
        "pyharness-sessions-pick-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or_default()
    ));
    // create_new + 0600: never follow or reuse a planted path in the shared
    // temp dir; the manager overwrites this empty file with its pick.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(&pick_file)
        .with_context(|| format!("create {}", pick_file.display()))?;
    let outcome = run_manager(&exe, &pick_file).await;
    let _ = std::fs::remove_file(&pick_file);
    outcome
}

async fn run_manager(exe: &Path, pick_file: &Path) -> Result<Option<PathBuf>> {
    let status = tokio::process::Command::new(exe)
        .args(["sessions", "--ui"])
        .env(PICK_FILE_ENV, pick_file)
        .status()
        .await
        .context("start the session manager")?;
    if !status.success() {
        return Err(anyhow!(
            "the session manager exited with {status}; run `pyharness sessions --ui` in a \
             terminal to see why"
        ));
    }
    let text = std::fs::read_to_string(pick_file)
        .with_context(|| format!("read the pick from {}", pick_file.display()))?;
    parse_pick(&text)
}

/// The manager's pick: `{"sessionFile": "<path>"}`, or nothing when the user
/// quit without picking.
fn parse_pick(text: &str) -> Result<Option<PathBuf>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_str(text).context("the session manager wrote an unreadable pick")?;
    let file = value
        .get("sessionFile")
        .and_then(serde_json::Value::as_str)
        .filter(|file| !file.is_empty())
        .ok_or_else(|| anyhow!("the session manager's pick names no session file"))?;
    Ok(Some(PathBuf::from(file)))
}

/// This binary, still reachable after a rebuild replaced the file on disk
/// (Linux keeps the running image at `/proc/self/exe`).
fn own_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("find the pyharness executable")?;
    if exe.exists() {
        return Ok(exe);
    }
    let running = Path::new("/proc/self/exe");
    Ok(if running.exists() {
        running.to_path_buf()
    } else {
        exe
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pick_is_a_session_file_or_nothing() {
        assert_eq!(parse_pick("").unwrap(), None);
        assert_eq!(parse_pick("  \n").unwrap(), None);
        assert_eq!(
            parse_pick(r#"{"sessionFile": "/s/a.jsonl"}"#).unwrap(),
            Some(PathBuf::from("/s/a.jsonl"))
        );
        assert!(parse_pick(r#"{"sessionFile": ""}"#).is_err());
        assert!(parse_pick("not json").is_err());
    }
}
