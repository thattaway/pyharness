from __future__ import annotations

from ccsessions.core.resume import ResumeRequest, build_resume_command


def test_command_includes_cd_for_existing_directory(tmp_path):
    proj = tmp_path / "my project"  # space forces quoting
    proj.mkdir()
    cmd = build_resume_command(ResumeRequest(session_id="abc-123", cwd=str(proj)))
    assert cmd == f"cd '{proj}' && claude --resume abc-123"


def test_command_skips_cd_for_missing_directory(tmp_path):
    cmd = build_resume_command(
        ResumeRequest(session_id="abc-123", cwd=str(tmp_path / "gone"))
    )
    assert cmd == "claude --resume abc-123"


def test_resume_refuses_a_missing_project_folder(tmp_path, monkeypatch):
    import pytest

    from ccsessions.core.resume import ResumeError, ResumeRequest, execute_resume

    monkeypatch.setattr("shutil.which", lambda _name: "/bin/true")
    with pytest.raises(ResumeError, match="no longer exists"):
        execute_resume(ResumeRequest(session_id="x", cwd=str(tmp_path / "gone")))
