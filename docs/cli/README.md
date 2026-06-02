# CLI Docs

- [LLM retries](./llm-retries.md)

## Logging

The CLI binary calls `cognee_logging::init_logging` at startup. The
env-var surface (`COGNEE_LOG_FILE`, `COGNEE_LOGS_DIR`,
`COGNEE_LOG_FORMAT`, `COGNEE_LOG_ROTATION`, `COGNEE_LOG_BACKUP_COUNT`,
`COGNEE_LOG_MAX_FILES`, `LOG_LEVEL`, `LOG_FILE_NAME`) is shared with
the HTTP server and bindings — see the
[project README's "Logging" section](../../README.md#logging) for the
canonical table and the multi-process rotation warning.

CLI-specific example — emit JSON-formatted logs to a custom directory:

```bash
COGNEE_LOG_FORMAT=json COGNEE_LOGS_DIR=/var/log/cognee cognee-cli cognify -d main_dataset
```
