# dpeek

Extremely fast CLI tool for previewing CSV and Parquet files — built for data engineers and data scientists.

Think `head`/`tail` but data-aware: shows row counts, column counts, inferred types, and colorized tabular output.

Built on [Polars](https://pola.rs) — a high-performance DataFrame library written in Rust. Output uses the standard Polars table format (column names, types, and values).

## Install

```bash
cargo install --path .
```

## Usage

```bash
dpeek file.parquet              # head 5 rows
dpeek file.csv -n 20            # head 20 rows
dpeek tail file.parquet         # tail 5 rows
dpeek schema file.parquet       # show column names and types
dpeek -c col1,col2 file.csv     # select columns by name
dpeek -c 0:5 file.parquet       # select columns by range (0-based)
dpeek -d '|' file.csv           # custom delimiter (tab: \t)
dpeek --fast file.csv           # skip full scan (faster, no row count)
```

### Head

```
$ dpeek examples/titanic.parquet
examples/titanic.parquet  891 rows × 15 cols  (showing top 5)
┌──────────┬────────┬────────┬──────┬───┬──────┬─────────────┬───────┬───────┐
│ survived ┆ pclass ┆ sex    ┆ age  ┆ … ┆ deck ┆ embark_town ┆ alive ┆ alone │
│ ---      ┆ ---    ┆ ---    ┆ ---  ┆   ┆ ---  ┆ ---         ┆ ---   ┆ ---   │
│ i64      ┆ i64    ┆ str    ┆ f64  ┆   ┆ str  ┆ str         ┆ str   ┆ bool  │
╞══════════╪════════╪════════╪══════╪═══╪══════╪═════════════╪═══════╪═══════╡
│ 0        ┆ 3      ┆ male   ┆ 22.0 ┆ … ┆ null ┆ Southampton ┆ no    ┆ false │
│ 1        ┆ 1      ┆ female ┆ 38.0 ┆ … ┆ C    ┆ Cherbourg   ┆ yes   ┆ false │
│ 1        ┆ 3      ┆ female ┆ 26.0 ┆ … ┆ null ┆ Southampton ┆ yes   ┆ true  │
│ 1        ┆ 1      ┆ female ┆ 35.0 ┆ … ┆ C    ┆ Southampton ┆ yes   ┆ false │
│ 0        ┆ 3      ┆ male   ┆ 35.0 ┆ … ┆ null ┆ Southampton ┆ no    ┆ true  │
└──────────┴────────┴────────┴──────┴───┴──────┴─────────────┴───────┴───────┘
```

### Schema

```
$ dpeek schema examples/titanic.parquet
examples/titanic.parquet  891 rows × 15 cols
  survived     i64
  pclass       i64
  sex          str
  age          f64
  sibsp        i64
  parch        i64
  fare         f64
  embarked     str
  class        str
  who          str
  adult_male   bool
  deck         str
  embark_town  str
  alive        str
  alone        bool
```

### Column selection

```
$ dpeek -c survived,sex,age examples/titanic.parquet
examples/titanic.parquet  891 rows × 15 cols  (showing top 5, 3 cols)
┌──────────┬────────┬──────┐
│ survived ┆ sex    ┆ age  │
│ ---      ┆ ---    ┆ ---  │
│ i64      ┆ str    ┆ f64  │
╞══════════╪════════╪══════╡
│ 0        ┆ male   ┆ 22.0 │
│ 1        ┆ female ┆ 38.0 │
│ 1        ┆ female ┆ 26.0 │
│ 1        ┆ female ┆ 35.0 │
│ 0        ┆ male   ┆ 35.0 │
└──────────┴────────┴──────┘
```

## Options

| Flag | Description |
|------|-------------|
| `-n N` | Number of rows to show (default: 5) |
| `--fast` | Fast mode: skip full CSV scan (CSV only, see below) |
| `-c COLS` | Column selection: `col1,col2` (names) or `0:5` (0-based range) |
| `-d CHAR` | Field delimiter for CSV (default: `,`). Use `\t` for tab |

### Default mode vs `--fast` (CSV only)

`--fast` only affects CSV files. Parquet stores schema and row count in its file footer — dpeek reads that metadata directly at no extra cost, so there's nothing to skip.

**dpeek defaults to accuracy.** In default mode:
- **Parquet**: row count comes from file metadata (free, no scan needed). Type inference is exact.
- **CSV**: dpeek scans the entire file to count rows and infer types across all data.

**`--fast` trades accuracy for speed.** With `--fast`:
- Type inference uses only the first 100 rows (may mis-detect types in dirty data)
- Row count is skipped (not shown in output header)
- `tail` is disabled for CSV (requires a full scan to find the end)

Use `--fast` when you just need a quick look and the file is large.

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `tail` | Show the last N rows |
| `schema` | Show column names and types without loading data |

## Performance

Measured on Apple M4, macOS, release build. All times are for the default `head` command (5 rows). All times include process startup.

### Warm cache (file already in OS page cache)

| File | Size | Mode | Time |
|------|------|------|------|
| `titanic.parquet` | 11 KB | default | ~23ms |
| `iris.csv` | 4 KB | default | ~24ms |
| `yellow_tripdata_2015-01.parquet` | 167 MB | default | ~35ms |
| `yellow_tripdata_2015-01.csv` | 1.8 GB | `--fast` | ~24ms |
| `yellow_tripdata_2015-01.csv` | 1.8 GB | default | ~30s |

The last row shows why `--fast` exists: default mode on a 1.8 GB CSV scans the entire file to count rows and infer types accurately. `--fast` drops that to 24ms by reading only the first 100 rows.

### Cold cache (file not in OS page cache)

Cold cache is the more realistic metric for day-to-day use — the first time you open a file after receiving it, it won't be in the OS cache.

| File | Size | Mode | Time |
|------|------|------|------|
| `titanic.parquet` | 11 KB | default | ~80ms |
| `iris.csv` | 4 KB | default | ~80ms |
| `yellow_tripdata_2015-01.parquet` | 167 MB | default | ~210ms |
| `yellow_tripdata_2015-01.csv` | 1.8 GB | `--fast` | ~90ms |

**Why Parquet is fast even cold:** dpeek reads only the file footer (schema + row count metadata) and the first row group. For a 167 MB file, that's typically well under 10 MB of actual I/O.

**Why CSV `--fast` is fast cold:** with `--fast`, dpeek reads only the first 100 rows (~a few KB). Even cold, almost no I/O is needed.

For comparison: Python tools (pandas, pyarrow) add ~500ms of interpreter startup on top of these numbers when invoked as CLI commands.

## Supported formats

| Format | Extension |
|--------|-----------|
| Parquet | `.parquet` |
| CSV | `.csv` |

## Build

```bash
cargo build --release   # binary at target/release/dpeek
cargo test              # run unit tests
```
