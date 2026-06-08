# Implementation Plan: Image & Audio Document Loaders

Status: in progress (T1 done)
Scope: close the only remaining **extraction** gap between the Rust and Python
cognee SDKs — image and audio documents. Both are already *classified* correctly
in Rust; they fail only at the extraction step because no loader is wired in.

---

## 0. Decision log

Decisions are resolved one at a time before implementation. Resolved entries are
authoritative; the prose sections below reflect them.

| # | Decision | Resolution |
|---|---|---|
| D1 | How loaders reach the LLM/transcriber | **Loader holds the handle** — `ImageLoader { llm }`, `AudioLoader { transcriber }`; `DocumentLoader::extract` signature unchanged; handles injected at registry-build time. |
| D2 | Registry construction shape | **Register directly in `build_cognify_pipeline`** — keep `default_registry()` LLM-free; register image/audio at the one site that holds the handles. No `with_llm` constructor, no builder. |
| D3 | How the audio `Transcriber` is threaded in | **`CognifyConfig.transcriber: Option<Arc<dyn Transcriber>>`** — set by callers alongside the LLM; `None` ⇒ audio stays gracefully unsupported. No signature change to `build_cognify_pipeline`. |
| D4 | Audio format whitelist mismatch | **Keep the existing whitelist unchanged; return `UnsupportedFormat` for everything outside it** (`flac`, `ogg`, `aac`, `mid`, `amr`, `aiff`). Smallest change; accepts that `flac`/`ogg` are rejected even though Whisper supports them (revisit later if needed). |
| D5 | Non-OpenAI audio fallback | **Whisper/OpenAI-only this pass.** Audio goes through the existing `Transcriber` trait (only OpenAI Whisper impl today). Non-OpenAI providers ⇒ `config.transcriber = None` ⇒ audio unsupported (graceful). The completion-based fallback is deferred; the trait already abstracts it. |
| D6 | Behaviour when the model lacks vision | **Fail fast with a clear error.** Map `FeatureNotSupported`/no-vision to `LoaderError::ExtractionFailed` naming the model; the run aborts via the existing `?` at [tasks.rs:241](crates/cognify/src/tasks.rs#L241). No new partial-failure/skip semantics. |
| D7 | Cross-SDK test strategy | **Real-key structural tolerance.** Run full cognify on both SDKs with real vision/Whisper keys; compare graph node/edge counts + node-type Jaccard within tolerance, like `test_cognify_structural`. Gated behind a key with vision+audio access; document the cost/flake. |
| D8 | Feature defaults in umbrella crates | **On by default.** Add `image-loader` + `audio-loader` to `cognee-lib` and `cognee-cli` `default` lists, per the feature-strategy convention. Registering a loader does not force LLM calls; it activates only when an image/audio doc is cognified. |
| D9 | Follow-up scope (`rtf`/`msg`/`doc`/`ppt`) | **Image + audio only this pass.** `rtf`/`msg` deferred to a separate follow-up (native crates); `doc`/`ppt` stay explicitly unsupported (`UnsupportedFormat`). |

---

## 1. Goal

Make `cognify` extract text from image and audio files end-to-end, matching the
Python SDK behaviour:

- **Image** → a text description produced by an LLM vision call
  (Python: `LLMGateway.transcribe_image()` → "What's in this image?", `max_tokens=300`).
- **Audio** → a transcript produced by a speech-to-text model
  (Python: `LLMGateway.create_transcript()` → OpenAI Whisper for OpenAI, vision/file
  fallback for other providers). **Per D5, this pass implements only the OpenAI
  Whisper path**; the non-OpenAI completion fallback is deferred.

The extracted text then flows through the *same* paragraph chunker as text/PDF
documents, so no downstream pipeline change is required.

---

## 2. Current state — what already exists

This gap is far smaller than it looks: the model-integration layer (the genuinely
hard part) is already implemented and tested. Only the loader-wiring layer is missing.

### Already implemented ✅

| Capability | Location |
|---|---|
| Vision trait method `Llm::transcribe_image(bytes, mime_type, options)` | [crates/llm/src/llm_trait.rs:91](crates/llm/src/llm_trait.rs#L91) |
| `Llm::supports_vision()` heuristic | [crates/llm/src/llm_trait.rs:111](crates/llm/src/llm_trait.rs#L111) |
| Concrete vision impl (base64 + `image_url` multimodal message, `max_tokens=300`) | [crates/llm/src/adapters/openai.rs:653](crates/llm/src/adapters/openai.rs#L653) |
| `Transcriber` trait (`transcribe_audio(audio, format, lang, prompt)`) | [crates/llm/src/transcriber.rs:42](crates/llm/src/transcriber.rs#L42) |
| `validate_audio_format()` + `TranscriptionOutput` | [crates/llm/src/transcriber.rs:17](crates/llm/src/transcriber.rs#L17) |
| Concrete Whisper impl (`POST /v1/audio/transcriptions`, `verbose_json`, retry) | [crates/llm/src/adapters/openai.rs:846](crates/llm/src/adapters/openai.rs#L846) |
| `MockTranscriber` for deterministic tests | [crates/test-utils/src/mock_transcriber.rs](crates/test-utils/src/mock_transcriber.rs) |
| Classification of image/audio extensions → `document_type` | [crates/models/src/document.rs:36-49](crates/models/src/document.rs#L36-L49) |
| Loader-engine names `image_loader` / `audio_loader` (cross-SDK metadata) | [crates/ingestion/src/loaders/loader_registry.rs:4](crates/ingestion/src/loaders/loader_registry.rs#L4) |
| `DocumentLoader` trait + `LoaderRegistry` + `LoaderOutput::Text` | [crates/ingestion/src/loaders/mod.rs](crates/ingestion/src/loaders/mod.rs) |
| Extraction task with loader dispatch (the `UnsupportedDocumentType` site) | [crates/cognify/src/tasks.rs:233-241](crates/cognify/src/tasks.rs#L233-L241) |
| `Arc<dyn Llm>` available where the registry is built | [crates/cognify/src/tasks.rs:3290-3295](crates/cognify/src/tasks.rs#L3290-L3295) |

### The gap ❌

1. No `image` / `audio` loader structs in `crates/ingestion/src/loaders/`.
2. The `ingestion` crate does **not** depend on `cognee-llm`, and the
   `DocumentLoader::extract(&self, bytes, doc)` signature has no LLM handle.
3. `LoaderRegistry::default()` registers neither `image` nor `audio`
   ([mod.rs:135-149](crates/ingestion/src/loaders/mod.rs#L135-L149)).
4. The pipeline builder constructs the registry with no LLM/transcriber
   ([tasks.rs:3295](crates/cognify/src/tasks.rs#L3295)).
5. Result: a classified image/audio document hits
   `CognifyError::UnsupportedDocumentType` at
   [tasks.rs:236](crates/cognify/src/tasks.rs#L236).

---

## 3. Key design decision: how the loader reaches the LLM

The current `DocumentLoader` trait has no LLM in scope. Two viable approaches:

- **(A) Loader holds the handle** — `ImageLoader { llm: Arc<dyn Llm> }`,
  `AudioLoader { transcriber: Arc<dyn Transcriber> }`. Registered into the
  registry at build time. The `extract()` signature is unchanged.
- (B) Add an `LlmContext` parameter to `extract()`. Touches every existing loader
  and `LoaderOutput` call site for no benefit.

**Decided (D1): approach (A).** It is local, keeps the trait stable, and matches
how the rest of the pipeline already passes `Arc<dyn Llm>` around. The registry
simply gains an optional LLM/transcriber at construction time.

### Crate-dependency note

`cognee-ingestion` must gain a dependency on `cognee-llm`. This is acyclic
(`llm` does not depend on `ingestion`) and the new loaders are feature-gated, so
the dependency is only compiled when `image-loader` / `audio-loader` is enabled.
The dependency is declared `optional = true` and pulled in by those features.

---

## 4. Implementation steps

### Step 1 — `ingestion` crate: dependency + features

`crates/ingestion/Cargo.toml`:

```toml
[dependencies]
cognee-llm = { path = "../llm", optional = true }
base64 = { workspace = true, optional = true }   # only if mime detection needs it; transcribe_image takes raw bytes

[features]
image-loader = ["dep:cognee-llm"]
audio-loader = ["dep:cognee-llm"]
```

(No new third-party deps — encoding lives inside `cognee-llm` already.)

### Step 2 — Image loader

New file `crates/ingestion/src/loaders/image.rs`:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use cognee_llm::Llm;
use cognee_models::Document;
use crate::loaders::{DocumentLoader, LoaderError, LoaderOutput};

/// Extracts text from an image by asking a vision-capable LLM to describe it.
/// Mirrors Python `ImageDocument.transcribe_image()`.
pub struct ImageLoader {
    llm: Arc<dyn Llm>,
}

impl ImageLoader {
    pub fn new(llm: Arc<dyn Llm>) -> Self { Self { llm } }
}

#[async_trait]
impl DocumentLoader for ImageLoader {
    async fn extract(&self, bytes: &[u8], doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let mime = image_mime_type(doc);   // from doc.mime_type, fallback by extension
        let description = self
            .llm
            .transcribe_image(bytes, &mime, None)   // None → Python defaults (max_tokens=300)
            .await
            .map_err(|e| LoaderError::ExtractionFailed(e.to_string()))?;
        Ok(LoaderOutput::Text(description))
    }

    fn engine_name(&self) -> &'static str { "image_loader" }
}
```

Notes:
- Output is `LoaderOutput::Text` (not `SingleChunk`) so the description is chunked
  by the standard paragraph chunker — matching Python, where the transcription is
  yielded into the normal `Chunker`.
- `image_mime_type()` should prefer `doc.mime_type` and fall back to an
  extension→`image/*` map (the classifier already stores both). The vision API
  requires a `image/` MIME prefix.

### Step 3 — Audio loader

New file `crates/ingestion/src/loaders/audio.rs`:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use cognee_llm::transcriber::Transcriber;
use cognee_models::Document;
use crate::loaders::{DocumentLoader, LoaderError, LoaderOutput};

/// Transcribes audio to text via a Transcriber backend (OpenAI Whisper).
/// Mirrors Python `AudioDocument.create_transcript()`.
pub struct AudioLoader {
    transcriber: Arc<dyn Transcriber>,
}

impl AudioLoader {
    pub fn new(transcriber: Arc<dyn Transcriber>) -> Self { Self { transcriber } }
}

#[async_trait]
impl DocumentLoader for AudioLoader {
    async fn extract(&self, bytes: &[u8], doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let format = audio_format(doc);   // extension without dot: "mp3", "wav", ...
        // D4: reject formats outside the Whisper whitelist with a precise error
        // rather than a raw API 400.
        validate_audio_format(&format)
            .map_err(|_| LoaderError::UnsupportedFormat(format!("audio format '{format}'")))?;
        let out = self
            .transcriber
            .transcribe_audio(bytes, &format, None, None)
            .await
            .map_err(|e| LoaderError::ExtractionFailed(e.to_string()))?;
        Ok(LoaderOutput::Text(out.text))
    }

    fn engine_name(&self) -> &'static str { "audio_loader" }
}
```

### Step 4 — `mod.rs`: declare modules and extend the registry

In [crates/ingestion/src/loaders/mod.rs](crates/ingestion/src/loaders/mod.rs):

```rust
#[cfg(feature = "image-loader")]
pub mod image;
#[cfg(feature = "audio-loader")]
pub mod audio;
```

**Decided (D2): no LLM-aware constructor or builder.** `LoaderRegistry` stays a
dumb `document_type → loader` map. `default_registry()` remains LLM-free and keeps
serving the LLM-free callers (tests, CSV/text/PDF-only paths). The image/audio
loaders are registered directly at the one site that owns the handles —
`build_cognify_pipeline` — via the existing `register()` method (see Step 5). No
`with_llm()` constructor and no `LoaderRegistryBuilder` are introduced; this
avoids `#[cfg]`-on-parameter awkwardness and keeps the registry crate free of any
construction-time LLM concept.

### Step 5 — Pipeline wiring

In `build_cognify_pipeline` ([tasks.rs:3285-3295](crates/cognify/src/tasks.rs#L3285-L3295)),
replace the bare `LoaderRegistry::default()` with registration of the image/audio
loaders using the `llm` already in scope:

```rust
let mut registry = LoaderRegistry::default_registry();
#[cfg(feature = "image-loader")]
registry.register("image", Arc::new(ImageLoader::new(Arc::clone(&llm))));
#[cfg(feature = "audio-loader")]
if let Some(transcriber) = config.transcriber.clone() {
    registry.register("audio", Arc::new(AudioLoader::new(transcriber)));
}
let loader_registry = Arc::new(registry);
```

- **Image** can reuse the existing `Arc<dyn Llm>` directly — no new pipeline arg.
- **Audio** needs an `Arc<dyn Transcriber>`. `OpenAIAdapter` implements both
  `Llm` and `Transcriber`, but the pipeline only holds `Arc<dyn Llm>` and Rust
  cannot recover the `Transcriber` impl from a `dyn Llm`.

**Decided (D3): carry the transcriber on `CognifyConfig`.** Add
`transcriber: Option<Arc<dyn Transcriber>>` to `CognifyConfig`. Callers that build
an `OpenAIAdapter` set both the LLM and the transcriber to the same `Arc`
(`config.transcriber = Some(adapter.clone())`). When `None`, audio stays
unsupported (graceful — same as today). This keeps `build_cognify_pipeline`'s
signature unchanged, so the many call sites (`cognee-lib`, CLI, HTTP server) are
untouched. `CognifyConfig` therefore gains a non-`Clone`-trivial field — confirm
its `Clone`/`Debug` derives still hold (`Arc<dyn Transcriber>` is `Clone`; add a
manual `Debug` or `#[debug(skip)]`-style handling if the struct derives `Debug`).

### Step 6 — Feature propagation (project convention)

**Decided (D8): on by default.** Per the feature strategy in CLAUDE.md, propagate
the new features up and add them to the default feature lists of the umbrella
crates (they are not platform- or test-specific):

- `crates/lib/Cargo.toml` — add `image-loader`, `audio-loader` to `cognee-ingestion`'s
  enabled features and to `cognee-lib`'s `default`.
- `crates/cli/Cargo.toml` — same, so a plain `cargo build` gives a fully-featured CLI.

Registering the loaders does not trigger any LLM/Whisper traffic on its own — the
calls happen only when an image/audio document is actually cognified, so defaulting
the features on carries no cost for text-only workloads.

---

## 5. Parity / correctness details to get right

1. **Output is chunked, not single-chunk.** Python feeds the transcription/description
   into the normal chunker. Use `LoaderOutput::Text` so paragraph chunking +
   `cut_type` behaviour matches text documents. Do **not** use `SingleChunk`.

2. **`engine_name` must be `"image_loader"` / `"audio_loader"`** to match the
   `loader_engine` metadata column the cross-SDK tests compare
   ([loader_registry.rs:4](crates/ingestion/src/loaders/loader_registry.rs#L4)).

3. **Audio format whitelist mismatch — handled per D4.** Classification accepts
   `aac, mid, mp3, m4a, ogg, flac, wav, amr, aiff`
   ([document.rs](crates/models/src/document.rs#L36)), but
   `validate_audio_format()` only allows `mp3, mp4, mpeg, mpga, m4a, wav, webm`
   ([transcriber.rs:11](crates/llm/src/transcriber.rs#L11)).
   **Decided (D4): leave the whitelist unchanged.** `AudioLoader` calls
   `validate_audio_format()` up front and, for any format outside the whitelist
   (`flac`, `ogg`, `aac`, `mid`, `amr`, `aiff`), returns a precise
   `LoaderError::UnsupportedFormat` naming the format — never a raw API 400.
   This knowingly rejects `flac`/`ogg` even though OpenAI Whisper accepts them;
   that trade-off is accepted for the smallest change and can be revisited by
   widening the whitelist later (it lives in `cognee-llm`, not the loader).

4. **Vision MIME prefix.** `transcribe_image` requires a `image/` MIME type. The
   loader must derive a correct MIME from `doc.mime_type` or the extension; e.g.
   `jpg → image/jpeg`. Reuse `mime_guess` (already an ingestion dep).

5. **No-vision LLM — fail fast (D6).** If the configured model lacks vision, the
   loader gets `FeatureNotSupported` (or the API rejects the request). The loader
   maps this to `LoaderError::ExtractionFailed` with a message naming the model
   and stating vision is unsupported. The error propagates through the existing
   `?` at [tasks.rs:241](crates/cognify/src/tasks.rs#L241) and aborts the cognify
   run — there is **no** skip/partial-success path (the pipeline is all-or-nothing
   today, and this keeps a misconfigured model loud rather than silently dropping
   image content). Note `supports_vision()` is a best-effort heuristic and is
   **not** used to gate loader registration (false negatives exist); the real
   signal is the API/`transcribe_image` result.

6. **Cost / token notes.** Token write-back ([tasks.rs:276-290](crates/cognify/src/tasks.rs#L276-L290))
   already counts the *extracted* text — no change needed; image/audio just pay
   the upstream API call.

---

## 6. Testing plan

Use the existing mocks so tests stay deterministic and LLM-free.

- **Unit — image loader** (`crates/ingestion/tests/`, feature `image-loader`):
  `ImageLoader::new(Arc::new(MockLlm::with_response("a cat")))`; assert
  `extract()` returns `LoaderOutput::Text("a cat")` and `engine_name() == "image_loader"`.
  Add a `MockLlm` whose `transcribe_image` returns a canned string (extend the
  existing `MockLlm` in test-utils if it doesn't already override it).
- **Unit — audio loader**: `AudioLoader::new(Arc::new(MockTranscriber::new(vec![...])))`
  ([mock_transcriber.rs](crates/test-utils/src/mock_transcriber.rs)); assert
  transcript text flows into `LoaderOutput::Text`.
- **Unit — format errors**: per D4, `flac`/`ogg`/`aac`/`mid`/`amr`/`aiff` audio →
  expected `UnsupportedFormat`; non-image MIME → error.
- **Registry**: with features on + handles provided, `registry.get("image")` and
  `registry.get("audio")` are `Some` with the right engine names.
- **Cognify integration** (mock LLM/transcriber): a classified image document
  produces ≥1 `DocumentChunk` instead of `UnsupportedDocumentType`. This is the
  regression test for the current stub error at
  [tasks.rs:236](crates/cognify/src/tasks.rs#L236) (mirror the existing
  `test_unsupported_document_type`).
- **Cross-SDK (`e2e-cross-sdk/`) — real-key structural tolerance (D7)**:
  image/audio extraction is LLM-dependent and **non-deterministic** — never assert
  exact text. Add image/audio cases to the `test_cognify_structural` suite: run
  full cognify on both SDKs and compare graph **node/edge counts within tolerance**
  and **node-type Jaccard ≥ threshold** (reuse the existing tolerances). Gate the
  cases behind an OpenAI key with **vision + Whisper** access; document the cost
  and expected flakiness so CI can skip them when the key is absent.

---

## 7. Out of scope (separate, lower priority)

**Decided (D9): this pass ships image + audio only.** The items below are the
remaining non-image/audio gaps from the support audit, explicitly deferred:

- **`rtf`, `msg`** unstructured formats — easy follow-up via native crates
  (`rtf-parser`, `msg-parser`) following the existing `eml`/`docx` sub-module
  pattern in [crates/ingestion/src/loaders/unstructured/](crates/ingestion/src/loaders/unstructured/).
- **`doc`, `ppt`** legacy binary OLE formats — no mature pure-Rust extractor;
  recommend leaving as the current explicit `UnsupportedFormat` error.
- **Non-OpenAI audio fallback (D5)** — Python's completion-based `file` transcription
  for non-OpenAI providers. Deferred; add a new `Transcriber` impl later — no
  `AudioLoader` change required since it depends only on `Arc<dyn Transcriber>`.
- **Python `docling_loader` / `advanced_pdf_loader`** — optional fallbacks over
  formats Rust already covers natively; not worth porting.

---

## 8. Effort estimate

| Task | Est. |
|---|---|
| `ingestion` dep + features | 0.25 d |
| Image loader + MIME helper + unit tests | 0.5 d |
| Audio loader + format handling + unit tests | 0.5 d |
| Registry + pipeline wiring + `CognifyConfig.transcriber` | 0.5 d |
| Feature propagation (`lib`, `cli`) + `scripts/check_all.sh` | 0.25 d |
| Cognify integration test + cross-SDK doc | 0.5 d |
| **Total** | **~2.5 days** |

The bulk is plumbing and tests; the model integrations (vision + Whisper) are
already done.

---

## 9. Checklist

- [x] `cognee-ingestion` gains optional `cognee-llm` dep + `image-loader`/`audio-loader` features
- [x] `image.rs` loader (`transcribe_image`, `image_loader` engine name, `Text` output)
- [ ] `audio.rs` loader (`Transcriber`, `audio_loader` engine name, format handling)
- [ ] Registry registers `image`/`audio` when handles + features present
- [ ] `CognifyConfig.transcriber: Option<Arc<dyn Transcriber>>` + wiring in `build_cognify_pipeline`
- [ ] Audio format whitelist decision documented and enforced
- [ ] Unit + integration tests (mock LLM / `MockTranscriber`)
- [ ] Features added to `cognee-lib` / `cognee-cli` defaults
- [ ] `scripts/check_all.sh` green (fmt, check, clippy -D warnings, binding checks)
