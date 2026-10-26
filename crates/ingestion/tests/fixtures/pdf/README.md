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

## Encrypted copies: `sample.encrypted-*.pdf`

`sample.encrypted-rc4-128.pdf`, `sample.encrypted-aes-128.pdf` and
`sample.encrypted-aes-256.pdf` are `sample.pdf` encrypted with an **empty user
password** and the owner password `cognee-owner`. That is the common
"protected" PDF: any reader opens it without prompting, but the content is
ciphertext on disk and every extractor has to decrypt it. They hold the same
pages, so the tests compare them against the same expected files. Both PDF
test files read them, through the list in `encrypted.rs`.

They exist because `pdf-extract` 0.10 could not read such a file: its `lopdf`
decrypted at load time but kept `/Encrypt` in the trailer, so `pdf-extract`
decrypted a second time (see `encrypted.rs`). With RC4 that second pass
re-encrypted the streams, every page came back empty and the loader reported
success; with AES it failed with `Decryption(InvalidCipherTextLength)`.

`sample.encrypted-user-password-rc4-128.pdf` is the opposite case: the user
password is `secret`, which the loader cannot supply, so extraction must fail
rather than yield empty text or ciphertext.

Unlike `sample.pdf` these are binary — their streams are encrypted (and
deflated, as real-world files are) — so review them by decrypting:
`mutool clean -d -D sample.encrypted-rc4-128.pdf out.pdf` should show the same
objects as `sample.pdf`. They are not byte-reproducible across runs, but only
because of the clock: MEASURED with MuPDF 1.23.10, two runs within the same
second give identical bytes, and runs in different seconds differ in the file
`/ID`, in the AES stream IVs, and for AES-256 in the salted `/O`, `/U` and
`/UE`. `/ID` feeds the RC4 and AES-128 key derivation, so every encrypted byte
changes with it, while their `/O` depends only on the passwords. That is why
the three empty-user-password files share one `/ID` (they were written in the
same second), and why the RC4-128 and AES-128 files also share `/O` and `/U`:
revisions 3 and 4 of the standard handler compute both the same way.
Regenerate all of them together rather than diffing against the old bytes.

`mutool clean -E` writes `/Encrypt` as an inline dictionary in the trailer.
Real-world writers put it in an indirect object, and `lopdf` only recognises
that form (`Document::get_encrypted` requires a reference), so a mutool file
used as is would not exercise decryption at all. The script below moves the
dictionary into a new object, leaving every other byte of the body in place.
With MuPDF 1.23.10:

```sh
for alg in rc4-128 aes-128 aes-256; do
    mutool clean -z -E "$alg" -O cognee-owner -U "" -P -4 sample.pdf "/tmp/$alg.pdf"
    python3 indirect_encrypt.py "/tmp/$alg.pdf" > "sample.encrypted-$alg.pdf"
done
mutool clean -z -E rc4-128 -O cognee-owner -U secret -P -4 sample.pdf /tmp/user.pdf
python3 indirect_encrypt.py /tmp/user.pdf > sample.encrypted-user-password-rc4-128.pdf
```

```python
"""Moves a `mutool clean -E` output's inline trailer /Encrypt into an object.

Usage: indirect_encrypt.py in.pdf > out.pdf
"""
import re
import sys

src = open(sys.argv[1], "rb").read()

# The body (every object, all byte offsets) is kept as is; only the final
# xref section and trailer are rewritten, with one object added before them.
xref_at = int(re.search(rb"startxref\s+(\d+)\s+%%EOF\s*$", src).group(1))
body, tail = src[:xref_at], src[xref_at:]
entries = re.search(rb"xref\s+0 (\d+)\s+(.*?)trailer", tail, re.S)
size = int(entries.group(1))
rows = entries.group(2).split(b"\n")
rows = [r.rstrip(b"\r") for r in rows if r.strip()]
assert len(rows) == size, (len(rows), size)

trailer = tail[tail.index(b"trailer") + len(b"trailer"):tail.index(b"startxref")].strip()
start = trailer.index(b"/Encrypt") + len(b"/Encrypt")
# Walk the inline dictionary: `<<`/`>>` nest, `<...>` is a hex string.
i, depth = start, 0
while True:
    if trailer[i:i + 2] == b"<<":
        depth, i = depth + 1, i + 2
    elif trailer[i:i + 2] == b">>":
        depth, i = depth - 1, i + 2
        if depth == 0:
            break
    elif trailer[i:i + 1] == b"<":
        i = trailer.index(b">", i) + 1
    else:
        i += 1
encrypt = trailer[start:i].strip()
trailer = trailer[:start] + b" %d 0 R" % size + trailer[i:]
trailer = re.sub(rb"/Size \d+", b"/Size %d" % (size + 1), trailer)

out = bytearray(body)
encrypt_at = len(out)
out += b"%d 0 obj\n" % size + encrypt + b"\nendobj\n"
new_xref = len(out)
out += b"xref\n0 %d\n" % (size + 1)
for row in rows:
    assert len(row.rstrip()) == 18, row
    out += row.rstrip() + b" \n"
out += b"%010d 00000 n \n" % encrypt_at
out += b"trailer\n" + trailer + b"\nstartxref\n%d\n%%%%EOF\n" % new_xref
sys.stdout.buffer.write(bytes(out))
```
