"""pyharness session support: the transcript format, discovery and actions."""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from ccsessions.core.parser import compute_token_stats, read_conversation_tail

PH_SID = "01a211c6-7d29-76b6-b14d-5897de0810b4"


def write_ph_session(path: Path, cwd: str, sid: str = PH_SID, depth: int = 0,
                     name: str | None = None) -> Path:
    """A pyharness session file shaped like a real one (header, bookkeeping
    entries, a user turn, a reply with a tool call, a tool result, a reply)."""
    usage = {"input": 100, "output": 20, "cacheRead": 500, "cacheWrite": 10,
             "totalTokens": 630, "cost": {"total": 0.0125}}
    lines = [
        {"type": "session", "version": 3, "id": sid, "timestamp": "2026-11-25T08:15:50.569Z",
         "cwd": cwd, "rlmDepth": depth},
        {"type": "model_change", "provider": "faux", "modelId": "faux-1", "id": "e1",
         "parentId": None, "timestamp": "2026-11-25T08:15:50.662Z"},
        {"type": "custom_message", "customType": "harness_digest", "content": "[harness-digest] …",
         "display": False, "id": "e2", "parentId": "e1", "timestamp": "2026-11-25T08:15:50.670Z"},
        {"type": "message", "id": "e3", "parentId": "e2", "timestamp": "2026-11-25T08:15:50.683Z",
         "message": {"role": "user", "content": [{"type": "text", "text": "Name a river: zażółć"}],
                     "timestamp": 1795594550676}},
        {"type": "message", "id": "e4", "parentId": "e3", "timestamp": "2026-11-25T08:15:51.000Z",
         "message": {"role": "assistant", "usage": usage, "content": [
             {"type": "thinking", "thinking": "hmm"},
             {"type": "text", "text": "Let me check."},
             {"type": "toolCall", "id": "t1", "name": "bash", "arguments": {"command": "ls -la"}}]}},
        {"type": "message", "id": "e5", "parentId": "e4", "timestamp": "2026-11-25T08:15:52.000Z",
         "message": {"role": "toolResult", "toolCallId": "t1", "content": [{"type": "text", "text": "x"}]}},
        {"type": "message", "id": "e6", "parentId": "e5", "timestamp": "2026-11-25T08:15:53.000Z",
         "message": {"role": "assistant", "usage": usage,
                     "content": [{"type": "text", "text": "The **Nile**."}]}},
    ]
    if name is not None:
        lines.append({"type": "session_info", "name": name, "id": "e7", "parentId": "e6",
                      "timestamp": "2026-11-25T08:16:00.000Z"})
    path.write_text("\n".join(json.dumps(x, ensure_ascii=False) for x in lines) + "\n",
                    encoding="utf-8")
    return path


def test_pyharness_preview_shows_turns_and_tool_calls(tmp_path):
    f = write_ph_session(tmp_path / f"{PH_SID}.jsonl", "/work/alpha")
    entries = read_conversation_tail(f)
    assert [(e.role, e.text) for e in entries] == [
        ("user", "Name a river: zażółć"),
        ("assistant", "Let me check."),
        ("tool", "bash: ls -la"),
        ("assistant", "The **Nile**."),
    ]
    assert entries[0].ts is not None


def test_pyharness_token_stats_cost_and_titles(tmp_path):
    f = write_ph_session(tmp_path / f"{PH_SID}.jsonl", "/work/alpha", name="River quiz")
    s = compute_token_stats(f)
    assert (s.input, s.output, s.cache_read, s.cache_write) == (200, 40, 1000, 20)
    assert s.cost == pytest.approx(0.025)
    assert s.messages == 3  # one prompt, two replies; the tool result is not a message
    assert s.first_prompt == "Name a river: zażółć"
    assert s.custom_title == "River quiz"


from ccsessions.core import pyharness  # noqa: E402
from ccsessions.core.resume import ResumeRequest, build_resume_command  # noqa: E402


