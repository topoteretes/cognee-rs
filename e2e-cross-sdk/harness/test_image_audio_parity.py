"""Cross-SDK add parity tests for image and audio inputs.

Both SDKs run their image and audio loaders at ``add``: an image is described
by the vision LLM and an audio file transcribed by Whisper, both through the
configured LLM key, so these tests need a key on both sides. Python 1d3ed15
(``cognee/tasks/ingestion/ingest_data.py``, ``loaders/store_derived_text.py``,
``loaders/core/image_loader.py``) stores the derived text, not the raw file, in
``raw_data_location``, and keeps the raw file in ``original_data_location``
(SDK-1039 D11).

Only deterministic fields are compared; the description and the transcript are
LLM output and are never compared across SDKs.

Verified assertions per media type:
- ``content_hash``   — MD5 of raw bytes is identical in both SDKs
- ``data.id``        — a random uuid4 owned by the dataset in both SDKs (D7);
                       ids are not compared across SDKs
- ``extension`` / ``mime_type`` — ``txt`` / ``text/plain`` (the derived text)
- ``loader_engine``  — ``image_loader`` / ``audio_loader``
- stored files       — ``original_data_location`` holds the raw bytes;
                       ``raw_data_location`` is ``text_<md5(raw)>.txt`` and
                       ``raw_content_hash`` is the MD5 of that file
"""

import hashlib
import struct
import uuid
import zlib
from pathlib import Path

from helpers import (
    open_db,
    query_data,
    query_datasets,
    python_db_path,
    rust_db_path,
    resolve_stored_file,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
)


# ── Minimal valid binary fixtures ─────────────────────────────────────────────


