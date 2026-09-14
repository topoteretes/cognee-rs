# PDF test fixture

`sample.pdf` is the repository's only real PDF. Every other PDF in the test
suite is a synthesized `%PDF-1.x` stub that no extractor ever reads past, which
is why nothing caught a build shipping with no PDF backend registered at all
(see [issue #212](https://github.com/topoteretes/cognee-rs/issues/212) item A1).

## Provenance and licence

Authored in this repository for this purpose. It embeds no font (base-14
`/Helvetica` only), no image, and no third-party text, so there is nothing in it
to license and nothing to attribute. It is covered by the repository's own
licence like any other source file.

The content streams are **uncompressed**, so the file is plain ASCII and its
text is readable — and reviewable — straight out of `git diff`. Keep it that
way: a compressed or binary fixture would be unauditable in review, and the
whole point of this file is that a human can confirm what a test claims was
extracted from it.

`sample.expected.txt` is the loader's output for it, with the page headers that
[`loaders::pdf::format::format_pages`] produces to match Python's
`pypdf_loader.py`. It is compared after normalising CRLF to LF: PDFium reports
intra-page line breaks as `\r\n` while the page separators come from
`format_pages` as `\n`, and committing a mixed-ending text file invites an
editor or a `.gitattributes` rule to silently rewrite it.

## Regenerating

Hand-editing is not safe — the `xref` table holds byte offsets, so any edit to
the objects above it invalidates the file. Regenerate instead:

```python
#!/usr/bin/env python3
"""Writes crates/ingestion/tests/fixtures/pdf/sample.pdf to stdout."""
import sys

PAGE1 = [
    "Cognee PDF loader fixture.",
    "This page exists so the test suite drives a real PDF parser",
    "over real PDF bytes instead of a synthesized %PDF-1.x stub.",
]
PAGE2 = [
    "Second page.",
    "Two pages are needed to pin the page-header output format,",
    "which must match the Python pypdf_loader.",
]

def content_stream(lines):
    out = ["BT", "/F1 12 Tf", "72 720 Td", "14 TL"]
    for i, line in enumerate(lines):
        if i:
            out.append("T*")
        escaped = line.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
        out.append("({}) Tj".format(escaped))
    out.append("ET")
    return ("\n".join(out) + "\n").encode("ascii")

objs = {
    1: b"<< /Type /Catalog /Pages 2 0 R >>",
    2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
    3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
       b"/Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>",
    4: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
       b"/Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>",
    7: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica "
       b"/Encoding /WinAnsiEncoding >>",
}
for num, lines in ((5, PAGE1), (6, PAGE2)):
    s = content_stream(lines)
    # PDF 32000-1 §7.3.8.1: the EOL before `endstream` is not stream data,
    # so /Length excludes the trailing newline `s` ends with.
    objs[num] = b"<< /Length %d >>\nstream\n" % (len(s) - 1) + s + b"endstream"

buf = bytearray(b"%PDF-1.4\n")
offsets = {}
for num in sorted(objs):
    offsets[num] = len(buf)
    buf += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"
xref_at = len(buf)
n = len(objs) + 1
buf += b"xref\n0 %d\n" % n + b"0000000000 65535 f \n"
for num in sorted(objs):
    buf += b"%010d 00000 n \n" % offsets[num]
buf += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n, xref_at)
sys.stdout.buffer.write(bytes(buf))
```

If you change `PAGE1` or `PAGE2`, **two** snapshots go stale, not one — update
both `sample.expected.txt` (PDFium) and `sample.expected.pure-rust.txt`
(`pdf-extract`). They hold the same page text in different shapes, so neither
can be derived from the other by hand; regenerate each from its own backend:

```sh
cargo test -p cognee-ingestion --features pdf-pdfium \
    --test pdf_fixture_extraction -- --nocapture
cargo test -p cognee-ingestion --features pdf-pure-rust \
    --test pdf_pure_rust_fixture_extraction -- --nocapture
```

Each failure prints the mismatch, and the module docs on `loaders::pdf` record
how the two backends differ.
