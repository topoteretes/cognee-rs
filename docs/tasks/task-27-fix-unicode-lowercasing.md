# Task 27: Fix Unicode lowercasing in Lexical retriever -- Use full Unicode `to_lowercase()` instead of `to_ascii_lowercase()`

## Summary

The Rust `LexicalRetriever::tokenize()` method uses `ch.to_ascii_lowercase()` to normalize characters during tokenization. This only lowercases ASCII letters (A-Z to a-z) and leaves all non-ASCII Unicode letters unchanged. For example, `'Ü'` stays `'Ü'` instead of becoming `'ü'`, and `'Ñ'` stays `'Ñ'` instead of becoming `'ñ'`. This causes case-insensitive matching to fail for any non-ASCII text. The Python reference implementation uses `text.lower()`, which performs full Unicode-aware lowercasing via ICU/Python's built-in Unicode tables.

## Current Rust Behavior

**File:** `crates/search/src/retrievers/lexical_retriever.rs`, line 51

```rust
fn tokenize(&self, text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());   // <-- line 51: BUG
        } else if !current.is_empty() {
            if !self.stop_words.contains(&current) {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }

    if !current.is_empty() && !self.stop_words.contains(&current) {
        tokens.push(current);
    }

    tokens
}
```

**The problem in detail:**

1. `char::to_ascii_lowercase()` only maps ASCII `'A'..='Z'` to `'a'..='z'`. For any character outside that range it returns the character unchanged.
2. A query `"Über"` tokenizes to `["Über"]`, while a chunk containing `"über"` tokenizes to `["über"]`. These do not match, so the chunk gets a Jaccard score of 0.0 even though it is a case-insensitive match.
3. Similarly, `"REPÚBLICA"` and `"república"` would not match because `'Ú'` is not lowercased to `'ú'`.
4. The bug affects all non-Latin scripts with case distinctions: German (Ü/ü, Ö/ö, Ä/ä, ß), French (É/é, È/è), Spanish (Ñ/ñ), Turkish (İ/i, I/ı), Greek (Σ/σ/ς), Cyrillic (Д/д), and many more.

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/retrieval/jaccard_retrival.py`, lines 38-43

```python
def _tokenizer(self, text: str) -> list[str]:
    """
    Tokenizer: lowercases, splits on word characters (w+), filters stopwords.
    """
    tokens = re.findall(r"\w+", text.lower())
    return [t for t in tokens if t not in self.stop_words]
