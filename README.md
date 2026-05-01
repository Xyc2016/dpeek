# dpeek

Extremely fast data file peek — preview CSV and Parquet files instantly.

## Usage

```bash
dpeek data.parquet       # show first 5 rows
dpeek data.parquet -n 20 # show first 20 rows
dpeek data.csv           # also works with CSV
```

## Install

```bash
cargo install --path .
```

## Supported formats

| Format   | Extension  | Row count from metadata |
|----------|-----------|------------------------|
| Parquet  | `.parquet` | Yes                    |
| CSV      | `.csv`    | No                     |
