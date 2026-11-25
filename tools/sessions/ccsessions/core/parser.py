from __future__ import annotations

import json
import os
import re
from collections.abc import Iterator
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING

from .models import Entry, TokenStats

if TYPE_CHECKING:
    from .cache import TokenCache


def extract_cwd(jsonl_path: Path, max_lines: int = 20) -> str:
    """Read the first few events looking for a `cwd` field. Empty string if none."""
    try:
        with open(jsonl_path, encoding="utf-8", errors="replace") as f:
            for i, line in enumerate(f):
                if i >= max_lines:
                    break
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(obj, dict):
                    cwd = obj.get("cwd")
                    if isinstance(cwd, str) and cwd:
                        return cwd
    except OSError:
        pass
    return ""


def _prompt_text(content: object) -> str:
    """Text the user typed, or "" for tool results and slash-command wrappers."""
    if isinstance(content, list):
        content = " ".join(
            c.get("text", "") for c in content if isinstance(c, dict) and c.get("type") == "text"
        )
    if not isinstance(content, str) or content.lstrip().startswith("<"):
        return ""
    return content.strip()


def _count(value: object) -> int:
    """A token count from a usage field; anything that is not a number counts as 0."""
    if isinstance(value, bool):
        return 0
    if isinstance(value, (int, float)):
        return int(value)
    return 0


def _is_tool_result(content: object) -> bool:
    return isinstance(content, list) and any(
        isinstance(c, dict) and c.get("type") == "tool_result" for c in content
    )


def _add_pyharness_message(stats: TokenStats, msg: dict) -> None:
    """Fold one pyharness `message` entry into the stats.

    pyharness writes each message on a single line, so there is nothing to
    de-duplicate; usage keys are camelCase and the cost is recorded too.
    """
    role = msg.get("role")
    if role == "user":
        stats.messages += 1
        if not stats.first_prompt:
            stats.first_prompt = _prompt_text(msg.get("content"))
    elif role == "assistant":
        stats.messages += 1
        usage = msg.get("usage")
        if isinstance(usage, dict):
            stats.input += _count(usage.get("input"))
            stats.output += _count(usage.get("output"))
            stats.cache_read += _count(usage.get("cacheRead"))
            stats.cache_write += _count(usage.get("cacheWrite"))
            cost = usage.get("cost")
            total = cost.get("total") if isinstance(cost, dict) else None
            if isinstance(total, (int, float)) and not isinstance(total, bool):
                stats.cost += float(total)


def compute_token_stats(jsonl_path: Path) -> TokenStats:
    """Token usage, message count and titles for one transcript.

    Claude Code writes a single reply as several lines, one per content block
    (thinking, text, tool call), and every one of them repeats the reply's
    `usage`. Usage is therefore taken once per reply (keyed by `message.id`,
    the last line wins), and the reply counts as one message. Tool results are
    stored as user lines but are not messages anyone typed, so they don't count.
    """
    stats = TokenStats()
    usage_by_reply: dict[str, dict] = {}
    replies: set[str] = set()
    try:
        with open(jsonl_path, encoding="utf-8", errors="replace") as f:
            for number, line in enumerate(f):
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(obj, dict):
                    continue
                if obj.get("type") == "custom-title":
                    title = obj.get("customTitle")
                    if isinstance(title, str):
                        stats.custom_title = title.strip()
                    continue
                if obj.get("type") == "ai-title":
                    title = obj.get("aiTitle")
                    if isinstance(title, str):
                        stats.ai_title = title.strip()
                    continue
                if obj.get("type") == "session_info":  # pyharness: a name set with /name
                    name = obj.get("name")
                    if isinstance(name, str) and name.strip():
                        stats.custom_title = name.strip()
                    continue
                msg = obj.get("message")
                if not isinstance(msg, dict) or obj.get("isMeta"):
                    continue
                if obj.get("type") == "message":  # pyharness: one line per message
                    _add_pyharness_message(stats, msg)
                    continue
                if obj.get("type") == "user":
                    content = msg.get("content")
                    if _is_tool_result(content):
                        continue
                    stats.messages += 1
                    if not stats.first_prompt:
                        stats.first_prompt = _prompt_text(content)
                elif obj.get("type") == "assistant":
                    reply_id = msg.get("id")
                    key = reply_id if isinstance(reply_id, str) and reply_id else f"line:{number}"
                    if key not in replies:
                        replies.add(key)
                        stats.messages += 1
                    usage = msg.get("usage")
                    if isinstance(usage, dict):
                        usage_by_reply[key] = usage
    except OSError:
        pass
    for usage in usage_by_reply.values():
        stats.input += _count(usage.get("input_tokens"))
        stats.output += _count(usage.get("output_tokens"))
        stats.cache_read += _count(usage.get("cache_read_input_tokens"))
        stats.cache_write += _count(usage.get("cache_creation_input_tokens"))
    return stats


def get_token_stats(jsonl_path: Path, cache: "TokenCache | None" = None) -> TokenStats:
    """Like compute_token_stats, but skips the read when the cache is fresh."""
    if cache is None:
        return compute_token_stats(jsonl_path)
    try:
        stat = jsonl_path.stat()
    except OSError:
        return TokenStats()
    cached = cache.get(jsonl_path, stat)
    if cached is not None:
        return cached
    stats = compute_token_stats(jsonl_path)
    cache.put(jsonl_path, stats, stat)
    return stats


def _summarize_tool_use(name: str, tool_input: object) -> str:
    if not isinstance(tool_input, dict):
        return name
    for key in ("command", "description", "file_path", "path", "pattern", "prompt", "query", "url", "skill", "code"):
        val = tool_input.get(key)
        if isinstance(val, str) and val.strip():
            first = val.strip().splitlines()[0]
            if len(first) > 80:
                first = first[:77] + "..."
            return f"{name}: {first}"
    return name


