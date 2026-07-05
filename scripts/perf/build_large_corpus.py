#!/usr/bin/env python3
"""Build the large-document benchmark corpus from a public-domain book.

Turns a Project Gutenberg plain-text book into the `{title, content,
references}` corpus shape the `cognee-cli bench` subcommand expects (same shape
as `fixtures/memories.json`), one memory per chapter.

Default book: Moby-Dick (Gutenberg ebook 2701, ~1.2 MB). Large enough to push
cognify past the point where it is CPU-bound, unlike the 50-memory fixture.

The output is deterministic (no random content), so the replay cassette lookup
hashes are stable across runs. Commit the emitted JSON like the small fixture;
recording its cassette is the only step that needs LLM credentials.

Usage:
  # download + build (writes fixtures/large/memories.json):
  python3 scripts/perf/build_large_corpus.py

  # from an already-downloaded text, custom output:
  python3 scripts/perf/build_large_corpus.py --source moby.txt -o out.json
"""

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path

DEFAULT_URL = "https://www.gutenberg.org/files/2701/2701-0.txt"
DEFAULT_OUT = Path(__file__).resolve().parent / "fixtures" / "large" / "memories.json"

# Gutenberg wraps the work in these banners; the text between them is the book.
START_RE = re.compile(r"\*\*\* START OF THE PROJECT GUTENBERG EBOOK.*?\*\*\*", re.S)
END_RE = re.compile(r"\*\*\* END OF THE PROJECT GUTENBERG EBOOK.*?\*\*\*", re.S)
# A chapter heading on its own line, e.g. "CHAPTER 1. Loomings."
CHAPTER_RE = re.compile(r"^CHAPTER (\d+)\. (.+)$", re.M)


def strip_gutenberg(raw: str) -> str:
    """Return only the book body, without the Gutenberg header/footer."""
    start = START_RE.search(raw)
    end = END_RE.search(raw)
    body = raw[start.end() if start else 0 : end.start() if end else len(raw)]
    return body.strip()


def split_chapters(body: str) -> list[dict]:
    """Split the body into one memory per chapter.

    The book lists every chapter twice: once in the table of contents, then
    again as the actual chapter. The body starts where the chapter numbering
    restarts at 1 (the second "CHAPTER 1."), so we anchor there.
    """
    matches = list(CHAPTER_RE.finditer(body))
    if not matches:
        sys.exit("error: no chapter headings found. Is this the expected book?")

    # Find the restart of chapter 1 (end of the table of contents).
    first_one = int(matches[0].group(1))
    body_start_idx = 0
    for i, m in enumerate(matches):
        if i > 0 and int(m.group(1)) == first_one:
            body_start_idx = i
            break
    chapters = matches[body_start_idx:]

    out: list[dict] = []
    for i, m in enumerate(chapters):
        title = f"CHAPTER {m.group(1)}. {m.group(2).strip()}"
        text_start = m.end()
        text_end = chapters[i + 1].start() if i + 1 < len(chapters) else len(body)
        content = body[text_start:text_end].strip()
        if content:
            out.append({"title": title, "content": content, "references": []})
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", default=DEFAULT_URL, help="Gutenberg plain-text URL.")
    ap.add_argument("--source", help="Local text file to use instead of downloading.")
    ap.add_argument("-o", "--output", default=str(DEFAULT_OUT), help="Output JSON path.")
    args = ap.parse_args()

    if args.source:
        raw = Path(args.source).read_text(encoding="utf-8", errors="replace")
    else:
        print(f"downloading {args.url} ...", file=sys.stderr)
        with urllib.request.urlopen(args.url) as resp:  # noqa: S310 (trusted URL)
            raw = resp.read().decode("utf-8", errors="replace")

    memories = split_chapters(strip_gutenberg(raw))

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(memories, ensure_ascii=False, indent=1), encoding="utf-8")

    chars = sum(len(m["content"]) for m in memories)
    print(
        f"wrote {len(memories)} chapters, {chars:,} chars (~{chars // 4:,} tokens) to {out_path}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
