#!/usr/bin/env python3
"""Measure cognify's in-memory retention slope (SDK-507).

SDK-507 gates its per-wave-flush and full-streaming tiers on a **bytes-per-
document slope** for `cognify()`, and nothing in the repo could produce one:
no allocator hook, no `dhat`, and the bench measured wall-clock only.

`cognee-cli bench` now samples the process peak RSS at each phase boundary and
reports it, with the corpus byte count and cognify's chunk/embedding counts, in
a `memory` block on the result JSON. This script turns that into the slope: it
runs the bench at several `--num-memories` sizes over one corpus and fits peak
RSS against corpus size.

**Why a sweep and not a single run.** Peak RSS at one corpus size is
`baseline + slope x corpus`. The baseline — tokenizer, embedding engine, graph
and vector backends, tokio + rayon stacks — is large and fixed, so one run's
peak divided by its document count is mostly baseline and badly overstates the
slope. The slope only exists across two or more sizes. That is the whole reason
this lives in a script rather than in the bench result.

Fully offline: `--mock-llm` replays a committed cassette and
`MOCK_EMBEDDING=deterministic` keeps embeddings local, so no API key is needed.

The same runs also print the cross-chunk edge-resolution counters added for this
ticket (`cross_chunk`, `max_cross_chunk_distance`), which price what a per-wave
flush would cost. One sweep settles both of SDK-507's tier-2 prerequisites.

Usage:
  # default: the 135-chapter Moby-Dick corpus at 4 sizes
  python3 scripts/perf/measure_retention.py

  # explicit corpus, sizes and a prebuilt binary
  python3 scripts/perf/measure_retention.py \\
      --memories scripts/perf/fixtures/large/memories.json \\
      --cassette scripts/perf/fixtures/large/cassette.json \\
      --sizes 17,34,68,135 --bin target/release/cognee-cli

  # re-fit results already in --out-dir without re-running anything
  python3 scripts/perf/measure_retention.py --reuse

Self-check (the fit is doctested):
  python3 -m doctest scripts/perf/measure_retention.py -v
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MEMORIES = REPO_ROOT / "scripts/perf/fixtures/large/memories.json"
DEFAULT_CASSETTE = REPO_ROOT / "scripts/perf/fixtures/large/cassette.json"
DEFAULT_BIN = REPO_ROOT / "target/release/cognee-cli"
DEFAULT_OUT = REPO_ROOT / "target/perf/retention"

# The once-per-run counters from `expand_with_nodes_and_edges_with_stats`.
# Matched as independent fields rather than one ordered pattern so a future
# reordering of the `info!` degrades to "unavailable" instead of a wrong number.
# `cross_chunk=` cannot match `cross_chunk_by_id=` or `max_cross_chunk_distance=`:
# the `=` is part of the pattern.
EDGE_STAT_RES = {
    "attempted": re.compile(r"\battempted=(\d+)"),
    "cross_chunk": re.compile(r"\bcross_chunk=(\d+)"),
    "max_distance": re.compile(r"\bmax_cross_chunk_distance=(\d+)"),
}

MIB = 1024 * 1024


def least_squares(xs, ys):
    """Fit ``y = slope * x + intercept`` by ordinary least squares.

    Returns ``(slope, intercept, r_squared)``, or ``None`` when the points
    cannot define a line (fewer than two distinct ``x`` values).

    ``r_squared`` is reported as ``0.0`` — not as a division by zero — when the
    ``y`` values have no variance to explain, since a flat series is a real and
    interesting outcome here: it would mean peak RSS does not grow with the
    corpus at all.

    Two points, exact line:

    >>> least_squares([0.0, 10.0], [5.0, 25.0])
    (2.0, 5.0, 1.0)

    Four points, non-zero intercept:

    >>> slope, intercept, r2 = least_squares([1, 2, 3, 4], [13, 23, 33, 43])
    >>> round(slope, 9), round(intercept, 9), round(r2, 9)
    (10.0, 3.0, 1.0)

    A flat series is a zero slope, not an error:

    >>> least_squares([1, 2, 3], [7, 7, 7])
    (0.0, 7.0, 0.0)

    Noisy points still fit, with R^2 below 1:

    >>> slope, intercept, r2 = least_squares([0, 1, 2, 3], [0, 1, 2, 9])
    >>> round(slope, 4), r2 < 1.0
    (2.8, True)

    Degenerate inputs cannot define a line:

    >>> least_squares([4, 4], [1, 2]) is None
    True
    >>> least_squares([1], [1]) is None
    True
    """
    if len(xs) < 2 or len(xs) != len(ys):
        return None
    n = len(xs)
    mean_x = sum(xs) / n
    mean_y = sum(ys) / n
    sxx = sum((x - mean_x) ** 2 for x in xs)
    if sxx == 0:
        return None
    sxy = sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys))
    slope = sxy / sxx
    intercept = mean_y - slope * mean_x
    ss_tot = sum((y - mean_y) ** 2 for y in ys)
    if ss_tot == 0:
        return (slope, intercept, 0.0)
    ss_res = sum((y - (slope * x + intercept)) ** 2 for x, y in zip(xs, ys))
    return (slope, intercept, 1.0 - ss_res / ss_tot)


def human_bytes(n):
    """Render a byte count for a report line.

    >>> human_bytes(512)
    '512 B'
    >>> human_bytes(1536)
    '1.5 KiB'
    >>> human_bytes(3 * 1024 * 1024)
    '3.0 MiB'
    >>> human_bytes(-2 * 1024 * 1024 * 1024)
    '-2.0 GiB'
    """
    sign = "-" if n < 0 else ""
    n = abs(float(n))
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if n < 1024 or unit == "TiB":
            return f"{sign}{int(n)} B" if unit == "B" else f"{sign}{n:.1f} {unit}"
        n /= 1024
    raise AssertionError("unreachable: the loop returns at TiB")


def parse_sizes(raw, corpus_len):
    """Turn a ``--sizes`` string into a sorted list of run sizes.

    Sizes above the corpus length are clamped to it and de-duplicated, so
    ``--sizes`` can be written once and reused across corpora.

    >>> parse_sizes("10,20,40", 135)
    [10, 20, 40]
    >>> parse_sizes("40,10,20", 135)
    [10, 20, 40]
    >>> parse_sizes("50,100,200", 135)
    [50, 100, 135]
    >>> parse_sizes("0,5", 135)
    Traceback (most recent call last):
        ...
    ValueError: run sizes must be >= 1, got 0
    """
    sizes = []
    for part in raw.split(","):
        part = part.strip()
        if not part:
            continue
        value = int(part)
        if value < 1:
            raise ValueError(f"run sizes must be >= 1, got {value}")
        sizes.append(min(value, corpus_len))
    return sorted(set(sizes))


def default_sizes(corpus_len):
    """Four points spanning the corpus: 1/8, 1/4, 1/2, all of it.

    >>> default_sizes(135)
    [16, 33, 67, 135]
    >>> default_sizes(4)
    [1, 2, 4]
    """
    raw = [corpus_len // 8, corpus_len // 4, corpus_len // 2, corpus_len]
    return sorted({max(1, v) for v in raw})


def run_one(binary, memories, cassette, size, out_dir, extra_args):
    """Run the bench at one corpus size. Returns (result_json_path, log_path)."""
    result_path = out_dir / f"n{size}.json"
    log_path = out_dir / f"n{size}.log"
    cmd = [
        str(binary),
        "bench",
        "--mock-llm",
        "--mock-memories",
        str(cassette),
        "--memories",
        str(memories),
        "--num-memories",
        str(size),
        "--output",
        str(result_path),
        *extra_args,
    ]
    env = dict(os.environ)
    env["MOCK_EMBEDDING"] = "deterministic"
    # The cross-chunk counters are an `info!`; without this the sweep would
    # silently report "cross-chunk: unavailable" on a default log level.
    env.setdefault("RUST_LOG", "info")
    # Each run must be its own process: `ru_maxrss` is a process-lifetime
    # high-water mark, so two sizes measured in one process would report the
    # larger twice.
    print(f"  running n={size} ...", file=sys.stderr, flush=True)
    with log_path.open("wb") as log:
        completed = subprocess.run(cmd, stdout=log, stderr=log, env=env, check=False)
    if completed.returncode != 0:
        print(
            f"  warning: n={size} exited {completed.returncode}; see {log_path}",
            file=sys.stderr,
        )
    return result_path, log_path


def edge_stats_from_log(log_path):
    """Pull the edge-resolution counters out of a run log.

    Returns a dict with whichever of ``attempted`` / ``cross_chunk`` /
    ``max_distance`` were found, or ``None`` if the log has none of them (the
    run logged below `info`, or failed before expansion).
    """
    try:
        text = log_path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    found = {}
    for key, pattern in EDGE_STAT_RES.items():
        match = pattern.search(text)
        if match:
            found[key] = int(match.group(1))
    return found or None


def load_row(result_path, log_path):
    """Read one run's result JSON into a row, or ``None`` if unusable."""
    try:
        doc = json.loads(result_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"  skipping {result_path.name}: {error}", file=sys.stderr)
        return None
    memory = doc.get("memory") or {}
    if not memory:
        print(
            f"  skipping {result_path.name}: no `memory` block — binary predates SDK-507",
            file=sys.stderr,
        )
        return None
    if not memory.get("peak_rss_supported"):
        print(
            f"  skipping {result_path.name}: this platform reports no peak RSS",
            file=sys.stderr,
        )
        return None
    peak = memory.get("peak_rss_bytes_after_cognify")
    if peak is None:
        print(f"  skipping {result_path.name}: no post-cognify peak", file=sys.stderr)
        return None
    if not doc.get("success"):
        # A failed cognify may have stopped early, so its peak is not a point
        # on the same curve. Report it and leave it out of the fit.
        print(
            f"  skipping {result_path.name}: run reported success=false "
            f"(status: {doc.get('status')})",
            file=sys.stderr,
        )
        return None
    return {
        "documents": memory.get("corpus_documents", doc.get("memories_count", 0)),
        "corpus_bytes": memory.get("corpus_bytes", 0),
        "chunks": memory.get("cognify_chunks"),
        "embeddings": memory.get("cognify_embeddings"),
        "baseline": memory.get("peak_rss_bytes_baseline"),
        "peak_after_add": memory.get("peak_rss_bytes_after_add"),
        "peak_after_cognify": peak,
        "cognify_time_s": doc.get("cognify_time_s"),
        "dims": (doc.get("config") or {}).get("embedding_dimensions"),
        "nodes": doc.get("node_count"),
        "edges": doc.get("edge_count"),
        "edge_stats": edge_stats_from_log(log_path),
    }