_CMD_NAME = re.compile(r"<command-name>(.*?)</command-name>", re.S)
_CMD_ARGS = re.compile(r"<command-args>(.*?)</command-args>", re.S)
_CMD_STDOUT = re.compile(r"<local-command-stdout>(.*?)</local-command-stdout>", re.S)


def _classify_user_text(text: str) -> tuple[str, str] | None:
    """Turn raw slash-command records into readable entries.

    Returns (role, text) or None when the entry should be skipped.
    """
    m = _CMD_NAME.search(text)
    if m:
        label = m.group(1).strip()
        args = _CMD_ARGS.search(text)
        if args and args.group(1).strip():
            label += " " + args.group(1).strip()
        return ("command", label) if label else None
    m = _CMD_STDOUT.search(text)
    if m:
        out = m.group(1).strip()
        return ("command-output", out) if out else None
    return ("user", text)


def _parse_ts(value: object) -> "datetime | None":
    """Parse the ISO-8601 timestamp Claude Code writes on every record."""
    if not isinstance(value, str) or not value:
        return None
    try:
        # fromisoformat only learned to read the trailing "Z" in 3.11
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _text_blocks(content: object) -> str:
    if isinstance(content, str):
        return content.strip()
    if isinstance(content, list):
        return "\n".join(
            b["text"].strip()
            for b in content
            if isinstance(b, dict)
            and b.get("type") == "text"
            and isinstance(b.get("text"), str)
            and b["text"].strip()
        )
    return ""


def _pyharness_entries(obj: dict) -> list[Entry]:
    """Entries from one pyharness `message` line.

    Roles: `user`, `assistant` (text, thinking and `toolCall` blocks),
    `toolResult` (skipped, as Claude Code's tool results are) and
    `bashExecution` (a `!` command, shown like a slash command). Summaries and
    other bookkeeping roles are not part of the conversation shown here.
    """
    msg = obj.get("message")
    if not isinstance(msg, dict):
        return []
    ts = _parse_ts(obj.get("timestamp"))
    role = msg.get("role")
    if role == "user":
        text = _text_blocks(msg.get("content"))
        return [Entry("user", text, ts)] if text else []
    if role == "bashExecution":
        command = msg.get("command")
        return [Entry("command", f"! {command}", ts)] if isinstance(command, str) else []
    if role != "assistant" or not isinstance(msg.get("content"), list):
        return []
    entries: list[Entry] = []
    for b in msg["content"]:
        if not isinstance(b, dict):
            continue
        if b.get("type") == "text" and isinstance(b.get("text"), str) and b["text"].strip():
            entries.append(Entry("assistant", b["text"].strip(), ts))
        elif b.get("type") == "toolCall":
            name = str(b.get("name") or "?")
            entries.append(Entry("tool", _summarize_tool_use(name, b.get("arguments")), ts))
    return entries


def entries_from_line(line: str) -> list[Entry]:
    """Parse one JSONL record into zero or more entries."""
    line = line.strip()
    if not line:
        return []
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return []
    if not isinstance(obj, dict) or obj.get("isMeta"):
        return []
    if obj.get("type") == "message":
        return _pyharness_entries(obj)
    typ = obj.get("type")
    msg = obj.get("message")
    if typ not in ("user", "assistant") or not isinstance(msg, dict):
        return []
    ts = _parse_ts(obj.get("timestamp"))
    content = msg.get("content")

    if typ == "user":
        if isinstance(content, str):
            text = content.strip()
        elif isinstance(content, list):
            text = "\n".join(
                b["text"].strip()
                for b in content
                if isinstance(b, dict)
                and b.get("type") == "text"
                and isinstance(b.get("text"), str)
                and b["text"].strip()
            )
        else:
            return []
        if not text:
            return []
        entry = _classify_user_text(text)
        return [Entry(*entry, ts)] if entry is not None else []

    if not isinstance(content, list):
        return []
    entries: list[Entry] = []
    for b in content:
        if not isinstance(b, dict):
            continue
        btype = b.get("type")
        if btype == "text" and isinstance(b.get("text"), str) and b["text"].strip():
            entries.append(Entry("assistant", b["text"].strip(), ts))
        elif btype == "tool_use":
            name = str(b.get("name") or "?")
            entries.append(Entry("tool", _summarize_tool_use(name, b.get("input")), ts))
    return entries


def read_conversation_tail(
    jsonl_path: Path,
    max_entries: int = 40,
    max_bytes: int = 512_000,
    end: int | None = None,
) -> list[Entry]:
    """Last conversation entries, newest last.

    Entry.role is 'user', 'assistant', 'tool' (tool call), 'command'
    (slash command) or 'command-output'. Only the tail of the file
    (max_bytes) is read, so this stays fast even for transcripts
    spanning many megabytes. `end` stops the read at that byte offset
    instead of the end of the file.
    """
    try:
        with open(jsonl_path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell() if end is None else min(end, f.tell())
            start = max(0, size - max_bytes)
            f.seek(start)
            data = f.read(size - start)
    except OSError:
        return []
    lines = data.decode("utf-8", errors="replace").splitlines()
    if start > 0 and lines:
        lines = lines[1:]  # the first line is usually cut mid-record

    entries: list[Entry] = []
    for line in lines:
        entries.extend(entries_from_line(line))
    return entries[-max_entries:]


def iter_conversation(jsonl_path: Path) -> Iterator[Entry]:
    """Stream the whole conversation, one entry at a time.

    Reads line by line, so even multi-hundred-megabyte transcripts
    never land in memory at once.
    """
    try:
        with open(jsonl_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                yield from entries_from_line(line)
    except OSError:
        return