```

**Key difference:** Python's `str.lower()` performs full Unicode lowercasing. It correctly handles:
- `"Ü".lower()` -> `"ü"`
- `"Ñ".lower()` -> `"ñ"`
- `"ÜBER".lower()` -> `"über"`
- `"REPÚBLICA".lower()` -> `"república"`
- `"ДМИТРО".lower()` -> `"дмитро"` (Cyrillic)

Rust's `str::to_lowercase()` and `char::to_lowercase()` provide the same full Unicode lowercasing. The issue is that the current code uses the ASCII-only variant instead.

## Step-by-Step Code Changes

### Change 1: Replace `to_ascii_lowercase()` with Unicode-aware lowercasing in `tokenize()`

**File:** `crates/search/src/retrievers/lexical_retriever.rs`

In Rust, `char::to_lowercase()` returns a `ToLowercase` iterator (because some characters expand to multiple characters when lowercased -- for example, the German `'ẞ'` (capital sharp s) lowercases to `"ss"` (two characters)). This means we cannot simply do `current.push(ch.to_lowercase())` since `push()` takes a single `char`. Instead, we use `current.extend(ch.to_lowercase())` which appends all characters produced by the iterator.

**Old code (lines 45-66):**
```rust
fn tokenize(&self, text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            if !self.stop_words.contains(&current) {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }

    if !current.is_empty() && !self.stop_words.contains(&current) {
        tokens.push(current);
    }

    tokens
}
```

**New code:**
```rust
fn tokenize(&self, text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            if !self.stop_words.contains(&current) {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }

    if !current.is_empty() && !self.stop_words.contains(&current) {
        tokens.push(current);
    }

    tokens
}
```

The only change is on line 51: `current.push(ch.to_ascii_lowercase())` becomes `current.extend(ch.to_lowercase())`.

**Why `extend` instead of `push`:**
- `char::to_ascii_lowercase()` returns a single `char`, so `push()` works.
- `char::to_lowercase()` returns a `ToLowercase` iterator that may yield 1 or more `char`s. `String::extend()` consumes the iterator and appends all produced characters. For the vast majority of characters this yields exactly one character, so performance is equivalent. For the rare multi-character expansions (e.g., `'ẞ'` -> `"ss"`), it produces the correct result.

### No other changes needed

- No new dependencies are required. `char::to_lowercase()` is part of Rust's standard library and uses the Unicode Character Database tables that are already compiled into `std`.
- The stop words normalization on line 39 already uses `token.to_lowercase()` (the `str` method), which is full Unicode. So stop words are already correctly lowercased -- only the tokenization loop had the bug.
- The `score()` method does exact string comparison on tokens, so once both query and chunk tokens are correctly lowercased, matching works automatically.

## Test Verification

### New test: Unicode lowercasing in tokenization

Add the following test inside the existing `mod tests` block in `crates/search/src/retrievers/lexical_retriever.rs`:

```rust
#[tokio::test]
async fn unicode_case_insensitive_matching() {
    let mock_graph_db = Arc::new(MockGraphDB::new());
    // Chunk contains lowercase Unicode characters
    add_chunk(&mock_graph_db, "über die straße nach münchen").await;
    // Chunk with Cyrillic
    add_chunk(&mock_graph_db, "дмитро працює в компанії").await;
    // Chunk with Spanish accented characters
    add_chunk(&mock_graph_db, "la república de españa").await;
    // Unrelated chunk
    add_chunk(&mock_graph_db, "plain english text about nothing").await;
    let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

    let retriever = JaccardChunksRetriever::new(
        Arc::clone(&graph_db),
        Some(4),
        true,
        None,
        false,
    );

    // Query with uppercase Unicode -- should match the lowercase chunk
    let context = retriever.get_context("ÜBER MÜNCHEN").await.unwrap();
    assert!(!context.is_empty());
    let top_text = context[0]
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .expect("top item should have text");
    assert!(
        top_text.contains("über") && top_text.contains("münchen"),
        "uppercase Unicode query should match lowercase chunk, got: {top_text}"
    );
    assert!(context[0].score.unwrap() > 0.0);

    // Query with uppercase Cyrillic
    let context = retriever.get_context("ДМИТРО").await.unwrap();
    assert!(!context.is_empty());
    let top_text = context[0]
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .expect("top item should have text");
    assert!(
        top_text.contains("дмитро"),
        "uppercase Cyrillic query should match lowercase chunk, got: {top_text}"
    );

    // Query with mixed-case Spanish
    let context = retriever.get_context("República España").await.unwrap();
    assert!(!context.is_empty());
    let top_text = context[0]
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .expect("top item should have text");
    assert!(
        top_text.contains("república") && top_text.contains("españa"),
        "mixed-case Spanish query should match, got: {top_text}"
    );
}
```

### Unit test for specific character lowercasing

Add a targeted unit test that verifies the `tokenize` method itself without needing a graph database:

```rust
#[test]
fn tokenize_lowercases_unicode_characters() {
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let retriever = LexicalRetriever::new(
        graph_db,
        None,
        false,
        None,
        false,
    );

    // German umlauts
    assert_eq!(retriever.tokenize("Über"), vec!["über"]);
    assert_eq!(retriever.tokenize("STRASSE"), vec!["strasse"]);

    // Spanish ñ
    assert_eq!(retriever.tokenize("Ñoño"), vec!["ñoño"]);

    // French accents
    assert_eq!(retriever.tokenize("Éclairé"), vec!["éclairé"]);

    // Cyrillic
    assert_eq!(retriever.tokenize("Дмитро"), vec!["дмитро"]);

    // Greek
    assert_eq!(retriever.tokenize("ΣΩΚΡΆΤΗΣ"), vec!["σωκράτης"]);

    // Capital sharp S (single char expands to two lowercase chars)
    assert_eq!(retriever.tokenize("ẞ"), vec!["ss"]);

    // ASCII still works as before
    assert_eq!(retriever.tokenize("Hello World"), vec!["hello", "world"]);

    // Mixed ASCII and Unicode in one token
    assert_eq!(retriever.tokenize("Café"), vec!["café"]);
}
```

### How to verify

```bash
cargo test -p cognee-search -- lexical_retriever::tests
```

All tests should pass. The `unicode_case_insensitive_matching` and `tokenize_lowercases_unicode_characters` tests will **fail** on the old code (because `to_ascii_lowercase()` leaves non-ASCII characters unchanged) and **pass** after the fix.

## Dependencies

None. `char::to_lowercase()` is part of the Rust standard library. No new crate dependencies are needed.
