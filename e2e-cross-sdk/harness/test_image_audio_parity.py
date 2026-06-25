"""Cross-SDK add parity tests for image and audio inputs.

Both SDKs are tested against **deterministic** assertions only — no LLM or
API key is required.  The ``add`` operation stores raw bytes as-is and records
metadata derived from the file extension.  LLM-based content extraction
(vision / Whisper) happens during ``cognify``, not ``add``.

Verified assertions per media type:
- ``content_hash``   — MD5 of raw bytes is identical in both SDKs
- ``data.id``        — UUID5(content_hash, owner_id, tenant_id) matches
- ``extension``      — derived from the file name
- ``mime_type``      — MIME inferred from extension
- ``loader_engine``  — Python-compatible engine name (``image_loader`` /
                       ``audio_loader``)
- stored bytes       — the raw file saved to disk is byte-for-byte identical
"""

import hashlib
import struct
import zlib

from helpers import (
    open_db,
    query_data,
    query_datasets,
    python_db_path,
    rust_db_path,
    read_stored_file,
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
    """Return a minimal valid WAV file containing ~0.05 s of silence (mono, 16-bit, 8 kHz)."""
    num_samples = 400   # 0.05 s at 8 kHz
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


def test_add_image_data_id_matches(tmp_path):
    """With synced user_id + tenant_id, image data.id must be identical (UUID5)."""
    _, _, py_row, rust_row = _run_both_sdks(
        tmp_path, "photo.png", _PNG_BYTES, "image_parity"
    )
    assert py_row["id"] == rust_row["id"], (
        f"data.id mismatch:\n  Python: {py_row['id']}\n  Rust:   {rust_row['id']}"
    )


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


def test_add_image_stored_bytes_match(tmp_path):
    """The PNG bytes stored on disk must be byte-for-byte identical in both SDKs."""
    py_ws, rust_ws, py_row, rust_row = _run_both_sdks(
        tmp_path, "photo.png", _PNG_BYTES, "image_parity"
    )
    py_stored = read_stored_file(py_ws / ".data_storage", py_row["raw_data_location"])
    rust_stored = read_stored_file(rust_ws / ".data_storage", rust_row["raw_data_location"])

    assert py_stored == rust_stored == _PNG_BYTES, (
        f"Stored PNG bytes differ.\n"
        f"  Python size: {len(py_stored)}, Rust size: {len(rust_stored)}, "
        f"Expected: {len(_PNG_BYTES)}"
    )


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


def test_add_audio_data_id_matches(tmp_path):
    """With synced user_id + tenant_id, audio data.id must be identical (UUID5)."""
    _, _, py_row, rust_row = _run_both_sdks(
        tmp_path, "speech.wav", _WAV_BYTES, "audio_parity"
    )
    assert py_row["id"] == rust_row["id"], (
        f"data.id mismatch:\n  Python: {py_row['id']}\n  Rust:   {rust_row['id']}"
    )


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


def test_add_audio_stored_bytes_match(tmp_path):
    """The WAV bytes stored on disk must be byte-for-byte identical in both SDKs."""
    py_ws, rust_ws, py_row, rust_row = _run_both_sdks(
        tmp_path, "speech.wav", _WAV_BYTES, "audio_parity"
    )
    py_stored = read_stored_file(py_ws / ".data_storage", py_row["raw_data_location"])
    rust_stored = read_stored_file(rust_ws / ".data_storage", rust_row["raw_data_location"])

    assert py_stored == rust_stored == _WAV_BYTES, (
        f"Stored WAV bytes differ.\n"
        f"  Python size: {len(py_stored)}, Rust size: {len(rust_stored)}, "
        f"Expected: {len(_WAV_BYTES)}"
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
