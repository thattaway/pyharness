from __future__ import annotations

import os
import shlex
import shutil
from dataclasses import dataclass

from .pyharness import pyharness_bin


@dataclass
class ResumeRequest:
    session_id: str
    cwd: str
    source: str = "claude"  # "claude" (Claude Code) or "pyharness"
    session_file: str = ""  # pyharness resumes by file


def _argv(req: ResumeRequest) -> list[str]:
    """The command that resumes the session (program name first)."""
    if req.source == "pyharness":
        return ["pyharness", "--resume", req.session_file]
    return ["claude", "--resume", req.session_id]


class ResumeError(RuntimeError):
    pass


def build_resume_command(req: ResumeRequest) -> str:
    """Shell command that resumes the session — for copy/paste into a terminal."""
    cmd = shlex.join(_argv(req))
    if req.cwd and os.path.isdir(req.cwd):
        cmd = f"cd {shlex.quote(req.cwd)} && {cmd}"
    return cmd


def execute_resume(req: ResumeRequest) -> None:
    """Replace this process with the agent's resume command, in the session's folder.

    Claude Code resumes by session id, pyharness by session file. Does not
    return on success (os.execv replaces the process image).
    """
    argv = _argv(req)
    program = pyharness_bin() if req.source == "pyharness" else shutil.which(argv[0])
    if not program:
        raise ResumeError(f"`{argv[0]}` not found in PATH")
    # Claude Code finds a session through the working directory, so resuming
    # anywhere else fails with "no conversation found"
    if req.cwd and not os.path.isdir(req.cwd):
        raise ResumeError(f"the session's project folder no longer exists: {req.cwd}")
    if req.cwd:
        os.chdir(req.cwd)
    os.execv(program, argv)
