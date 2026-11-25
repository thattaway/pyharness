from __future__ import annotations

import os

from ccsessions.core.cache import TokenCache
from ccsessions.core.models import TokenStats
from ccsessions.core.parser import get_token_stats

from conftest import write_transcript


def test_cache_roundtrip_and_invalidation(tmp_path):
    f = tmp_path / "s.jsonl"
    write_transcript(f, exchanges=2)
    cache_path = tmp_path / "cache.json"

    cache = TokenCache(cache_path)
    first = get_token_stats(f, cache)
    assert first.total == 30
    cache.save()
    assert cache_path.is_file()

    # a fresh cache instance serves the stored entry
    cache2 = TokenCache(cache_path)
    cached = cache2.get(f)
    assert cached is not None
    assert cached.total == 30

    # modifying the file invalidates the entry
    write_transcript(f, exchanges=3)
    os.utime(f, ns=(1, 1))  # force a different mtime
    assert cache2.get(f) is None
    assert get_token_stats(f, cache2).total == 45


def test_corrupted_cache_file_is_tolerated(tmp_path):
    cache_path = tmp_path / "cache.json"
    cache_path.write_text("{not json", encoding="utf-8")
    cache = TokenCache(cache_path)
    f = tmp_path / "s.jsonl"
    write_transcript(f)
    assert get_token_stats(f, cache).total == 30


def test_entries_from_an_older_stats_version_are_recomputed(tmp_path):
    jsonl = tmp_path / "s.jsonl"
    jsonl.write_text("{}\n", encoding="utf-8")
    cache = TokenCache(tmp_path / "cache.json")
    cache.put(jsonl, TokenStats(input=5), jsonl.stat())
    assert cache.get(jsonl) is not None
    cache._entries[str(jsonl)].pop("version")
    assert cache.get(jsonl) is None