def _make_minimal_png() -> bytes:
    """Return a valid 1×1 red-pixel PNG (no external library required)."""
    def _chunk(name: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(name + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + name + data + struct.pack(">I", crc)

    sig = b"\x89PNG\r\n\x1a\n"
    ihdr_data = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)   # 1×1, 8-bit RGB
    ihdr = _chunk(b"IHDR", ihdr_data)
    # Single red pixel: filter byte (0x00) + R=255 G=0 B=0
    raw_row = b"\x00\xff\x00\x00"
    idat = _chunk(b"IDAT", zlib.compress(raw_row))
    iend = _chunk(b"IEND", b"")
    return sig + ihdr + idat + iend


def _make_minimal_wav() -> bytes:
    """Return a minimal valid WAV file containing 0.5 s of silence (mono, 16-bit, 8 kHz).

    At least 0.1 s: Python transcribes audio during ``add`` (its loader runs
    at ingest), and OpenAI's transcription endpoint rejects shorter files with
    "Audio file is too short. Minimum audio length is 0.1 seconds."
    """
    num_samples = 4000   # 0.5 s at 8 kHz
    sample_rate = 8000
    num_channels = 1
    bits_per_sample = 16
    byte_rate = sample_rate * num_channels * bits_per_sample // 8
    block_align = num_channels * bits_per_sample // 8
    data_chunk = b"\x00\x00" * num_samples   # silence

    fmt = struct.pack(
        "<HHIIHH",
        1,              # PCM
        num_channels,
        sample_rate,
        byte_rate,
        block_align,
        bits_per_sample,
    )
    riff = (
        b"RIFF"
        + struct.pack("<I", 4 + 8 + len(fmt) + 8 + len(data_chunk))
        + b"WAVE"
        + b"fmt "
        + struct.pack("<I", len(fmt))
        + fmt
        + b"data"
        + struct.pack("<I", len(data_chunk))
        + data_chunk
    )
    return riff


_PNG_BYTES = _make_minimal_png()
_WAV_BYTES = _make_minimal_wav()


# ── Shared helper ─────────────────────────────────────────────────────────────


def _run_both_sdks(tmp_path, filename: str, file_bytes: bytes, dataset: str):
    """Add *filename* (with *file_bytes*) through both SDKs.

    Returns ``(py_ws, rust_ws, py_row, rust_row)`` — the workspace directories
    and the first data-table row from each SDK's SQLite DB.
    """
    py_ws = tmp_path / "python"
    rust_ws = tmp_path / "rust"
    py_ws.mkdir()
    rust_ws.mkdir()

    # Write the fixture file to both workspaces
    py_file = py_ws / filename
    py_file.write_bytes(file_bytes)
    rust_file = rust_ws / filename
    rust_file.write_bytes(file_bytes)

    # ── Python SDK add ────────────────────────────────────────────────────
    py_result = run_python_cli(py_ws, ["add", str(py_file), "-d", dataset], check=False)
    assert py_result.returncode == 0, (
        f"Python add failed:\n{py_result.stdout}\n{py_result.stderr}"
    )

    py_conn = open_db(python_db_path(py_ws))
    py_datasets = query_datasets(py_conn)
    assert py_datasets, "Python: expected at least one dataset"
    py_owner = py_datasets[0]["owner_id"]
    py_tenant = py_datasets[0].get("tenant_id")
    py_conn.close()

    # ── Rust SDK add (synced user/tenant IDs) ────────────────────────────
    write_rust_config(rust_ws, user_id=str(py_owner))
    rust_args = ["add", str(rust_file), "-d", dataset]
    if py_tenant:
        rust_args.extend(["--tenant-id", str(py_tenant)])

    rust_result = run_rust_cli(rust_ws, rust_args, check=False)
    assert rust_result.returncode == 0, (
        f"Rust add failed:\n{rust_result.stdout}\n{rust_result.stderr}"
    )

    py_conn = open_db(python_db_path(py_ws))
    rust_conn = open_db(rust_db_path(rust_ws))
    py_rows = query_data(py_conn)
    rust_rows = query_data(rust_conn)
    py_conn.close()
    rust_conn.close()

    assert len(py_rows) == 1, f"Python: expected 1 data row, got {len(py_rows)}"
    assert len(rust_rows) == 1, f"Rust: expected 1 data row, got {len(rust_rows)}"

    return py_ws, rust_ws, py_rows[0], rust_rows[0]


def _assert_dataset_scoped_uuid4(db_path, sdk: str) -> None:
    """The one data row is a random uuid4 owned by its dataset (SDK-1039 D7).

    Python's ``add`` mints ``uuid4()`` for a new ``data`` row and dedups by
    ``(content_hash, dataset_id, owner_id, tenant_id)``
    (``cognee/tasks/ingestion/ingest_data.py:364-389``); Rust does the same,
    so data ids are never equal across SDKs.
    """
    conn = open_db(db_path)
    rows = query_data(conn)
    datasets = query_datasets(conn)
    conn.close()
    assert len(rows) == 1, f"{sdk}: expected 1 data row, got {len(rows)}"
    assert len(datasets) == 1, f"{sdk}: expected 1 dataset, got {len(datasets)}"
    assert uuid.UUID(str(rows[0]["id"])).version == 4, (
        f"{sdk}: data.id {rows[0]['id']} is not a uuid4"
    )
    assert rows[0].get("dataset_id") == datasets[0]["id"], (
        f"{sdk}: data.dataset_id {rows[0].get('dataset_id')!r} is not the "
        f"dataset's id {datasets[0]['id']!r}"
    )


def _read_location(ws: Path, location: str) -> bytes:
    """Read a stored location; a relative path is relative to the workspace."""
    path = resolve_stored_file(ws / ".data_storage", location)
    if not path.is_absolute() and not path.exists():
        path = ws / path
    return path.read_bytes()


def _assert_stored_files_match_python_contract(
    py_ws: Path, rust_ws: Path, py_row: dict, rust_row: dict, raw: bytes
) -> None:
    """Both SDKs store the derived text and keep the raw file (SDK-1039 D11).

    Python 1d3ed15, run on these fixtures: ``raw_data_location`` is the derived
    ``text_<md5(raw)>.txt`` and ``raw_content_hash`` its MD5;
    ``original_data_location`` holds the raw file. The text itself is LLM
    output and is not compared across SDKs.
    """
    expected_name = f"text_{hashlib.md5(raw).hexdigest()}.txt"
    for sdk, ws, row in (("Python", py_ws, py_row), ("Rust", rust_ws, rust_row)):
        original = _read_location(ws, row["original_data_location"])
        assert original == raw, (
            f"{sdk}: original_data_location {row['original_data_location']!r} holds "
            f"{len(original)} bytes, not the {len(raw)}-byte fixture"
        )
        location = row["raw_data_location"]
        assert Path(resolve_stored_file(ws, location)).name == expected_name, (
            f"{sdk}: raw_data_location {location!r} is not {expected_name}"
        )
        stored = _read_location(ws, location)
        assert row.get("raw_content_hash") == hashlib.md5(stored).hexdigest(), (
            f"{sdk}: raw_content_hash {row.get('raw_content_hash')!r} is not the MD5 "
            f"of the stored text"
        )


# ── Image parity tests ────────────────────────────────────────────────────────


def test_add_image_content_hash_matches(tmp_path):
    """Both SDKs must produce the same MD5 content_hash for an identical PNG."""
    _, _, py_row, rust_row = _run_both_sdks(
        tmp_path, "photo.png", _PNG_BYTES, "image_parity"
    )
    expected_md5 = hashlib.md5(_PNG_BYTES).hexdigest()

    assert py_row["content_hash"] == expected_md5, (
        f"Python content_hash {py_row['content_hash']!r} != expected MD5 {expected_md5!r}"
    )
    assert rust_row["content_hash"] == expected_md5, (
        f"Rust content_hash {rust_row['content_hash']!r} != expected MD5 {expected_md5!r}"
    )


def test_add_image_data_id_is_dataset_scoped_uuid4(tmp_path):
    """Both SDKs give the image's data row a random uuid4 owned by its dataset.

    Replaces the cross-SDK ``data.id`` equality check, whose uuid5 premise
    Python no longer has (SDK-1039 D7).
    """
    py_ws, rust_ws, _, _ = _run_both_sdks(
        tmp_path, "photo.png", _PNG_BYTES, "image_parity"
    )
    _assert_dataset_scoped_uuid4(python_db_path(py_ws), "Python")
    _assert_dataset_scoped_uuid4(rust_db_path(rust_ws), "Rust")


def test_add_image_metadata_matches(tmp_path):
    """extension, mime_type, and loader_engine must match for a PNG input."""
    _, _, py_row, rust_row = _run_both_sdks(
        tmp_path, "photo.png", _PNG_BYTES, "image_parity"
    )
    for field in ("extension", "mime_type", "loader_engine"):
        assert py_row.get(field) == rust_row.get(field), (
            f"{field} mismatch:\n"
            f"  Python: {py_row.get(field)!r}\n"
            f"  Rust:   {rust_row.get(field)!r}"
        )
    assert rust_row.get("loader_engine") == "image_loader", (
        f"expected loader_engine='image_loader', got {rust_row.get('loader_engine')!r}"
    )


def test_add_image_stored_files_match(tmp_path):
    """Both SDKs keep the raw PNG and store its description as text (D11)."""
    py_ws, rust_ws, py_row, rust_row = _run_both_sdks(
        tmp_path, "photo.png", _PNG_BYTES, "image_parity"
    )
    _assert_stored_files_match_python_contract(
        py_ws, rust_ws, py_row, rust_row, _PNG_BYTES
    )


# ── Loader routing by content (SDK-1039 P20) ─────────────────────────────────


def test_add_png_named_jpg_routes_by_content(tmp_path):
    """PNG bytes named ``.jpg`` are an image in both SDKs, recorded as PNG.

    Python ``LoaderEngine.get_loader`` sniffs the bytes, so ``image_loader``
    claims the file through the sniffed ``png`` / ``image/png``, and the row
    stores those sniffed values rather than the name's. Rust now does the same.
    """
    py_ws, rust_ws, py_row, rust_row = _run_both_sdks(
        tmp_path, "image.jpg", _PNG_BYTES, "image_routing"
    )
    for field in (
        "loader_engine",
        "original_extension",
        "original_mime_type",
        "extension",
        "mime_type",
    ):
        assert py_row.get(field) == rust_row.get(field), (
            f"{field} mismatch:\n"
            f"  Python: {py_row.get(field)!r}\n"
            f"  Rust:   {rust_row.get(field)!r}"
        )
    assert rust_row["loader_engine"] == "image_loader"
    assert rust_row["original_extension"] == "png"
    assert rust_row["original_mime_type"] == "image/png"
    _assert_stored_files_match_python_contract(
        py_ws, rust_ws, py_row, rust_row, _PNG_BYTES
    )


def test_add_png_without_extension_fails_in_both(tmp_path):
    """A PNG with no extension is routed to the image loader and fails there.

    Premise corrected with Python evidence (SDK-1039 P20). ``get_loader``
    does pick ``image_loader`` for it (sniffed ``png`` / ``image/png``), but
    the vision call takes the MIME type from the file NAME
    (``native_adapter.py:633-638``: ``mimetypes.guess_type(input)``, else
    ``ValueError: Could not determine MIME type for image file ... Is the
    extension correct?``), so Python 1d3ed15 fails the add. Rust matches.
    """
    py_ws = tmp_path / "python"
    rust_ws = tmp_path / "rust"
    py_ws.mkdir()
    rust_ws.mkdir()
    py_file = py_ws / "image"
    rust_file = rust_ws / "image"
    py_file.write_bytes(_PNG_BYTES)
    rust_file.write_bytes(_PNG_BYTES)

    py_result = run_python_cli(py_ws, ["add", str(py_file), "-d", "image_noext"], check=False)
    write_rust_config(rust_ws)
    rust_result = run_rust_cli(rust_ws, ["add", str(rust_file), "-d", "image_noext"], check=False)

    py_out = f"{py_result.stdout}\n{py_result.stderr}"
    rust_out = f"{rust_result.stdout}\n{rust_result.stderr}"
    assert py_result.returncode != 0, f"Python accepted an extensionless image:\n{py_out}"
    assert "Could not determine MIME type" in py_out, py_out
    assert rust_result.returncode != 0, f"Rust accepted an extensionless image:\n{rust_out}"
    assert "Could not determine MIME type" in rust_out, rust_out


# ── Audio parity tests ────────────────────────────────────────────────────────


def test_add_audio_content_hash_matches(tmp_path):
    """Both SDKs must produce the same MD5 content_hash for an identical WAV."""
    _, _, py_row, rust_row = _run_both_sdks(
        tmp_path, "speech.wav", _WAV_BYTES, "audio_parity"
    )
    expected_md5 = hashlib.md5(_WAV_BYTES).hexdigest()

    assert py_row["content_hash"] == expected_md5, (
        f"Python content_hash {py_row['content_hash']!r} != expected MD5 {expected_md5!r}"
    )
    assert rust_row["content_hash"] == expected_md5, (
        f"Rust content_hash {rust_row['content_hash']!r} != expected MD5 {expected_md5!r}"
    )


def test_add_audio_data_id_is_dataset_scoped_uuid4(tmp_path):
    """Both SDKs give the audio file's data row a random uuid4 owned by its dataset.

    Replaces the cross-SDK ``data.id`` equality check, whose uuid5 premise
    Python no longer has (SDK-1039 D7).
    """
    py_ws, rust_ws, _, _ = _run_both_sdks(
        tmp_path, "speech.wav", _WAV_BYTES, "audio_parity"
    )
    _assert_dataset_scoped_uuid4(python_db_path(py_ws), "Python")
    _assert_dataset_scoped_uuid4(rust_db_path(rust_ws), "Rust")


def test_add_audio_metadata_matches(tmp_path):
    """extension, mime_type, and loader_engine must match for a WAV input."""
    _, _, py_row, rust_row = _run_both_sdks(
        tmp_path, "speech.wav", _WAV_BYTES, "audio_parity"
    )
    for field in ("extension", "mime_type", "loader_engine"):
        assert py_row.get(field) == rust_row.get(field), (
            f"{field} mismatch:\n"
            f"  Python: {py_row.get(field)!r}\n"
            f"  Rust:   {rust_row.get(field)!r}"
        )
    assert rust_row.get("loader_engine") == "audio_loader", (
        f"expected loader_engine='audio_loader', got {rust_row.get('loader_engine')!r}"
    )


def test_add_audio_stored_files_match(tmp_path):
    """Both SDKs keep the raw WAV and store its transcript as text (D11)."""
    py_ws, rust_ws, py_row, rust_row = _run_both_sdks(
        tmp_path, "speech.wav", _WAV_BYTES, "audio_parity"
    )
    _assert_stored_files_match_python_contract(
        py_ws, rust_ws, py_row, rust_row, _WAV_BYTES
    )


# ── Deduplication across SDKs ─────────────────────────────────────────────────


def test_add_image_deduplication_within_rust(tmp_path):
    """Adding the same PNG twice in Rust must produce exactly 1 data row."""
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()
    write_rust_config(rust_ws)

    img1 = rust_ws / "photo_a.png"
    img2 = rust_ws / "photo_b.png"
    img1.write_bytes(_PNG_BYTES)
    img2.write_bytes(_PNG_BYTES)

    run_rust_cli(rust_ws, ["add", str(img1), "-d", "dedup_img"])
    run_rust_cli(rust_ws, ["add", str(img2), "-d", "dedup_img"])

    rows = query_data(open_db(rust_db_path(rust_ws)))
    assert len(rows) == 1, f"Rust image dedup failed: got {len(rows)} rows, expected 1"


def test_add_audio_deduplication_within_rust(tmp_path):
    """Adding the same WAV twice in Rust must produce exactly 1 data row."""
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()
    write_rust_config(rust_ws)

    aud1 = rust_ws / "track_a.wav"
    aud2 = rust_ws / "track_b.wav"
    aud1.write_bytes(_WAV_BYTES)
    aud2.write_bytes(_WAV_BYTES)

    run_rust_cli(rust_ws, ["add", str(aud1), "-d", "dedup_aud"])
    run_rust_cli(rust_ws, ["add", str(aud2), "-d", "dedup_aud"])

    rows = query_data(open_db(rust_db_path(rust_ws)))
    assert len(rows) == 1, f"Rust audio dedup failed: got {len(rows)} rows, expected 1"
