//! The pluggable [`Chunker`] trait and the built-in chunkers.
//!
//! Port of Python's `cognee.modules.chunking.Chunker` hierarchy. Every chunker
//! declares a stable [`Chunker::chunker_id`] that is stamped on each chunk it
//! produces (`DocumentChunk.chunker_id`), so a document can later be told apart
//! by the strategy that cut it — chunkers disagree on boundaries, and an
//! overlapping chunker's output does not tile its input.
//!
//! | Rust type | Python class | `chunker_id` |
//! |---|---|---|
//! | [`TextChunker`] | `TextChunker` | `text_chunker_v1` |
//! | [`RecursiveChunker`] | `LangchainChunker` | `langchain_chunker_v1` |
//! | [`CsvChunker`] | `CsvChunker` | `csv_chunker_v1` |
//! | [`JsonListChunker`] | `JsonListChunker` | `json_list_chunker_v1` |
//! | [`TextChunkerWithOverlap`] | `TextChunkerWithOverlap` | `text_chunker_overlap_v1` |
//! | [`FnChunker`] | a user `Chunker` subclass | caller-chosen (default `custom_chunker`) |
//!
//! # Chunk ids
//!
//! Every built-in chunker derives a chunk's id as
//! `uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}")`, the convention the
//! Rust pipeline has always used, except where the chunker it wraps already
//! picks one (`chunk_text`'s single-paragraph chunks, `chunk_by_row`, and the
//! overlap chunker's lone over-budget piece, which keeps the paragraph
//! splitter's `uuid5(OID, text)` exactly as Python's `TextChunkerWithOverlap`
//! does). Python's
//! `LangchainChunker` ids a chunk by its text alone (`uuid5(OID, chunk)`), which
//! merges equal chunks of *different* documents into one node; that is not
//! reproduced. Content-addressed ids are SDK-1060's subject.

use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use cognee_models::DocumentChunk;

use crate::chunk_by_paragraph::chunk_by_paragraph;
use crate::chunk_by_row::chunk_by_row;
use crate::error::ChunkingError;
use crate::recursive_splitter::{RecursiveCharacterSplitter, word_length};
use crate::text_chunker::{NAMESPACE_OID, chunk_text};
use crate::token_counter::{TokenCountMode, TokenCounter};

/// `chunker_id` of [`TextChunker`].
pub const TEXT_CHUNKER_ID: &str = "text_chunker_v1";
/// `chunker_id` of [`RecursiveChunker`] (Python's `LangchainChunker`).
pub const LANGCHAIN_CHUNKER_ID: &str = "langchain_chunker_v1";
/// `chunker_id` of [`CsvChunker`].
pub const CSV_CHUNKER_ID: &str = "csv_chunker_v1";
/// `chunker_id` of [`JsonListChunker`].
pub const JSON_LIST_CHUNKER_ID: &str = "json_list_chunker_v1";
/// `chunker_id` of [`TextChunkerWithOverlap`].
pub const TEXT_CHUNKER_OVERLAP_ID: &str = "text_chunker_overlap_v1";
/// Default `chunker_id` of an [`FnChunker`].
pub const CUSTOM_CHUNKER_ID: &str = "custom_chunker";

/// What a [`Chunker`] needs to know about the document it is cutting.
#[derive(Clone, Copy)]
pub struct ChunkContext<'a> {
    /// Id of the document the chunks belong to.
    pub document_id: Uuid,
    /// Token budget of one chunk.
    pub max_chunk_size: usize,
    /// Counts the tokens of a chunk.
    pub counter: &'a dyn TokenCounter,
    /// How sizes are measured; see [`TokenCountMode`].
    pub mode: TokenCountMode,
}

impl<'a> ChunkContext<'a> {
    /// A context with the default [`TokenCountMode`].
    pub fn new(document_id: Uuid, max_chunk_size: usize, counter: &'a dyn TokenCounter) -> Self {
        Self {
            document_id,
            max_chunk_size,
            counter,
            mode: TokenCountMode::default(),
        }
    }

    /// Set the token count mode.
    pub fn with_mode(mut self, mode: TokenCountMode) -> Self {
        self.mode = mode;
        self
    }

    /// `uuid5(NAMESPACE_OID, "{document_id}-{index}")`.
    pub fn positional_chunk_id(&self, index: usize) -> Uuid {
        Uuid::new_v5(
            &NAMESPACE_OID,
            format!("{}-{index}", self.document_id).as_bytes(),
        )
    }
}