@pytest.fixture
def fake_pyharness(tmp_path, monkeypatch):
    """A stand-in `pyharness` that answers `list --all --json` from a file and
    records every call, so actions are checked without the real binary."""
    calls = tmp_path / "calls.txt"
    listing = tmp_path / "listing.json"
    listing.write_text(json.dumps({"sessions": []}))
    script = tmp_path / "pyharness"
    script.write_text(
        "#!/bin/sh\n"
        f'echo "$@" >> "{calls}"\n'
        f'if [ "$1" = list ]; then cat "{listing}"; fi\n'
        'if [ "$1" = session ] && [ -n "$FAIL_DELETE" ]; then echo "Error: refused" >&2; exit 1; fi\n'
    )
    script.chmod(0o755)
    monkeypatch.setenv(pyharness.BIN_ENV, str(script))
    return {"calls": calls, "listing": listing}


def test_sessions_are_grouped_by_working_directory(tmp_path):
    base = tmp_path / "sessions"
    base.mkdir()
    write_ph_session(base / "a.jsonl", "/work/alpha", sid="a")
    write_ph_session(base / "b.jsonl", "/work/alpha", sid="b", name="Second")
    write_ph_session(base / "c.jsonl", "/work/beta", sid="c")
    write_ph_session(base / "child.jsonl", "/work/alpha", sid="child", depth=1)
    (base / "not-a-session.jsonl").write_text("{}\n")
    projects = pyharness.discover_pyharness_projects(base=base, running=set())
    by_path = {p.project_path: p for p in projects}
    assert set(by_path) == {"/work/alpha", "/work/beta"}
    assert sorted(s.session_id for s in by_path["/work/alpha"].sessions) == ["a", "b"]
    assert all(p.source == "pyharness" and not p.encoded_dir.exists() for p in projects)
    assert {s.display_title for s in by_path["/work/alpha"].sessions} == {
        "Name a river: zażółć", "Second"}


def test_running_sessions_come_from_pyharness_list(tmp_path, fake_pyharness):
    base = tmp_path / "sessions"
    base.mkdir()
    running = write_ph_session(base / "a.jsonl", "/work/alpha", sid="a")
    write_ph_session(base / "b.jsonl", "/work/alpha", sid="b")
    fake_pyharness["listing"].write_text(json.dumps({"sessions": [
        {"sessionFile": str(running), "workerPid": 4242},
        {"sessionFile": str(base / "b.jsonl")},  # saved, not running
    ]}))
    sessions = pyharness.discover_pyharness_projects(base=base)[0].sessions
    assert {s.session_id: s.is_live for s in sessions} == {"a": True, "b": False}


def test_no_background_service_means_nothing_is_running(monkeypatch, tmp_path):
    script = tmp_path / "pyharness"
    script.write_text("#!/bin/sh\necho 'Error: Failed to connect' >&2\nexit 1\n")
    script.chmod(0o755)
    monkeypatch.setenv(pyharness.BIN_ENV, str(script))
    assert pyharness.running_session_files() == set()


def test_delete_goes_through_pyharness(tmp_path, fake_pyharness, monkeypatch):
    base = tmp_path / "sessions"
    base.mkdir()
    path = write_ph_session(base / "a.jsonl", "/work/alpha", sid="a")
    session = pyharness.discover_pyharness_projects(base=base, running=set())[0].sessions[0]
    pyharness.delete_session(session)
    assert fake_pyharness["calls"].read_text().strip() == f"session delete {path}"
    assert path.exists()  # the file is pyharness's to remove, never ours
    monkeypatch.setenv("FAIL_DELETE", "1")
    with pytest.raises(pyharness.PyharnessError, match="refused"):
        pyharness.delete_session(session)


def test_pyharness_resume_command_uses_the_session_file(tmp_path):
    cmd = build_resume_command(ResumeRequest(
        session_id="a", cwd=str(tmp_path), source="pyharness",
        session_file="/home/me/.pyharness/sessions/a.jsonl"))
    assert cmd == f"cd {tmp_path} && pyharness --resume /home/me/.pyharness/sessions/a.jsonl"


def test_sessions_dir_follows_the_pyharness_environment(monkeypatch, tmp_path):
    monkeypatch.delenv(pyharness.SESSION_DIR_ENV, raising=False)
    monkeypatch.setenv(pyharness.AGENT_DIR_ENV, str(tmp_path / "state"))
    assert pyharness.sessions_dir() == tmp_path / "state" / "sessions"
    monkeypatch.setenv(pyharness.SESSION_DIR_ENV, str(tmp_path / "elsewhere"))
    assert pyharness.sessions_dir() == tmp_path / "elsewhere"
