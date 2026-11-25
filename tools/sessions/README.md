# pyharness sessions

A full-screen session manager for **pyharness** and **Claude Code** sessions:
browse every project on your machine, preview conversations, see token usage
and cost, resume, export and clean up — from one place.

It is a fork of [ccsessions](https://github.com/miskiewiczm/ccsessions) by
Marek Miśkiewicz (MIT). See `UPSTREAM.md` for the imported version.

## Run it

```bash
pyharness sessions --ui
```

It needs [uv](https://docs.astral.sh/uv/) (pyharness already does, for its
Python kernel); the first run installs the app's one dependency,
[Textual](https://textual.textualize.io/). To run it without pyharness, from
this folder:

```bash
uv run ccsessions
```

## What it shows

- **Projects** — one row per working directory, newest first. pyharness
  projects are tagged `· pyharness`; a green dot marks a project with a running
  session.
- **Sessions** — the project's sessions with message counts, tokens and last
  change. Names set with `/name` (pyharness) or `/rename` (Claude Code) are
  used as titles.
- **Details** — ids, folder, dates, token breakdown, and cost when the session
  records it (pyharness does).
- **Conversation** — the end of the conversation: your prompts, replies, tool
  calls (⚙) and commands (⌘).

## Keys

| Key | Action |
| --- | --- |
| `j` / `k` / arrows | Move within the focused pane |
| `Tab` / `Shift+Tab` | Switch panes |
| `r` | Resume the selected session in its folder |
| `c` | Copy the resume command to the clipboard |
| `a` | Archive ↔ restore (Claude Code sessions only) |
| `d` | Delete the session, or every session in the project (with confirmation) |
| `n` | Rename the project (a display alias only) |
| `e` | Export the session to Markdown, or copy the raw transcript |
| `l` | Keep a live Markdown copy of the session next to the project |
| `/` | Filter the focused list |
| `Ctrl+R` | Rescan now |
| `q` | Quit |

## How it treats each kind of session

| | pyharness | Claude Code |
| --- | --- | --- |
| Where it looks | `~/.pyharness/sessions/` (or `PYHARNESS_SESSION_DIR`, `PYHARNESS_CODING_AGENT_DIR`) | `~/.claude/projects/` |
| Running sessions | asks `pyharness list` | Claude Code's process markers |
| Resume | `pyharness --resume <session file>` | `claude --resume <id>` |
| Delete | `pyharness session delete`, so pyharness keeps its records in step; a running session is refused | removes the transcript and its index entry |
| Archive | not yet | moves the transcript aside, reversibly |

Sub-agent sessions are listed with their parent in pyharness itself and are
not shown here. Running sessions are never deleted or archived: the check is
repeated at the moment you confirm.

## Settings

Display aliases, the theme and other preferences live in
`~/.config/ccsessions/`; the token cache lives in `~/.cache/ccsessions/`.
Nothing under `~/.pyharness` or `~/.claude` is changed except by resume,
delete and archive.

## Tests

```bash
uv run --with pytest python -m pytest
```