/// A chunking strategy: turns a document's text into [`DocumentChunk`]s.
///
/// Mirrors Python's `Chunker` base class. Implementations produce chunks with
/// sequential `chunk_index` from 0; callers should go through
/// [`Chunker::chunk_document`], which stamps [`Chunker::chunker_id`] on each.
pub trait Chunker: Send + Sync {
    /// Stable identity of this strategy, stamped on every chunk it produces.
    /// Empty means "not declared" (Python's default `chunker_id = ""`), and
    /// leaves `DocumentChunk.chunker_id` unset.
    fn chunker_id(&self) -> &str;

    /// Split `text` into chunks. Need not set `chunker_id`.
    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError>;

    /// [`Chunker::chunk`], then stamp [`Chunker::chunker_id`] on every chunk.
    fn chunk_document(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        let mut chunks = self.chunk(text, ctx)?;
        let id = self.chunker_id();
        if !id.is_empty() {
            for chunk in &mut chunks {
                chunk.chunker_id = Some(id.to_owned());
            }
        }
        Ok(chunks)
    }
}

// ---------------------------------------------------------------------------
// TextChunker
// ---------------------------------------------------------------------------

/// The default paragraph chunker: [`chunk_text`] behind the [`Chunker`] trait.
///
/// Python's `TextChunker`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextChunker;

impl Chunker for TextChunker {
    fn chunker_id(&self) -> &str {
        TEXT_CHUNKER_ID
    }

    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        Ok(chunk_text(
            ctx.document_id,
            text,
            ctx.max_chunk_size,
            &ctx.counter,
            ctx.mode,
        ))
    }
}

// ---------------------------------------------------------------------------
// RecursiveChunker (LangchainChunker)
// ---------------------------------------------------------------------------

/// Recursive character splitting, Python's `LangchainChunker`.
///
/// Splits with a port of langchain's `RecursiveCharacterTextSplitter`
/// (see [`crate::recursive_splitter`]) measuring length in **words**
/// (`len(text.split())`), `chunk_size` words per chunk with `chunk_overlap`
/// words carried between consecutive chunks. Python hard-codes 1024 / 10;
/// [`RecursiveChunker::default`] does the same.
///
/// Each chunk is then measured with the context's token counter and, as in
/// Python, a chunk over `max_chunk_size` tokens is an error rather than being
/// silently truncated or re-split. Chunks carry `cut_type = "missing"`, as in
/// Python.
#[derive(Debug, Clone, Copy)]
pub struct RecursiveChunker {
    chunk_size: usize,
    chunk_overlap: usize,
}

impl RecursiveChunker {
    /// Python's `LangchainChunker` default splitter `chunk_size`, in words.
    pub const DEFAULT_CHUNK_SIZE: usize = 1024;
    /// Python's `LangchainChunker` default `chunk_overlap`, in words.
    pub const DEFAULT_CHUNK_OVERLAP: usize = 10;

    /// A chunker with an explicit splitter size and overlap (both in words).
    /// Rejects what langchain rejects: `chunk_size == 0` or
    /// `chunk_overlap > chunk_size`.
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Result<Self, ChunkingError> {
        // Validate once up front, with langchain's own messages.
        RecursiveCharacterSplitter::new(chunk_size, chunk_overlap, word_length)?;
        Ok(Self {
            chunk_size,
            chunk_overlap,
        })
    }

    /// Splitter size in words.
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Overlap in words.
    pub fn chunk_overlap(&self) -> usize {
        self.chunk_overlap
    }
}

impl Default for RecursiveChunker {
    fn default() -> Self {
        Self {
            chunk_size: Self::DEFAULT_CHUNK_SIZE,
            chunk_overlap: Self::DEFAULT_CHUNK_OVERLAP,
        }
    }
}

