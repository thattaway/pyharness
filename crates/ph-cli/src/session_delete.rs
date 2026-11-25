//! `pyharness session delete <file>`: delete a saved session through the
//! background service. The service refuses a running session and keeps its
//! own records (the roster, sub-agent usage) in step with the removal, so a
//! session file is never deleted behind its back. The service is started
//! when none is running.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ph_types::daemon::DaemonCommand;
use serde_json::Value;

use crate::daemon_client::DaemonClient;

const USAGE: &str = "Usage: pyharness session delete <file> [--json]";

/// Run the command and return the process exit code.
pub(crate) fn run_session_delete(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error:#}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let (path, json) = parse_args(args)?;
    let cwd = std::env::current_dir().context("read the current directory")?;
    let path = absolute(&cwd, &path);
    if !path.is_file() {
        bail!("no session file at {}", path.display());
    }
    let socket = crate::config::resolve_daemon_socket_path(None);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start the runtime")?;
    runtime.block_on(crate::interactive_mode::ensure_daemon_running(&socket, &cwd))?;
    let mut client = DaemonClient::connect(&socket)?;
    let response = client.request(DaemonCommand::DeleteSavedSession {
        id: None,
        active_session_id: None,
        session_path: path.to_string_lossy().to_string(),
        rest: serde_json::Map::new(),
    })?;
    if !response.success {
        bail!("{}", response.error.unwrap_or_default());
    }
    let data = response.data.unwrap_or(Value::Null);
    if data.get("ok").and_then(Value::as_bool) != Some(true) {
        let reason = data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the session was not deleted");
        bail!("{reason}");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        println!("Deleted {}", path.display());
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<(PathBuf, bool)> {
    let mut json = false;
    let mut path = None;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown option {other}\n{USAGE}"),
            other if path.is_none() => path = Some(PathBuf::from(other)),
            _ => bail!("{USAGE}"),
        }
    }
    Ok((path.ok_or_else(|| anyhow!("{USAGE}"))?, json))
}

fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn arguments_take_one_file_and_an_optional_json_flag() {
        assert_eq!(
            parse_args(&args(&["a.jsonl", "--json"])).unwrap(),
            (PathBuf::from("a.jsonl"), true)
        );
        assert_eq!(
            parse_args(&args(&["a.jsonl"])).unwrap(),
            (PathBuf::from("a.jsonl"), false)
        );
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["a.jsonl", "b.jsonl"])).is_err());
        assert!(parse_args(&args(&["--force", "a.jsonl"])).is_err());
    }

    #[test]
    fn a_missing_file_is_refused_before_anything_starts() {
        let dir = tempfile::tempdir().unwrap();
        let error = run(&args(&[dir.path().join("gone.jsonl").to_str().unwrap()])).unwrap_err();
        assert!(error.to_string().starts_with("no session file at"));
    }
}