def extraction_is_live(row):
    """Whether this run's LLM extraction actually produced edges.

    The replay mock's default miss policy is `EmptyGraph`: a stale cassette
    returns empty graphs **silently**, and `bench`'s own stale-cassette guard
    only requires one node — which chunk and document nodes supply on their own.
    A run in that state still reports `success: true`, and its peak RSS measures
    chunking, embedding and storage with no entities at all. That is a real
    lower bound but not the number this ticket is about, so it must never be
    mistaken for one.

    >>> extraction_is_live({"edge_stats": {"attempted": 204}})
    True
    >>> extraction_is_live({"edge_stats": {"attempted": 0}})
    False
    >>> extraction_is_live({"edge_stats": None}) is None
    True
    >>> extraction_is_live({"edge_stats": {"cross_chunk": 0}}) is None
    True
    """
    stats = row.get("edge_stats")
    if not stats or "attempted" not in stats:
        return None
    return stats["attempted"] > 0


def report(rows):
    """Print the table and the fits. Returns the exit status."""
    if not rows:
        print("\nNo usable runs - nothing to fit.", file=sys.stderr)
        return 1

    print("\n=== Runs ===")
    header = (
        f"{'docs':>6}  {'corpus':>10}  {'chunks':>7}  {'embeds':>7}  {'edges':>7}  "
        f"{'baseline':>10}  {'peak@add':>10}  {'peak@cognify':>13}  {'cognify_s':>9}"
    )
    print(header)
    print("-" * len(header))
    for row in rows:
        stats = row["edge_stats"] or {}
        print(
            f"{row['documents']:>6}  {human_bytes(row['corpus_bytes']):>10}  "
            f"{str(row['chunks'] if row['chunks'] is not None else '?'):>7}  "
            f"{str(row['embeddings'] if row['embeddings'] is not None else '?'):>7}  "
            f"{str(stats.get('attempted', '?')):>7}  "
            f"{human_bytes(row['baseline'] or 0):>10}  "
            f"{human_bytes(row['peak_after_add'] or 0):>10}  "
            f"{human_bytes(row['peak_after_cognify']):>13}  "
            f"{row['cognify_time_s']:>9}"
        )

    # The `edges` column is `attempted` - edges the LLM proposed, before
    # endpoint resolution. Zero means extraction produced nothing.
    dead = [r for r in rows if extraction_is_live(r) is False]
    unknown = [r for r in rows if extraction_is_live(r) is None]
    if dead:
        print("\n" + "!" * 72)
        print(
            "!! STRUCTURE-ONLY RESULT - the LLM extracted zero edges in "
            f"{len(dead)} of {len(rows)} runs."
        )
        print("!! The replay cassette is stale for this corpus and the mock's")
        print("!! default EmptyGraph miss policy swallowed it silently. These")
        print("!! runs measure chunking + embedding + storage with NO entities,")
        print("!! so the slope below is a LOWER BOUND on the real one, and the")
        print("!! cross-chunk counters are vacuous (no edges to resolve).")
        print("!! Re-record the cassette (scripts/perf/README.md) for the real")
        print("!! figure, or pass --min-graph-nodes <recorded baseline> to make")
        print("!! bench fail loudly instead of reporting success.")
        print("!" * 72)
    elif unknown:
        print(
            f"\nNote: {len(unknown)} run(s) logged no edge-resolution counters, so"
            "\nwhether extraction was live could not be confirmed. Re-run with"
            "\nRUST_LOG=info."
        )

    if len(rows) < 2:
        print(
            "\nOnly one usable run: a slope needs at least two corpus sizes.\n"
            "Re-run with more --sizes.",
            file=sys.stderr,
        )
        return 1

    peaks = [r["peak_after_cognify"] for r in rows]

    print("\n=== Fits (peak RSS after cognify) ===")
    print(
        "Read the intercept as the fixed process cost and the slope as what one\n"
        "more unit of corpus adds to the whole-run peak. A low R^2 means the\n"
        "points are not on a line and the slope should not be extrapolated."
    )
    axes = (
        ("per corpus byte", "corpus_bytes", "B of peak RSS per corpus byte"),
        ("per document", "documents", "B of peak RSS per document"),
        ("per chunk", "chunks", "B of peak RSS per chunk"),
    )
    fits = {}
    for label, key, unit in axes:
        xs = [r[key] for r in rows]
        if any(x is None for x in xs):
            print(f"\n  {label}: unavailable (missing {key} on some runs)")
            continue
        fit = least_squares(xs, peaks)
        if fit is None:
            print(f"\n  {label}: not enough distinct values of {key} to fit")
            continue
        fits[key] = fit
        slope, intercept, r2 = fit
        print(f"\n  {label}:")
        print(f"    slope     = {slope:,.2f} {unit}")
        print(f"    intercept = {human_bytes(intercept)}  (fixed process cost)")
        print(f"    R^2       = {r2:.4f}")

    print("\n=== Projection ===")
    print(
        "Each axis is projected independently. They are fitted to the SAME runs,\n"
        "so where they disagree wildly the growth is not really driven by the\n"
        "axis that disagrees - many small documents, for instance, make the\n"
        "per-byte slope enormous because the cost tracks entities, not text."
    )
    # 171,888 documents is the corpus that prompted SDK-507.
    targets = (
        ("corpus_bytes", "3 GiB of corpus text", 3 * 1024 * MIB),
        ("documents", "171,888 documents", 171_888),
        ("chunks", "171,888 chunks", 171_888),
    )
    for key, label, x in targets:
        fit = fits.get(key)
        if fit is None:
            continue
        slope, intercept, r2 = fit
        if r2 < 0.9:
            print(f"  {label:>22}: R^2 {r2:.3f} too weak to project")
            continue
        print(f"  {label:>22}: peak ~ {human_bytes(intercept + slope * x)}")
    print(
        "\n  These are linear extrapolations far outside the measured range and\n"
        "  are not predictions. They answer one question only: does the measured\n"
        "  slope leave headroom on a fixed-size box, or not. Note also that peak\n"
        "  RSS is a WHOLE-PROCESS figure - it includes allocator pages not\n"
        "  returned to the OS and the vector store's own buffers, so it does not\n"
        "  by itself attribute the growth to cognify's payload clones. Attributing\n"
        "  it needs an allocator-level profiler, which this repo still lacks."
    )

    largest = rows[-1]
    stats = largest["edge_stats"] or {}
    print("\n=== Cross-chunk edge resolution (SDK-507 tier-2 cost) ===")
    if "cross_chunk" not in stats:
        print("  unavailable - no counter line in the largest run's log.")
    elif not stats.get("attempted"):
        print("  vacuous - zero edges were extracted (see the banner above).")
    else:
        print(
            f"  largest run ({largest['documents']} docs, {largest['chunks']} chunks, "
            f"{stats['attempted']} edges attempted):"
        )
        print(f"    cross_chunk              = {stats['cross_chunk']}")
        print(f"    max_cross_chunk_distance = {stats.get('max_distance', '?')}")
        print(
            "  The count is the upper bound on resolutions a per-wave flush\n"
            "  could lose; the distance is how wide a wave must be to keep most\n"
            "  of them. Zero means run-global resolution bought nothing on THIS\n"
            "  corpus - one data point, not a general result: a corpus whose\n"
            "  entities recur across documents is exactly where it would not be."
        )
    return 0


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--memories", default=str(DEFAULT_MEMORIES), help="Corpus JSON.")
    ap.add_argument("--cassette", default=str(DEFAULT_CASSETTE), help="Replay cassette.")
    ap.add_argument(
        "--sizes",
        help="Comma-separated --num-memories values (default: 1/8, 1/4, 1/2, all).",
    )
    ap.add_argument("--bin", help="Prebuilt cognee-cli (default: build a release one).")
    ap.add_argument("--out-dir", default=str(DEFAULT_OUT), help="Where results land.")
    ap.add_argument(
        "--reuse",
        action="store_true",
        help="Do not run anything; re-fit the results already in --out-dir.",
    )
    ap.add_argument(
        "bench_args",
        nargs="*",
        help="Extra args forwarded to `cognee-cli bench` (after a --).",
    )
    args = ap.parse_args()

    memories_path = Path(args.memories)
    cassette_path = Path(args.cassette)
    out_dir = Path(args.out_dir)

    if not memories_path.is_file():
        sys.exit(
            f"error: corpus not found at {memories_path}\n"
            "       build it with `python3 scripts/perf/build_large_corpus.py`."
        )
    corpus_len = len(json.loads(memories_path.read_text(encoding="utf-8")))
    sizes = parse_sizes(args.sizes, corpus_len) if args.sizes else default_sizes(corpus_len)

    out_dir.mkdir(parents=True, exist_ok=True)

    if not args.reuse:
        if not cassette_path.is_file():
            sys.exit(f"error: cassette not found at {cassette_path}")
        if args.bin:
            binary = Path(args.bin)
        else:
            print("Building cognee-cli --release (pass --bin to skip)...", file=sys.stderr)
            subprocess.run(
                ["cargo", "build", "--release", "-p", "cognee-cli", "--features", "bench"],
                cwd=REPO_ROOT,
                check=True,
            )
            binary = DEFAULT_BIN
        if not binary.is_file():
            sys.exit(f"error: binary not found at {binary}")
        if shutil.which(str(binary)) is None and not os.access(binary, os.X_OK):
            sys.exit(f"error: {binary} is not executable")

        print(f"Corpus   : {memories_path} ({corpus_len} documents)", file=sys.stderr)
        print(f"Sizes    : {sizes}", file=sys.stderr)
        print(f"Out dir  : {out_dir}", file=sys.stderr)
        for size in sizes:
            run_one(binary, memories_path, cassette_path, size, out_dir, args.bench_args)

    rows = []
    for size in sizes:
        row = load_row(out_dir / f"n{size}.json", out_dir / f"n{size}.log")
        if row is not None:
            rows.append(row)
    rows.sort(key=lambda r: r["corpus_bytes"])
    sys.exit(report(rows))


if __name__ == "__main__":
    main()