impl Chunker for RecursiveChunker {
    fn chunker_id(&self) -> &str {
        LANGCHAIN_CHUNKER_ID
    }

    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        let splitter =
            RecursiveCharacterSplitter::new(self.chunk_size, self.chunk_overlap, word_length)?;
        splitter
            .split_text(text)
            .into_iter()
            .enumerate()
            .map(|(index, piece)| {
                let token_count = ctx.counter.count_tokens(&piece);
                if token_count > ctx.max_chunk_size {
                    return Err(ChunkingError::ChunkTooLarge {
                        size: token_count,
                        max: ctx.max_chunk_size,
                        hint: "reduce the recursive splitter's chunk_size",
                    });
                }
                Ok(DocumentChunk::new(
                    ctx.positional_chunk_id(index),
                    piece,
                    token_count,
                    index,
                    "missing".to_owned(),
                    ctx.document_id,
                ))
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// CsvChunker
// ---------------------------------------------------------------------------

/// Row chunking, Python's `CsvChunker`: [`chunk_by_row`] behind the trait.
///
/// Expects rows separated by `"\n\n"` and `key: value` pairs separated by
/// `", "` (the CSV loader's output format).
///
/// Unlike Python, a single `key: value` pair larger than `max_chunk_size` is
/// emitted on its own rather than failing the document — the behaviour the
/// Rust CSV path had before chunkers were selectable, kept so the default CSV
/// path does not start failing files it used to accept.
#[derive(Debug, Clone, Copy, Default)]
pub struct CsvChunker;

impl Chunker for CsvChunker {
    fn chunker_id(&self) -> &str {
        CSV_CHUNKER_ID
    }

    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        let chunks = chunk_by_row(ctx.document_id, text, ctx.max_chunk_size, &ctx.counter);
        if let Some(over) = chunks.iter().find(|c| c.chunk_size > ctx.max_chunk_size) {
            tracing::warn!(
                document_id = %ctx.document_id,
                chunk_index = over.chunk_index,
                chunk_size = over.chunk_size,
                max_chunk_size = ctx.max_chunk_size,
                "CsvChunker emitted a row value larger than max_chunk_size"
            );
        }
        Ok(chunks)
    }
}

// ---------------------------------------------------------------------------
// JsonListChunker
// ---------------------------------------------------------------------------

/// One chunk per item of a JSON list, Python's `JsonListChunker`.
///
/// The document text must parse as a JSON array; anything else is an error.
/// Each item becomes one chunk whose text is Python's `str(item)` (a string
/// item verbatim, anything else in Python `repr` form — `{'a': 1}`, `True`,
/// `None`), sized in words (`len(text.split())`), with `cut_type =
/// "json_list_item"` and the item's position in `metadata.json_list_index`.
/// An item over `max_chunk_size` is kept whole and logged, as in Python.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonListChunker;

impl Chunker for JsonListChunker {
    fn chunker_id(&self) -> &str {
        JSON_LIST_CHUNKER_ID
    }

    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        let parsed: Value = serde_json::from_str(text)
            .map_err(|e| ChunkingError::InvalidInput(format!("invalid JSON: {e}")))?;
        let Value::Array(items) = parsed else {
            return Err(ChunkingError::InvalidInput(
                "JsonListChunker expects the document content to be a JSON list.".to_owned(),
            ));
        };

        let mut max_observed = 0usize;
        let mut chunks = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let text = python_str(item);
            let chunk_size = word_length(&text);
            max_observed = max_observed.max(chunk_size);
            if chunk_size > ctx.max_chunk_size {
                tracing::warn!(
                    document_id = %ctx.document_id,
                    chunk_index = index,
                    chunk_size,
                    max_chunk_size = ctx.max_chunk_size,
                    "JsonListChunker item exceeds max_chunk_size"
                );
            }
            let mut chunk = DocumentChunk::new(
                ctx.positional_chunk_id(index),
                text,
                chunk_size,
                index,
                "json_list_item".to_owned(),
                ctx.document_id,
            );
            chunk
                .base
                .set_metadata("json_list_index", Value::from(index));
            chunks.push(chunk);
        }
        if max_observed > ctx.max_chunk_size {
            tracing::warn!(
                document_id = %ctx.document_id,
                max_observed_chunk_size = max_observed,
                max_chunk_size = ctx.max_chunk_size,
                "JsonListChunker max item size exceeds max_chunk_size"
            );
        }
        Ok(chunks)
    }
}

/// Python's `str(value)` for a value parsed from JSON: a string is itself,
/// everything else is its `repr`.
fn python_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => python_repr(other),
    }
}

