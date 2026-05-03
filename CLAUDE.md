# CLAUDE.md — dpeek

Project context and coding conventions for AI assistants working in this repository.

## Agent Rules

- **Do not `git push` without explicit user approval.** Commit and merge locally, then ask before pushing.
- **Create a new branch for every feat/fix.** Branch off master, implement, commit, then merge back. Never commit directly to master.
- **Follow Polars ecosystem conventions.** Feature design should match Polars user intuition: 0-based indexing, hard errors on unknown column names (`ColumnNotFoundError` style), silent clamp on range overflow (Rust/Python slice semantics), etc.

## Design Principles

- **Default to accuracy.** The default mode always produces correct results (full scan, full type inference).
- **`--fast` trades accuracy for speed.** With `--fast`: CSV type inference uses only the first 100 rows, row count is skipped, and CSV tail is disabled. Users opt in knowingly.

## Project Overview

**dpeek** is a fast CLI tool for previewing CSV and Parquet files. Think `head`/`tail` but data-aware: it shows row counts, column counts, and colorized tabular output.

```
dpeek file.parquet           # head 5 rows
dpeek -n 20 file.csv         # head 20 rows
dpeek tail file.parquet      # tail 5 rows
dpeek --lazy file.csv        # skip row count scan (fast path)
```

## Build & Test

```bash
cargo build --release          # release binary → target/release/dpeek
cargo test                     # 13 unit tests
cargo test --release           # run tests with optimizations
```

No external setup required. All dependencies are pinned in `Cargo.toml`.

## Dependencies

```toml
polars = { version = "0.53", features = ["parquet", "csv", "lazy", "timezones", "cloud"] }
clap   = { version = "4.6",  features = ["derive", "color"] }
regex  = "1.12"
owo-colors = { version = "4.3", features = ["supports-colors"] }
anstyle    = "1.0"
```

## Key Architecture

All logic lives in two files:

- **`src/main.rs`** — CLI parsing, `run()`, `preview()`, `detect_format()`, tests
- **`src/highlight.rs`** — ANSI colorization of DataFrame output

### `preview()` function

The core function. Returns `(Option<total_rows>, n_cols, DataFrame)`.

**Parquet path:**
- Opens file once with `ParquetReader`, calls `num_rows()` then `schema()` on the same instance — the footer is parsed and cached on the first call, reused on the second.
- Then creates a `LazyFrame` purely for data reading (`scan_parquet` → `limit`/`slice`).
- This avoids the duplicate footer read that `collect_schema()` would cause.

**CSV path:**
- Uses `LazyCsvReader` → `collect_schema()` for column count.
- Uses `lf.clone().select([len()]).collect()` for row count (Polars fast-path, no full scan).
- With `--lazy`: skips row count entirely, returns `None` for total_rows.

**Tail mode:**
- Computes `offset = total_rows - n`, then `lf.slice(offset, n)`.
- Not supported for CSV with `--lazy` (requires full scan).

### Remote file rejection

Any path containing `://` is rejected early with a clear error. Remote files are not supported.

## Polars 0.53 Internals (relevant to this codebase)

- `parquet` feature auto-enables `new_streaming` → `collect()` uses tokio async engine
- `scan_parquet().limit(n).collect()` only decodes the first `n` rows within the first row group
- `ParquetReader::num_rows()` reads only the footer (~64KB read). `O(row_groups)` not `O(1)` — footer stores metadata for every row group, so 300+ row groups means parsing a large footer
- `ParquetReader::schema()` reuses the already-cached footer from `num_rows()`
- `select([len()]).collect()` on Parquet: Polars 0.53 has a fast path when projection is empty (row count from footer metadata, no data read)

## Performance Characteristics

Measured on Apple M4, macOS, release build:

### Warm cache (file in OS page cache)

| File type | Total time |
|-----------|-----------|
| 45MB Parquet, GZIP, 1 row group | ~15ms |
| 63MB Parquet, ZSTD, 307 row groups | ~10ms |
| Small Parquet (titanic, 891 rows) | <5ms |

### Cold data (binary warm, data not cached)

| File type | Total time |
|-----------|-----------|
| 45MB Parquet, GZIP, 1 row group | ~100ms |
| 63MB Parquet, ZSTD, 307 row groups | ~70ms |

Cold-start bottleneck breakdown:
- `parquet_footer` (schema + row count): scales with number of row groups
- `data_collect` (actual data read): dominates for large single-row-group files (must decompress entire row group to get first N rows)

### Why cold start is slow for poorly-formatted Parquet

Parquet's minimum IO unit is a **row group**. If a file has 1 row group with 3M rows, reading 5 rows requires decompressing all column chunks of that row group. A well-formatted file should have row groups of ~50K–500K rows.

### Comparison with Python (warm cache, in-process)

| Tool | Original file | Optimized file |
|------|--------------|----------------|
| dpeek (CLI, incl. process start) | 19ms | 15ms |
| pandas `read_parquet()` | 74ms | 80ms |
| pyarrow `iter_batches(5)` | 12ms | 2ms |

Note: Python numbers are in-process (no interpreter startup). As a cold CLI tool, pandas takes ~600ms and pyarrow ~200ms vs dpeek ~70–100ms.

## Testing

Tests live at the bottom of `src/main.rs`. They use files in `examples/`:

- `examples/titanic.parquet` — 891 rows, used for Parquet tests
- `examples/iris.csv` — 150 rows, used for CSV tests

Tests cover: format detection, head/tail row counts, lazy mode, error cases.

## Ignored Paths

```
local/      # large test files (not committed)
plan.md
```

## Conventions

- No `unwrap()` in production paths — use `?` and propagate errors
- Remote URL check is done in `run()` before format detection
- `POLARS_FMT_MAX_ROWS=-1` is set at startup so Polars never truncates the displayed DataFrame
- `colorize` is determined once from `stdout().is_terminal()` and passed down
