mod highlight;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use clap::{Parser, Subcommand, builder::Styles};
use owo_colors::OwoColorize;
use polars::prelude::*;
use highlight::rich_highlight;

/// Extremely fast data file peek — preview CSV and Parquet files instantly
#[derive(Parser)]
#[command(styles = help_styles())]
struct Cli {
    /// File to preview (defaults to head)
    file: Option<PathBuf>,

    /// Number of rows to show
    #[arg(short = 'n', default_value = "5")]
    n: usize,

    /// Fast mode: skip full CSV scan (CSV head shows no row count, CSV tail is disabled)
    #[arg(long)]
    lazy: bool,

    /// Field separator character (default: comma). Use \t for tab.
    #[arg(short = 'd', long)]
    delimiter: Option<String>,

    #[command(subcommand)]
    command: Option<SubCmd>,
}

#[derive(Subcommand)]
enum SubCmd {
    /// Show the last N rows
    Tail {
        /// File to preview
        file: PathBuf,
        /// Number of rows to show
        #[arg(short = 'n', default_value = "5")]
        n: usize,
        /// Fast mode: skip full CSV scan (CSV tail is disabled with this flag)
        #[arg(long)]
        lazy: bool,
        /// Field separator character (default: comma). Use \t for tab.
        #[arg(short = 'd', long)]
        delimiter: Option<String>,
    },
    /// Show column names and types without loading data
    Schema {
        /// File to inspect
        file: PathBuf,
        /// Fast mode: infer CSV types from first 100 rows only, skip row count scan
        #[arg(long)]
        lazy: bool,
        /// Field separator character (default: comma). Use \t for tab.
        #[arg(short = 'd', long)]
        delimiter: Option<String>,
    },
}