/// Python's `repr` of the object `json.loads` would build for `value`.
fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => python_number_repr(n),
        Value::String(s) => python_string_repr(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(python_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", python_string_repr(k), python_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// `repr` of a JSON number: an integer literal stays an `int`; anything with a
/// fraction or exponent is a `float`, printed the way Python prints floats
/// (shortest round-trip, `.0` on integral values, `e+NN`/`e-NN` exponents
/// outside `[1e-4, 1e16)`).
fn python_number_repr(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    let Some(f) = n.as_f64() else {
        return n.to_string();
    };
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let abs = f.abs();
    if abs == 0.0 || (1e-4..1e16).contains(&abs) {
        // Rust's Debug is the shortest round-trip repr and keeps a `.0` on
        // integral values, as Python does in this range.
        return format!("{f:?}");
    }
    // Scientific: Rust prints `1e20` / `1.5e-7`; Python prints `1e+20` /
    // `1.5e-07`.
    let formatted = format!("{f:e}");
    match formatted.split_once('e') {
        Some((mantissa, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => formatted,
    }
}

/// Python's `repr` of a `str`: single-quoted unless the string contains a
/// single quote and no double quote; backslash, the chosen quote, `\n`, `\r`,
/// `\t` and other ASCII control characters escaped.
fn python_string_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

// ---------------------------------------------------------------------------
// TextChunkerWithOverlap
// ---------------------------------------------------------------------------

/// Paragraph chunking with a ratio-based overlap, Python's
/// `TextChunkerWithOverlap`.
///
/// `chunk_overlap = floor(max_chunk_size * overlap_ratio)` tokens of the tail
/// of each emitted chunk are carried into the next one. With a ratio above 0,
/// the text is pre-cut into paragraphs of at most
/// `floor(0.5 * overlap_ratio * max_chunk_size)` tokens (so the overlap is
/// assembled from small pieces); with ratio 0 it cuts like [`TextChunker`]
/// (pieces of up to `max_chunk_size`, no carry-over). As in Python, a chunk's
/// `chunk_size` is the sum of its pieces' sizes, not a re-count of the joined
/// text, so under [`TokenCountMode::Span`] sizes can differ slightly from
/// [`TextChunker`]'s. No emitted chunk exceeds `max_chunk_size` unless a single
/// piece does on its own.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextChunkerWithOverlap {
    overlap_ratio: f64,
}

/// One pre-cut piece the overlap accumulator works on (Python's `chunk_data`).
#[derive(Debug, Clone)]
pub(crate) struct OverlapPiece {
    pub text: String,
    pub chunk_size: usize,
    pub cut_type: String,
    pub chunk_id: Uuid,
}

impl TextChunkerWithOverlap {
    /// A chunker with the given overlap ratio, which must be in `[0, 1)`.
    ///
    /// Python does not validate the ratio; a ratio of 1 or more keeps the whole
    /// accumulation on every emit, so chunks would grow without bound.
    pub fn new(overlap_ratio: f64) -> Result<Self, ChunkingError> {
        if !(0.0..1.0).contains(&overlap_ratio) {
            return Err(ChunkingError::InvalidConfig(format!(
                "chunk overlap ratio must be in [0, 1), got {overlap_ratio}"
            )));
        }
        Ok(Self { overlap_ratio })
    }

    /// The overlap ratio.
    pub fn overlap_ratio(&self) -> f64 {
        self.overlap_ratio
    }

    /// Overlap budget in tokens: `int(max_chunk_size * ratio)`.
    fn overlap_tokens(&self, max_chunk_size: usize) -> usize {
        (max_chunk_size as f64 * self.overlap_ratio) as usize
    }

    /// Max size of the pre-cut paragraphs.
    fn piece_max_size(&self, max_chunk_size: usize) -> usize {
        if self.overlap_ratio > 0.0 {
            // Python: int(0.5 * ratio * max_chunk_size). A 0 budget would make
            // the paragraph splitter emit word-per-chunk at best; floor at 1.
            ((0.5 * self.overlap_ratio * max_chunk_size as f64) as usize).max(1)
        } else {
            max_chunk_size
        }
    }

    /// Python's `read()` loop over already-cut pieces.
    pub(crate) fn accumulate(
        &self,
        pieces: impl IntoIterator<Item = OverlapPiece>,
        ctx: &ChunkContext<'_>,
    ) -> Vec<DocumentChunk> {
        let max = ctx.max_chunk_size;
        let overlap = self.overlap_tokens(max);
        let mut out = Vec::new();
        let mut acc: Vec<OverlapPiece> = Vec::new();
        let mut acc_size = 0usize;
        let mut chunk_index = 0usize;

        let from_acc = |acc: &[OverlapPiece], acc_size: usize, index: usize| {
            let text = acc
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let cut_type = acc.last().map(|p| p.cut_type.clone()).unwrap_or_default();
            DocumentChunk::new(
                ctx.positional_chunk_id(index),
                text,
                acc_size,
                index,
                cut_type,
                ctx.document_id,
            )
        };

        for piece in pieces {
            if acc_size + piece.chunk_size <= max {
                acc_size += piece.chunk_size;
                acc.push(piece);
                continue;
            }
            if acc.is_empty() {
                // A single piece over the budget is emitted on its own.
                out.push(DocumentChunk::new(
                    piece.chunk_id,
                    piece.text,
                    piece.chunk_size,
                    chunk_index,
                    piece.cut_type,
                    ctx.document_id,
                ));
            } else {
                out.push(from_acc(&acc, acc_size, chunk_index));
                // Keep the tail that fits in the overlap budget.
                let mut keep = 0usize;
                let mut kept_size = 0usize;
                for p in acc.iter().rev() {
                    if kept_size + p.chunk_size <= overlap {
                        kept_size += p.chunk_size;
                        keep += 1;
                    } else {
                        break;
                    }
                }
                // Then drop from the front of that tail until the incoming
                // piece fits, so no chunk exceeds `max`. Python skips this
                // check: a ratio above 2/3 (tail up to `ratio*max` plus a piece
                // up to `ratio*max/2`), or an over-budget piece arriving after
                // a non-empty accumulation, emits an over-budget chunk there.
                let mut start = acc.len() - keep;
                while start < acc.len() && kept_size + piece.chunk_size > max {
                    kept_size -= acc[start].chunk_size;
                    start += 1;
                }
                acc.drain(..start);
                acc_size = kept_size + piece.chunk_size;
                acc.push(piece);
            }
            chunk_index += 1;
        }
        if !acc.is_empty() {
            out.push(from_acc(&acc, acc_size, chunk_index));
        }
        out
    }
}

impl Chunker for TextChunkerWithOverlap {
    fn chunker_id(&self) -> &str {
        TEXT_CHUNKER_OVERLAP_ID
    }

    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        let pieces = chunk_by_paragraph(
            text,
            self.piece_max_size(ctx.max_chunk_size),
            true,
            &ctx.counter,
            ctx.mode,
        )
        .into_iter()
        .map(|p| OverlapPiece {
            text: p.text.to_owned(),
            chunk_size: p.chunk_size,
            cut_type: p.cut_type.to_string(),
            chunk_id: p.chunk_id,
        });
        Ok(self.accumulate(pieces, ctx))
    }
}

// ---------------------------------------------------------------------------
// FnChunker
// ---------------------------------------------------------------------------

/// Signature of a closure-based chunker: text and token budget in, chunk texts
/// out.
pub type ChunkFn = dyn Fn(&str, usize) -> Vec<String> + Send + Sync;

/// A [`Chunker`] built from a closure returning chunk texts.
///
/// Each returned string becomes one chunk, in order, sized with the context's
/// token counter and given `cut_type = "custom"`. Empty strings are skipped.
#[derive(Clone)]
pub struct FnChunker {
    id: String,
    f: Arc<ChunkFn>,
}

impl FnChunker {
    /// A chunker stamping [`CUSTOM_CHUNKER_ID`].
    pub fn new(f: Arc<ChunkFn>) -> Self {
        Self::with_id(CUSTOM_CHUNKER_ID, f)
    }

    /// A chunker stamping `id` (empty for none).
    pub fn with_id(id: impl Into<String>, f: Arc<ChunkFn>) -> Self {
        Self { id: id.into(), f }
    }
}

impl std::fmt::Debug for FnChunker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnChunker")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Chunker for FnChunker {
    fn chunker_id(&self) -> &str {
        &self.id
    }

    fn chunk(
        &self,
        text: &str,
        ctx: &ChunkContext<'_>,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        Ok((self.f)(text, ctx.max_chunk_size)
            .into_iter()
            .filter(|t| !t.is_empty())
            .enumerate()
            .map(|(index, piece)| {
                let size = ctx.counter.count_tokens(&piece);
                DocumentChunk::new(
                    ctx.positional_chunk_id(index),
                    piece,
                    size,
                    index,
                    "custom".to_owned(),
                    ctx.document_id,
                )
            })
            .collect())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::token_counter::WordCounter;

    fn ctx(max: usize) -> ChunkContext<'static> {
        ChunkContext::new(Uuid::from_u128(42), max, &WordCounter)
    }

    #[test]
    fn chunk_document_stamps_the_chunker_id() {
        let chunkers: Vec<(Box<dyn Chunker>, &str)> = vec![
            (Box::new(TextChunker), TEXT_CHUNKER_ID),
            (Box::new(RecursiveChunker::default()), LANGCHAIN_CHUNKER_ID),
            (Box::new(CsvChunker), CSV_CHUNKER_ID),
            (
                Box::new(TextChunkerWithOverlap::new(0.25).unwrap()),
                TEXT_CHUNKER_OVERLAP_ID,
            ),
            (
                Box::new(FnChunker::new(Arc::new(|t: &str, _| vec![t.to_owned()]))),
                CUSTOM_CHUNKER_ID,
            ),
        ];
        for (chunker, id) in chunkers {
            let chunks = chunker
                .chunk_document("alpha: one, beta: two", &ctx(100))
                .unwrap();
            assert!(!chunks.is_empty(), "{id} produced no chunks");
            for c in &chunks {
                assert_eq!(c.chunker_id.as_deref(), Some(id));
            }
        }
        let json = JsonListChunker
            .chunk_document(r#"["a", "b"]"#, &ctx(100))
            .unwrap();
        assert!(
            json.iter()
                .all(|c| c.chunker_id.as_deref() == Some(JSON_LIST_CHUNKER_ID))
        );
    }

    #[test]
    fn empty_chunker_id_leaves_the_field_unset() {
        let chunker = FnChunker::with_id("", Arc::new(|t: &str, _| vec![t.to_owned()]));
        let chunks = chunker.chunk_document("text", &ctx(10)).unwrap();
        assert_eq!(chunks[0].chunker_id, None);
    }

    #[test]
    fn text_chunker_matches_chunk_text() {
        let text = "One two. Three four.\n\nFive six seven.";
        let direct = chunk_text(
            Uuid::from_u128(42),
            text,
            3,
            &WordCounter,
            TokenCountMode::default(),
        );
        let via = TextChunker.chunk(text, &ctx(3)).unwrap();
        let texts = |v: &[DocumentChunk]| v.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&direct), texts(&via));
    }

    // -- RecursiveChunker (ports test_langchain_chunker.py) -----------------

    #[test]
    fn recursive_yields_valid_document_chunks() {
        let text = "one two three four five. ".repeat(40);
        let chunks = RecursiveChunker::default()
            .chunk_document(&text, &ctx(512))
            .unwrap();
        assert!(!chunks.is_empty());
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, i);
            assert_eq!(c.cut_type, "missing");
            assert_eq!(c.chunk_size, word_length(&c.text));
            assert!(c.chunk_size <= 512);
            assert_eq!(c.document_id, Uuid::from_u128(42));
            assert_eq!(c.chunker_id.as_deref(), Some(LANGCHAIN_CHUNKER_ID));
        }
    }

    #[test]
    fn recursive_rejects_chunks_over_the_token_budget() {
        // 200 words fit one 1024-word split, but not a 50-token budget.
        let text = "word ".repeat(200);
        let err = RecursiveChunker::default()
            .chunk(&text, &ctx(50))
            .unwrap_err();
        assert!(
            matches!(
                err,
                ChunkingError::ChunkTooLarge {
                    size: 200,
                    max: 50,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn recursive_boundaries_differ_from_paragraph_chunking() {
        let text = (0..60)
            .map(|i| format!("Sentence number {i} is here."))
            .collect::<Vec<_>>()
            .join(" ");
        let recursive = RecursiveChunker::new(40, 10)
            .unwrap()
            .chunk(&text, &ctx(100))
            .unwrap();
        let paragraph = TextChunker.chunk(&text, &ctx(40)).unwrap();
        let texts = |v: &[DocumentChunk]| v.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
        assert_ne!(texts(&recursive), texts(&paragraph));
        // The overlap repeats words, so recursive output covers more words.
        let words = |v: &[DocumentChunk]| v.iter().map(|c| c.chunk_size).sum::<usize>();
        assert!(words(&recursive) > words(&paragraph));
    }

    #[test]
    fn recursive_rejects_invalid_splitter_configs() {
        assert!(RecursiveChunker::new(0, 0).is_err());
        assert!(RecursiveChunker::new(10, 11).is_err());
    }

    // -- CsvChunker ----------------------------------------------------------

    #[test]
    fn csv_chunker_cuts_by_row() {
        let text = "name: Alice, age: 30\n\nname: Bob, age: 25";
        let chunks = CsvChunker.chunk(text, &ctx(100)).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "name: Alice, age: 30");
        assert_eq!(chunks[1].text, "name: Bob, age: 25");
        assert!(chunks.iter().all(|c| c.cut_type == "row_end"));
    }

    // -- JsonListChunker -----------------------------------------------------

    #[test]
    fn json_list_gives_one_chunk_per_item() {
        let chunks = JsonListChunker
            .chunk(
                r#"["first item", {"b": 1, "a": [true, null, 1.5]}, 7]"#,
                &ctx(100),
            )
            .unwrap();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["first item", "{'b': 1, 'a': [True, None, 1.5]}", "7"]
        );
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_index, i);
            assert_eq!(c.cut_type, "json_list_item");
            assert_eq!(c.base.metadata["json_list_index"], serde_json::json!(i));
            assert_eq!(c.base.id, ctx(100).positional_chunk_id(i));
        }
        assert_eq!(chunks[1].chunk_size, 6);
    }

    #[test]
    fn json_list_rejects_non_lists() {
        for text in [r#"{"a": 1}"#, "\"str\"", "not json"] {
            let err = JsonListChunker.chunk(text, &ctx(100)).unwrap_err();
            assert!(
                matches!(err, ChunkingError::InvalidInput(_)),
                "{text}: {err:?}"
            );
        }
    }

    #[test]
    fn json_list_keeps_oversized_items() {
        let chunks = JsonListChunker.chunk(r#"["a b c d e"]"#, &ctx(2)).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_size, 5);
    }

    #[test]
    fn python_repr_matches_python() {
        let cases = [
            (serde_json::json!("it's"), "\"it's\""),
            (serde_json::json!("say \"hi\""), "'say \"hi\"'"),
            (serde_json::json!("both ' and \""), "'both \\' and \"'"),
            (serde_json::json!("a\nb\\"), "'a\\nb\\\\'"),
            (serde_json::json!(1.0), "1.0"),
            (serde_json::json!(1e20), "1e+20"),
            (serde_json::json!(1.5e-7), "1.5e-07"),
            (serde_json::json!(0.0001), "0.0001"),
            (serde_json::json!(-3), "-3"),
            (serde_json::json!([]), "[]"),
            (serde_json::json!({}), "{}"),
        ];
        for (value, expected) in cases {
            assert_eq!(python_repr(&value), expected, "{value}");
        }
    }

    // -- TextChunkerWithOverlap (ports test_text_chunker_with_overlap.py) ----

    fn pieces(words: &[&str], size: usize) -> Vec<OverlapPiece> {
        words
            .iter()
            .map(|w| OverlapPiece {
                text: (*w).to_owned(),
                chunk_size: size,
                cut_type: "sentence".to_owned(),
                chunk_id: Uuid::new_v4(),
            })
            .collect()
    }

    fn run(ratio: f64, max: usize, words: &[&str], size: usize) -> Vec<DocumentChunk> {
        TextChunkerWithOverlap::new(ratio)
            .unwrap()
            .accumulate(pieces(words, size), &ctx(max))
    }

    #[test]
    fn half_overlap_preserves_content_across_chunks() {
        let chunks = run(0.5, 20, &["one", "two", "three", "four"], 10);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["one two", "two three", "three four"]);
        assert_eq!(
            chunks.iter().map(|c| c.chunk_index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn zero_overlap_produces_no_duplicate_content() {
        let chunks = run(0.0, 20, &["one", "two", "three", "four"], 10);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["one two", "three four"]);
    }

    #[test]
    fn small_overlap_ratio_creates_minimal_overlap() {
        let chunks = run(
            0.25,
            40,
            &["alpha", "beta", "gamma", "delta", "epsilon"],
            10,
        );
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["alpha beta gamma delta", "delta epsilon"]);
    }

    #[test]
    fn high_overlap_ratio_creates_significant_overlap() {
        let chunks = run(0.75, 20, &["red", "blue", "green", "yellow", "purple"], 5);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["red blue green yellow", "blue green yellow purple"]
        );
    }

    #[test]
    fn single_chunk_no_dangling_overlap() {
        let chunks = run(0.5, 20, &["alpha", "beta"], 10);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "alpha beta");
        assert_eq!(chunks[0].chunk_index, 0);
    }

    #[test]
    fn oversized_piece_is_emitted_alone_with_its_own_id() {
        let mut p = pieces(&["big"], 50);
        let id = p[0].chunk_id;
        p.extend(pieces(&["small"], 5));
        let chunks = TextChunkerWithOverlap::new(0.5)
            .unwrap()
            .accumulate(p, &ctx(20));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "big");
        assert_eq!(chunks[0].base.id, id);
        assert_eq!(chunks[1].text, "small");
        assert_eq!(chunks[1].chunk_index, 1);
    }

    #[test]
    fn paragraph_chunking_with_overlap() {
        // Port of test_paragraph_chunking_with_overlap: 50 words, budget 20,
        // 25% overlap → pre-cut paragraphs of int(0.5*0.25*20) = 2 words.
        let text = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|p| {
                (0..5)
                    .map(|i| format!("{p}{} {p}{}.", 2 * i, 2 * i + 1))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = TextChunkerWithOverlap::new(0.25)
            .unwrap()
            .chunk_document(&text, &ctx(20))
            .unwrap();
        assert_eq!(chunks.len(), 3, "{chunks:#?}");
        assert!(chunks[0].text.contains("A0") && chunks[0].text.contains("B9"));
        assert!(chunks[1].text.contains('B') && chunks[1].text.contains('D'));
        assert!(chunks[2].text.contains('D') && chunks[2].text.contains("E9"));
        for pair in chunks.windows(2) {
            let tail: Vec<&str> = pair[0].text.split_whitespace().rev().take(4).collect();
            let next: Vec<&str> = pair[1].text.split_whitespace().collect();
            assert!(
                tail.iter().any(|w| next.contains(w)),
                "no overlap: {pair:?}"
            );
        }
        assert!(
            chunks
                .iter()
                .all(|c| c.chunker_id.as_deref() == Some(TEXT_CHUNKER_OVERLAP_ID))
        );
    }

    /// Above a 2/3 ratio, a carried tail plus the next piece can exceed the
    /// budget; the tail is trimmed instead (Python emits a 135-token chunk
    /// here).
    #[test]
    fn a_high_overlap_ratio_never_exceeds_the_budget() {
        // max 100, ratio 0.9 → overlap 90; pieces of 45.
        let chunks = run(0.9, 100, &["a", "b", "c", "d", "e"], 45);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chunk_size <= 100, "{} is {} tokens", c.text, c.chunk_size);
        }
        // The overlap is still there: each chunk ends with the next one's head.
        for pair in chunks.windows(2) {
            let last = pair[0].text.split(' ').next_back().unwrap();
            assert!(pair[1].text.starts_with(last), "{pair:?}");
        }
    }

    #[test]
    fn an_oversized_piece_after_an_accumulation_is_emitted_alone() {
        let mut p = pieces(&["x", "y"], 5);
        p.extend(pieces(&["big"], 50));
        p.extend(pieces(&["z"], 5));
        let chunks = TextChunkerWithOverlap::new(0.5)
            .unwrap()
            .accumulate(p, &ctx(20));
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["x y", "big", "z"]);
    }

    #[test]
    fn overlap_ratio_must_be_below_one() {
        assert!(TextChunkerWithOverlap::new(1.0).is_err());
        assert!(TextChunkerWithOverlap::new(-0.1).is_err());
        assert!(TextChunkerWithOverlap::new(f64::NAN).is_err());
        assert!(TextChunkerWithOverlap::new(0.0).is_ok());
    }

    // -- FnChunker -----------------------------------------------------------

    #[test]
    fn fn_chunker_builds_chunks_from_strings() {
        let chunker = FnChunker::new(Arc::new(|text: &str, max: usize| {
            assert_eq!(max, 7);
            text.split('|').map(str::to_owned).collect()
        }));
        let chunks = chunker.chunk_document("a b|c||d", &ctx(7)).unwrap();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["a b", "c", "d"]);
        assert_eq!(chunks[0].chunk_size, 2);
        assert_eq!(chunks[2].chunk_index, 2);
        assert!(chunks.iter().all(|c| c.cut_type == "custom"));
    }
}
