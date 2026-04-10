# Task 10: Port Temporal Interval Prompt

## Summary

The Rust `TemporalRetriever` uses a minimal one-sentence prompt for temporal interval extraction and models the interval with flat `start`/`end` string fields. The Python implementation uses a detailed multi-rule extraction prompt that injects the current date at runtime and models the interval with structured `Timestamp` objects using `starts_at`/`ends_at` field names. This task ports the full Python prompt, aligns the data model field names, and adds current-time injection.

## Current Rust Behavior

**File:** `crates/search/src/retrievers/temporal_retriever.rs`

### Prompt (line 25)

```rust
const DEFAULT_TEMPORAL_INTERVAL_PROMPT: &str = "Extract the temporal interval for a user question. Return JSON with optional string fields `start` and `end` in ISO-like format (YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339). Leave missing bounds as null.";
```

This is a single-sentence prompt with no extraction rules and no current date injection.

### Data model (lines 41-45)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct QueryInterval {
    start: Option<String>,
    end: Option<String>,
}
```

Fields are `start` and `end` (flat strings), not `starts_at` and `ends_at` (structured `Timestamp` objects).

### Interval extraction (lines 115-143)

```rust
async fn extract_interval(&self, query: &str) -> Result<Option<ParsedInterval>, SearchError> {
    let system_prompt = self
        .temporal_interval_prompt
        .as_deref()
        .unwrap_or(DEFAULT_TEMPORAL_INTERVAL_PROMPT)
        .to_string();
    // ... sends system_prompt + user query to LLM ...
}
```

No current date is injected into the prompt. The prompt text is used verbatim.

## Required Python Behavior

### Prompt file: `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/extract_query_time.txt` (lines 1-14)

```
You are tasked with identifying relevant time periods where the answer to a given query should be searched.
Current date is:  `{{ time_now }}`. Determine relevant period(s) and return structured intervals.

Extraction rules:

1. Query without specific timestamp: use the time period with starts_at set to None and ends_at set to now.
2. Explicit time intervals: If the query specifies a range (e.g., from 2010 to 2020, between January and March 2023), extract both start and end dates. Always assign the earlier date to starts_at and the later date to ends_at.
3. Single timestamp: If the query refers to one specific moment (e.g., in 2015, on March 5, 2022), set starts_at and ends_at to that same timestamp.
4. Open-ended time references: For phrases such as "before X" or "after X", represent the unspecified side as None. For example: before 2009 → starts_at: None, ends_at: 2009; after 2009 → starts_at: 2009, ends_at: None.
5. Current-time references ("now", "current", "today"): If the query explicitly refers to the present, set both starts_at and ends_at to now (the ingestion timestamp).
6. "Who is" and "Who was" questions: These imply a general identity or biographical inquiry without a specific temporal scope. Set both starts_at and ends_at to None.
7. Ordering rule: Always ensure the earlier date is assigned to starts_at and the later date to ends_at.
8. No temporal information: If no valid or inferable time reference is found, set both starts_at and ends_at to None.
```

### Current time injection: `temporal_retriever.py` (lines 84-103)

```python
async def extract_time_from_query(self, query: str):
    # ...
    time_now = datetime.now().strftime("%d-%m-%Y")
    system_prompt = render_prompt(
        prompt_path, {"time_now": time_now}, base_directory=base_directory
    )
    interval = await LLMGateway.acreate_structured_output(query, system_prompt, QueryInterval)
    time_from = interval.starts_at
    time_to = interval.ends_at
    return time_from, time_to
```

The current date is formatted as `DD-MM-YYYY` and injected into the prompt template at `{{ time_now }}`.

### Data model: `tasks/temporal_graph/models.py` (lines 5-27)

```python
class Timestamp(BaseModel):
    year: int = Field(..., ge=1, le=9999, description="Always required. If only a year is known, use it.")
    month: int = Field(1, ge=1, le=12, description="If unknown, default to 1")
    day: int = Field(1, ge=1, le=31, description="If unknown, default to 1")
    hour: int = Field(0, ge=0, le=23, description="If unknown, default to 0")
    minute: int = Field(0, ge=0, le=59, description="If unknown, default to 0")
    second: int = Field(0, ge=0, le=59, description="If unknown, default to 0")

class QueryInterval(BaseModel):
    starts_at: Optional[Timestamp] = None
    ends_at: Optional[Timestamp] = None
```

Python uses structured `Timestamp` with individual numeric fields, and the interval fields are named `starts_at`/`ends_at`.

## Step-by-Step Changes

### Step 1: Replace the default prompt constant

In `crates/search/src/retrievers/temporal_retriever.rs`, replace the `DEFAULT_TEMPORAL_INTERVAL_PROMPT` constant (line 25) with the full Python prompt text. Use a placeholder `{time_now}` for runtime substitution:

```rust
const DEFAULT_TEMPORAL_INTERVAL_PROMPT: &str = "\
You are tasked with identifying relevant time periods where the answer to a given query should be searched.
Current date is: `{time_now}`. Determine relevant period(s) and return structured intervals.

Extraction rules:

1. Query without specific timestamp: use the time period with starts_at set to None and ends_at set to now.
2. Explicit time intervals: If the query specifies a range (e.g., from 2010 to 2020, between January and March 2023), extract both start and end dates. Always assign the earlier date to starts_at and the later date to ends_at.
3. Single timestamp: If the query refers to one specific moment (e.g., in 2015, on March 5, 2022), set starts_at and ends_at to that same timestamp.
4. Open-ended time references: For phrases such as \"before X\" or \"after X\", represent the unspecified side as None. For example: before 2009 → starts_at: None, ends_at: 2009; after 2009 → starts_at: 2009, ends_at: None.
5. Current-time references (\"now\", \"current\", \"today\"): If the query explicitly refers to the present, set both starts_at and ends_at to now (the ingestion timestamp).
6. \"Who is\" and \"Who was\" questions: These imply a general identity or biographical inquiry without a specific temporal scope. Set both starts_at and ends_at to None.
7. Ordering rule: Always ensure the earlier date is assigned to starts_at and the later date to ends_at.
8. No temporal information: If no valid or inferable time reference is found, set both starts_at and ends_at to None.";
```

### Step 2: Add a `Timestamp` struct

Add a new struct matching the Python `Timestamp` model, placed above `QueryInterval`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct Timestamp {
    /// Always required. If only a year is known, use it.
    year: i32,
    /// If unknown, default to 1.
    #[serde(default = "default_one")]
    month: u32,
    /// If unknown, default to 1.
    #[serde(default = "default_one")]
    day: u32,
    /// If unknown, default to 0.
    #[serde(default)]
    hour: u32,
    /// If unknown, default to 0.
    #[serde(default)]
    minute: u32,
    /// If unknown, default to 0.
    #[serde(default)]
    second: u32,
}

fn default_one() -> u32 {
    1
}
```

Add a conversion method on `Timestamp`:

```rust
impl Timestamp {
    fn to_datetime(&self) -> Option<DateTime<Utc>> {
        let date = NaiveDate::from_ymd_opt(self.year, self.month, self.day)?;
        let time = chrono::NaiveTime::from_hms_opt(self.hour, self.minute, self.second)?;
        Some(Utc.from_utc_datetime(&date.and_time(time)))
    }
}
```

### Step 3: Rename `QueryInterval` fields from `start`/`end` to `starts_at`/`ends_at`

Change the struct to use the Python field names and `Timestamp` type:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct QueryInterval {
    starts_at: Option<Timestamp>,
    ends_at: Option<Timestamp>,
}
```

### Step 4: Update `QueryInterval::parse` to use `Timestamp::to_datetime`

```rust
impl QueryInterval {
    fn parse(self) -> ParsedInterval {
        ParsedInterval {
            start: self.starts_at.and_then(|ts| ts.to_datetime()),
            end: self.ends_at.and_then(|ts| ts.to_datetime()),
        }
    }
}
```

### Step 5: Inject current time into the prompt

In the `extract_interval` method (line 115), inject the current date into the prompt template before sending it to the LLM:

```rust
async fn extract_interval(&self, query: &str) -> Result<Option<ParsedInterval>, SearchError> {
    let time_now = Utc::now().format("%d-%m-%Y").to_string();

    let prompt_template = self
        .temporal_interval_prompt
        .as_deref()
        .unwrap_or(DEFAULT_TEMPORAL_INTERVAL_PROMPT);

    let system_prompt = prompt_template.replace("{time_now}", &time_now);

    let interval = match self
        .llm
        .create_structured_output_with_messages::<QueryInterval>(
            vec![
                Message::system(system_prompt),
                Message::user(query.to_string()),
            ],
            self.generation_options.clone(),
        )
        .await
    {
        Ok(interval) => interval,
        Err(_) => return Ok(None),
    };

    let parsed = interval.parse();
    if parsed.start.is_none() && parsed.end.is_none() {
        return Ok(None);
    }

    Ok(Some(parsed))
}
```

### Step 6: Update test `QueryInterval` construction

In the test module, update all `QueryInterval` construction to use the new field names and `Timestamp` type. For example, the test at line 942:

```rust
interval_response: Some(QueryInterval {
    starts_at: Some(Timestamp {
        year: 2024, month: 1, day: 1,
        hour: 0, minute: 0, second: 0,
    }),
    ends_at: Some(Timestamp {
        year: 2024, month: 12, day: 31,
        hour: 23, minute: 59, second: 59,
    }),
}),
```

The `TestLlm::create_structured_output_with_messages_raw` must serialize the new `QueryInterval` shape correctly -- this should work as-is since it uses `serde_json::to_value`.

### Step 7: Verify the `parse_bound` function is still used for event time parsing

The `parse_bound` function (line 492) is still needed for parsing temporal values from graph node data (called by `extract_event_time` -> `parse_temporal_value`). It is no longer needed for parsing `QueryInterval` fields since those are now structured `Timestamp` objects. No changes needed to `parse_bound` itself -- it remains used for node data parsing.

## Test Verification

1. **Existing test `returns_temporal_event_context_when_interval_matches`** (line 889): Update the `QueryInterval` construction to use `Timestamp` structs and new field names. Verify it still passes.

2. **Existing test `falls_back_to_graph_context_when_interval_extraction_fails`** (line 978): No changes needed -- this test uses `fail_structured_output: true` so the interval is never parsed.

3. **New unit test: `current_time_is_injected_into_prompt`**: Capture the messages sent to the LLM during `extract_interval` and assert that the system prompt contains the current date in `DD-MM-YYYY` format.

4. **New unit test: `timestamp_to_datetime_conversion`**: Test `Timestamp::to_datetime()` with various inputs (year-only, full datetime, edge cases like Feb 29).

5. **New unit test: `query_interval_parse_with_timestamps`**: Test `QueryInterval::parse()` with `Timestamp` values and verify the resulting `ParsedInterval` has correct `DateTime<Utc>` values.

6. Run `cargo check --all-targets` and `scripts/check_all.sh`.

## Dependencies

- `chrono` (already a dependency) -- used for `Utc::now().format()` and `DateTime` operations.
- No new crate dependencies required.
- Depends on the existing `Llm::create_structured_output_with_messages` trait method supporting the new `Timestamp`/`QueryInterval` JSON schema via `schemars::JsonSchema`.
