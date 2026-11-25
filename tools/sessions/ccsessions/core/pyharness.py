"""pyharness sessions: where they live, which are running, and the actions
that have to go through pyharness itself.

pyharness keeps every session in one flat folder, `<agent dir>/sessions/`,
one `<id>.jsonl` per session whose first line is a header carrying the
session's working directory. Projects are therefore groups of sessions that
share a working directory, not directories on disk.

Deleting a pyharness session also updates the background service's records
(the roster, sub-agent usage), so it is done by `pyharness session delete`,
never by removing the file here. Which sessions are running comes from
`pyharness list --all --json`; when no background service runs, none are.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
from datetime import datetime
from pathlib import Path

from .cache import TokenCache
from .models import Project, Session
from .parser import get_token_stats

AGENT_DIR_ENV = "PYHARNESS_CODING_AGENT_DIR"
SESSION_DIR_ENV = "PYHARNESS_SESSION_DIR"
# Set by `pyharness sessions --ui`: the binary that launched this app.
BIN_ENV = "PYHARNESS_BIN"
SOURCE = "pyharness"
LIST_TIMEOUT = 5.0
DELETE_TIMEOUT = 30.0


class PyharnessError(RuntimeError):
    pass


def _expand(value: str) -> Path:
    return Path(value).expanduser()


def agent_dir() -> Path:
    override = os.environ.get(AGENT_DIR_ENV, "").strip()
    return _expand(override) if override else Path.home() / ".pyharness"


def sessions_dir() -> Path:
    override = os.environ.get(SESSION_DIR_ENV, "").strip()
    return _expand(override) if override else agent_dir() / "sessions"


def pyharness_bin() -> str | None:
    """The pyharness executable: the one that launched us, else the one on PATH."""
    launched_by = os.environ.get(BIN_ENV, "").strip()
    if launched_by and os.access(launched_by, os.X_OK):
        return launched_by
    return shutil.which("pyharness")


def read_header(path: Path) -> dict | None:
    """The session header (first line), or None when the file is not a session."""
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            first = f.readline()
    except OSError:
        return None
    try:
        header = json.loads(first)
    except json.JSONDecodeError:
        return None
    if not isinstance(header, dict) or header.get("type") != "session":
        return None
    if not isinstance(header.get("id"), str) or not isinstance(header.get("cwd"), str):
        return None
    return header


def running_session_files(binary: str | None = None) -> set[str]:
    """Resolved paths of the session files a pyharness worker is running now.

    Empty when pyharness is not installed or its background service is not
    running (then nothing can be running either).
    """
    binary = binary or pyharness_bin()
    if not binary:
        return set()
    try:
        result = subprocess.run(
            [binary, "list", "--all", "--json"],
            capture_output=True,
            text=True,
            timeout=LIST_TIMEOUT,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return set()
    if result.returncode != 0:
        return set()
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError:
        return set()
    rows = data.get("sessions") if isinstance(data, dict) else None
    running: set[str] = set()
    for row in rows if isinstance(rows, list) else []:
        if not isinstance(row, dict) or not row.get("workerPid"):
            continue
        session_file = row.get("sessionFile")
        if isinstance(session_file, str) and session_file:
            running.add(_resolved(Path(session_file)))
    return running


def _resolved(path: Path) -> str:
    try:
        return str(path.resolve())
    except OSError:
        return str(path)


def _parse_iso(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return dt.astimezone().replace(tzinfo=None) if dt.tzinfo else dt


def project_key(cwd: str) -> str:
    """A stable, file-name-safe key for a project, used for display aliases."""
    return "pyharness-" + hashlib.sha256(cwd.encode()).hexdigest()[:12]


def discover_pyharness_projects(
    base: Path | None = None,
    cache: TokenCache | None = None,
    running: set[str] | None = None,
) -> list[Project]:
    """pyharness projects, newest first.

    Sub-agent sessions (rlmDepth > 0) belong to their parent session and are
    left out, as pyharness's own session list does by default.
    """
    base = base or sessions_dir()
    if not base.is_dir():
        return []
    if running is None:
        running = running_session_files()
    by_cwd: dict[str, list[Session]] = {}
    for path in base.glob("*.jsonl"):
        header = read_header(path)
        if header is None or int(header.get("rlmDepth") or 0) > 0:
            continue
        tokens = get_token_stats(path, cache)
        try:
            modified = datetime.fromtimestamp(path.stat().st_mtime)
        except OSError:
            modified = None
        git = header.get("git") if isinstance(header.get("git"), dict) else {}
        cwd = header["cwd"]
        by_cwd.setdefault(cwd, []).append(
            Session(
                session_id=header["id"],
                jsonl_path=path,
                project_path=cwd,
                first_prompt=tokens.first_prompt,
                summary=tokens.custom_title,
                message_count=tokens.messages,
                created=_parse_iso(header.get("timestamp")),
                modified=modified,
                git_branch=str(git.get("branch") or ""),
                is_live=_resolved(path) in running,
                tokens=tokens,
                source=SOURCE,
            )
        )
    projects = []
    for cwd, sessions in by_cwd.items():
        sessions.sort(key=lambda s: s.modified or datetime.min, reverse=True)
        projects.append(
            Project(
                project_path=cwd,
                # never a real directory: pyharness projects are groups of
                # sessions, and nothing ever moves or deletes this path
                encoded_dir=base / project_key(cwd),
                sessions=sessions,
                source=SOURCE,
            )
        )
    projects.sort(key=lambda p: p.sessions[0].modified or datetime.min, reverse=True)
    return projects


def delete_session(session: Session, binary: str | None = None) -> None:
    """Delete a saved pyharness session through pyharness itself."""
    binary = binary or pyharness_bin()
    if not binary:
        raise PyharnessError("`pyharness` not found in PATH")
    try:
        result = subprocess.run(
            [binary, "session", "delete", str(session.jsonl_path)],
            capture_output=True,
            text=True,
            timeout=DELETE_TIMEOUT,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as e:
        raise PyharnessError(str(e)) from e
    if result.returncode != 0:
        message = (result.stderr or result.stdout).strip().splitlines()
        raise PyharnessError(message[-1] if message else f"exit code {result.returncode}")