fn help_styles() -> Styles {
    use anstyle::{AnsiColor, Color, Style};
    Styles::styled()
        .usage(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .header(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .literal(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
}

#[derive(Clone, Copy)]
pub enum Mode { Head, Tail }

fn main() {
    // fetch(n) already limits rows; set -1 so Polars never truncates the display
    std::env::set_var("POLARS_FMT_MAX_ROWS", "-1");
    let cli = Cli::parse();
    let colorize = std::io::stdout().is_terminal();

    let result = match cli.command {
        Some(SubCmd::Tail { file, n, lazy, delimiter }) =>
            parse_delimiter_opt(delimiter.as_deref()).and_then(|sep| run(&file, n, Mode::Tail, colorize, lazy, sep)),
        Some(SubCmd::Schema { file, lazy, delimiter }) =>
            parse_delimiter_opt(delimiter.as_deref()).and_then(|sep| print_schema(&file, colorize, lazy, sep)),
        None => match cli.file {
            Some(file) =>
                parse_delimiter_opt(cli.delimiter.as_deref()).and_then(|sep| run(&file, cli.n, Mode::Head, colorize, cli.lazy, sep)),
            None => {
                eprintln!("error: provide a file or subcommand. Try --help");
                std::process::exit(1);
            }
        },
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn run(path: &PathBuf, n: usize, mode: Mode, colorize: bool, lazy: bool, delimiter: Option<u8>) -> Result<(), Box<dyn std::error::Error>> {
    if path.to_string_lossy().contains("://") {
        return Err(format!("{}: remote files are not supported", path.display()).into());
    }
    let fmt = detect_format(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let (total_rows, n_cols, df) = preview(path, &fmt, n, mode, lazy, delimiter)?;

    let showing = match mode { Mode::Head => "top", Mode::Tail => "last" };
    let display_n = total_rows.map(|r| n.min(r)).unwrap_or(n);

    if colorize {
        if let Some(rows) = total_rows {
            println!("{}  {} rows × {} cols  (showing {} {})",
                path.display().to_string().bold(), rows, n_cols, showing, display_n);
        } else {
            println!("{}  {} cols  (showing {} {})",
                path.display().to_string().bold(), n_cols, showing, display_n);
        }
    } else {
        if let Some(rows) = total_rows {
            println!("{}  {} rows × {} cols  (showing {} {})",
                path.display(), rows, n_cols, showing, display_n);
        } else {
            println!("{}  {} cols  (showing {} {})",
                path.display(), n_cols, showing, display_n);
        }
    }

    let text: String = df.to_string().lines().skip(1).collect::<Vec<_>>().join("\n");
    if colorize {
        println!("{}", rich_highlight(&text));
    } else {
        println!("{}", text);
    }
    Ok(())
}

fn print_schema(path: &PathBuf, colorize: bool, lazy: bool, delimiter: Option<u8>) -> Result<(), Box<dyn std::error::Error>> {
    if path.to_string_lossy().contains("://") {
        return Err(format!("{}: remote files are not supported", path.display()).into());
    }
    let fmt = detect_format(path).map_err(|e| format!("{}: {}", path.display(), e))?;

    // fields, total_rows (None = unknown), partial (types inferred from sample)
    let (fields, total_rows, partial) = match fmt {
        Format::Parquet => {
            // footer parsed once: num_rows() caches it, schema() reuses cache
            let f = std::fs::File::open(path)?;
            let mut reader = ParquetReader::new(f);
            let total_rows = reader.num_rows()?;
            let mut lf = new_lazy_frame(path, &fmt, delimiter);
            let schema = lf.collect_schema()?;
            let fields: Vec<(String, String)> = schema.iter()
                .map(|(name, dtype)| (name.to_string(), format!("{}", dtype)))
                .collect();
            (fields, Some(total_rows), false)
        }
        Format::Csv if lazy => {
            // fast path: infer from first 100 rows, no row count scan
            let mut lf = new_lazy_frame(path, &fmt, delimiter);
            let schema = lf.collect_schema()?;
            let fields: Vec<(String, String)> = schema.iter()
                .map(|(name, dtype)| (name.to_string(), format!("{}", dtype)))
                .collect();
            (fields, None, true)
        }
        Format::Csv => {
            // full scan: infer_schema_length(None) for accurate types + count rows
            let mut reader = LazyCsvReader::new(path.to_str().unwrap().into())
                .with_infer_schema_length(None);
            if let Some(sep) = delimiter { reader = reader.with_separator(sep); }
            let mut lf = reader.finish()?;
            let schema = lf.collect_schema()?;
            let fields: Vec<(String, String)> = schema.iter()
                .map(|(name, dtype)| (name.to_string(), format!("{}", dtype)))
                .collect();
            let count_df = lf.clone().select([len()]).collect()?;
            let total_rows = count_df.columns()[0].as_materialized_series().u32()?.get(0).unwrap_or(0) as usize;
            (fields, Some(total_rows), false)
        }
    };

    let n_cols = fields.len();
    let max_name_len = fields.iter().map(|(n, _)| n.len()).max().unwrap_or(0);

    let file_str = path.display().to_string();
    let row_part = match total_rows {
        Some(rows) => format!("  {} rows × {} cols", rows, n_cols),
        None       => format!("  {} cols", n_cols),
    };
    let note = if partial { "  (types inferred from first 100 rows)" } else { "" };

    if colorize {
        println!("{}{}{}", file_str.bold(), row_part, note);
        for (name, dtype) in &fields {
            println!("  {:<width$}  {}", name, dtype.dimmed(), width = max_name_len);
        }
    } else {
        println!("{}{}{}", file_str, row_part, note);
        for (name, dtype) in &fields {
            println!("  {:<width$}  {}", name, dtype, width = max_name_len);
        }
    }
    Ok(())
}

pub fn preview(
    path: &PathBuf,
    fmt: &Format,
    n: usize,
    mode: Mode,
    lazy: bool,
    delimiter: Option<u8>,
) -> Result<(Option<usize>, usize, DataFrame), Box<dyn std::error::Error>> {
    // CSV --lazy: fast path, no full scan/download
    if matches!(fmt, Format::Csv) && lazy {
        match mode {
            Mode::Tail => return Err("CSV tail requires full scan; remove --lazy to enable".into()),
            Mode::Head => {
                let mut lf = new_lazy_frame(path, fmt, delimiter);
                let n_cols = lf.collect_schema()?.len();
                let df = lf.limit(n as u32).collect()?;
                return Ok((None, n_cols, df));
            }
        }
    }

    // Parquet: open once, parse footer once → get both schema (n_cols) and row count.
    // This avoids a duplicate footer parse that collect_schema() would cause.
    // CSV: still needs collect_schema() + select([len()]) via LazyFrame.
    let (n_cols, total_rows, lf) = match fmt {
        Format::Parquet => {
            let f = std::fs::File::open(path)?;
            let mut reader = ParquetReader::new(f);
            let total_rows = reader.num_rows()?;  // parses + caches footer
            let n_cols = reader.schema()?.len();  // reuses cached footer
            let lf = new_lazy_frame(path, fmt, delimiter);
            (n_cols, total_rows, lf)
        }
        Format::Csv => {
            let mut lf = new_lazy_frame(path, fmt, delimiter);
            let n_cols = lf.collect_schema()?.len();
            let count_df = lf.clone().select([len()]).collect()?;
            let total_rows = count_df.columns()[0].as_materialized_series().u32()?.get(0).unwrap_or(0) as usize;
            (n_cols, total_rows, lf)
        }
    };

    let df = match mode {
        Mode::Head => lf.limit(n as u32).collect()?,
        Mode::Tail => {
            let offset = (total_rows as i64).saturating_sub(n as i64);
            lf.slice(offset, n as u32).collect()?
        }
    };

    Ok((Some(total_rows), n_cols, df))
}

fn new_lazy_frame(path: &PathBuf, fmt: &Format, delimiter: Option<u8>) -> LazyFrame {
    match fmt {
        Format::Parquet => LazyFrame::scan_parquet(path.to_str().unwrap().into(), ScanArgsParquet::default()).unwrap(),
        Format::Csv => {
            let mut r = LazyCsvReader::new(path.to_str().unwrap().into());
            if let Some(sep) = delimiter { r = r.with_separator(sep); }
            r.finish().unwrap()
        }
    }
}

fn parse_delimiter_opt(s: Option<&str>) -> Result<Option<u8>, Box<dyn std::error::Error>> {
    match s {
        None => Ok(None),
        Some("\\t") | Some("\t") => Ok(Some(b'\t')),
        Some(s) if s.len() == 1 => Ok(Some(s.as_bytes()[0])),
        Some(s) => Err(format!("delimiter must be a single character, got {:?}", s).into()),
    }
}

pub enum Format { Parquet, Csv }

pub fn detect_format(path: &PathBuf) -> Result<Format, &'static str> {
    let raw = path.to_string_lossy();
    let raw = raw.split(['?', '#']).next().unwrap_or(&raw);
    match Path::new(raw).extension().and_then(|e| e.to_str()) {
        Some("parquet") => Ok(Format::Parquet),
        Some("csv")     => Ok(Format::Csv),
        _               => Err("unsupported format: expected .parquet or .csv"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn examples_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
    }

    #[test]
    fn parse_delimiter_values() {
        assert_eq!(parse_delimiter_opt(None).unwrap(), None);
        assert_eq!(parse_delimiter_opt(Some(";")).unwrap(), Some(b';'));
        assert_eq!(parse_delimiter_opt(Some("|")).unwrap(), Some(b'|'));
        assert_eq!(parse_delimiter_opt(Some("\\t")).unwrap(), Some(b'\t'));
        assert!(parse_delimiter_opt(Some("ab")).is_err());
    }

    #[test]
    fn pipe_delimiter_preview() {
        let path = examples_dir().join("sample_pipe.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, n_cols, df) = preview(&path, &fmt, 5, Mode::Head, false, Some(b'|')).unwrap();
        assert_eq!(total, Some(3));
        assert_eq!(n_cols, 3);
        assert_eq!(df.height(), 3);
    }

    #[test]
    fn detect_format_parquet() {
        assert!(matches!(detect_format(&PathBuf::from("file.parquet")).unwrap(), Format::Parquet));
    }

    #[test]
    fn detect_format_csv() {
        assert!(matches!(detect_format(&PathBuf::from("file.csv")).unwrap(), Format::Csv));
    }

    #[test]
    fn detect_format_unknown_errors() {
        assert!(detect_format(&PathBuf::from("file.txt")).is_err());
    }

    #[test]
    fn detect_format_remote_parquet_with_query() {
        let path = PathBuf::from("https://example.com/data.parquet?download=1");
        assert!(matches!(detect_format(&path).unwrap(), Format::Parquet));
    }

    #[test]
    fn detect_format_remote_csv_with_query() {
        let path = PathBuf::from("https://example.com/data.csv?download=1");
        assert!(matches!(detect_format(&path).unwrap(), Format::Csv));
    }

    #[test]
    fn head_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 7, Mode::Head, false, None).unwrap();
        assert_eq!(total, Some(891));
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn tail_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 7, Mode::Tail, false, None).unwrap();
        assert_eq!(total, Some(891));
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn head_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 10, Mode::Head, false, None).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn tail_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 10, Mode::Tail, false, None).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn lazy_csv_head_no_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 5, Mode::Head, true, None).unwrap();
        assert_eq!(total, None);
        assert_eq!(df.height(), 5);
    }

    #[test]
    fn lazy_csv_tail_errors() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        assert!(preview(&path, &fmt, 5, Mode::Tail, true, None).is_err());
    }

    #[test]
    fn head_and_tail_parquet_differ() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (head_total, _, head) = preview(&path, &fmt, 3, Mode::Head, false, None).unwrap();
        let (tail_total, _, tail) = preview(&path, &fmt, 3, Mode::Tail, false, None).unwrap();
        assert_eq!(head_total, Some(891));
        assert_eq!(tail_total, Some(891));
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }

    #[test]
    fn head_and_tail_csv_differ() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (head_total, _, head) = preview(&path, &fmt, 3, Mode::Head, false, None).unwrap();
        let (tail_total, _, tail) = preview(&path, &fmt, 3, Mode::Tail, false, None).unwrap();
        assert_eq!(head_total, Some(150));
        assert_eq!(tail_total, Some(150));
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }
}
