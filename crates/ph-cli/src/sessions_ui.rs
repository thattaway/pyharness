//! `pyharness sessions --ui`: open the full-screen session manager in
//! `tools/sessions/` (a Python app, forked from ccsessions) for pyharness and
//! Claude Code sessions. It runs through `uv`, which pyharness already needs
//! for its Python kernel, and learns which pyharness binary launched it so its
//! resume and delete actions use the same build.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Overrides where the app lives (by default `tools/sessions` in the source
/// tree this binary was built from).
const ENV_APP_DIR: &str = "PYHARNESS_SESSIONS_UI_DIR";
/// Tells the app which pyharness binary launched it.
const ENV_BIN: &str = "PYHARNESS_BIN";

/// Run the session manager and return the process exit code.
pub(crate) fn run_sessions_ui() -> i32 {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error:#}");
            1
        }
    }
}

fn run() -> Result<i32> {
    let app_dir = app_dir();
    if !app_dir.join("pyproject.toml").is_file() {
        bail!(
            "the session manager is not available: {} has no pyproject.toml. \
             It ships in the pyharness source tree (tools/sessions); set {ENV_APP_DIR} \
             if it lives elsewhere",
            app_dir.display()
        );
    }
    let uv = find_uv().context(
        "the session manager needs uv: install it with \
         `curl -LsSf https://astral.sh/uv/install.sh | sh`",
    )?;
    let mut command = Command::new(uv);
    command
        .args(["run", "--quiet", "--project"])
        .arg(&app_dir)
        .arg("ccsessions");
    if let Ok(exe) = std::env::current_exe() {
        command.env(ENV_BIN, exe);
    }
    exec(command)
}

fn app_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_APP_DIR).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    // crates/ph-cli -> the workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .join("tools")
        .join("sessions")
}

/// `uv` on PATH, else the standalone installer's `~/.local/bin/uv`.
fn find_uv() -> Option<PathBuf> {
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let local = ph_types::platform::home_dir()?
        .join(".local")
        .join("bin")
        .join(name);
    local.is_file().then_some(local)
}

/// Hand the terminal to the app: replace this process on Unix, so the app's
/// own resume (which replaces itself with pyharness) leaves nothing behind.
#[cfg(unix)]
fn exec(mut command: Command) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    Err(error).context("start the session manager")
}

#[cfg(not(unix))]
fn exec(mut command: Command) -> Result<i32> {
    let status = command.status().context("start the session manager")?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_ships_in_this_source_tree() {
        if std::env::var_os(ENV_APP_DIR).is_none() {
            assert!(app_dir().join("pyproject.toml").is_file());
        }
    }
}
