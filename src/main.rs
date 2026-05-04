mod highlight;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use clap::{Parser, Subcommand, builder::Styles};
use owo_colors::OwoColorize;
use polars::prelude::*;
use terminal_size::{Width, terminal_size};
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

    /// Fast mode: skip full CSV scan; type inference uses first 100 rows only, no row count, CSV tail disabled
    #[arg(long)]
    fast: bool,

    /// Columns to show: names (col1,col2) or 0-based range (0:5)
    #[arg(short = 'c', long)]
    cols: Option<String>,

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
        /// Fast mode: skip full CSV scan; type inference uses first 100 rows only, CSV tail disabled
        #[arg(long)]
        fast: bool,
        /// Columns to show: names (col1,col2) or 0-based range (0:5)
        #[arg(short = 'c', long)]
        cols: Option<String>,
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
        fast: bool,
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
    // Scale max visible columns and table width to terminal width.
    // Each column needs ~12 chars min; floor at 4 so narrow terminals still show something.
    if let Some((Width(w), _)) = terminal_size() {
        let max_cols = (w / 12).max(4);
        std::env::set_var("POLARS_FMT_MAX_COLS", max_cols.to_string());
        std::env::set_var("POLARS_TABLE_WIDTH", w.to_string());
    }
    let cli = Cli::parse();
    let colorize = std::io::stdout().is_terminal();

    let result = match cli.command {
        Some(SubCmd::Tail { file, n, fast, cols, delimiter }) =>
            parse_delimiter_opt(delimiter.as_deref()).and_then(|sep| run(&file, n, Mode::Tail, colorize, fast, cols.as_deref(), sep)),
        Some(SubCmd::Schema { file, fast, delimiter }) =>
            parse_delimiter_opt(delimiter.as_deref()).and_then(|sep| print_schema(&file, colorize, fast, sep)),
        None => match cli.file {
            Some(file) =>
                parse_delimiter_opt(cli.delimiter.as_deref()).and_then(|sep| run(&file, cli.n, Mode::Head, colorize, cli.fast, cli.cols.as_deref(), sep)),
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

fn run(path: &PathBuf, n: usize, mode: Mode, colorize: bool, fast: bool, cols: Option<&str>, delimiter: Option<u8>) -> Result<(), Box<dyn std::error::Error>> {
    if path.to_string_lossy().contains("://") {
        return Err(format!("{}: remote files are not supported", path.display()).into());
    }
    if !path.exists() {
        return Err(format!("{}: no such file", path.display()).into());
    }
    let fmt = detect_format(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let (total_rows, total_cols, sel_cols, df) = preview(path, &fmt, n, mode, fast, cols, delimiter)?;

    let showing = match mode { Mode::Head => "top", Mode::Tail => "last" };
    let display_n = total_rows.map(|r| n.min(r)).unwrap_or(n);
    let col_note = if sel_cols < total_cols { format!(", {} cols", sel_cols) } else { String::new() };

    if colorize {
        if let Some(rows) = total_rows {
            println!("{}  {} rows × {} cols  (showing {} {}{})",
                path.display().to_string().bold(), rows, total_cols, showing, display_n, col_note);
        } else {
            println!("{}  {} cols  (showing {} {}{})",
                path.display().to_string().bold(), total_cols, showing, display_n, col_note);
        }
    } else {
        if let Some(rows) = total_rows {
            println!("{}  {} rows × {} cols  (showing {} {}{})",
                path.display(), rows, total_cols, showing, display_n, col_note);
        } else {
            println!("{}  {} cols  (showing {} {}{})",
                path.display(), total_cols, showing, display_n, col_note);
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

fn print_schema(path: &PathBuf, colorize: bool, fast: bool, delimiter: Option<u8>) -> Result<(), Box<dyn std::error::Error>> {
    if path.to_string_lossy().contains("://") {
        return Err(format!("{}: remote files are not supported", path.display()).into());
    }
    if !path.exists() {
        return Err(format!("{}: no such file", path.display()).into());
    }
    let fmt = detect_format(path).map_err(|e| format!("{}: {}", path.display(), e))?;

    // fields, total_rows (None = unknown), partial (types inferred from sample)
    let (fields, total_rows, partial) = match fmt {
        Format::Parquet => {
            // num_rows() reads footer; collect_schema() via LazyFrame is a separate read.
            // Two reads are unavoidable here since we need Polars DataType for display.
            let f = std::fs::File::open(path)?;
            let mut reader = ParquetReader::new(f);
            let total_rows = reader.num_rows()?;
            let mut lf = new_lazy_frame(path, &fmt, delimiter, false);
            let schema = lf.collect_schema()?;
            let fields: Vec<(String, String)> = schema.iter()
                .map(|(name, dtype)| (name.to_string(), format!("{}", dtype)))
                .collect();
            (fields, Some(total_rows), false)
        }
        Format::Csv if fast => {
            // fast path: infer from first 100 rows only, no row count scan
            let mut lf = new_lazy_frame(path, &fmt, delimiter, false);
            let schema = lf.collect_schema()?;
            let fields: Vec<(String, String)> = schema.iter()
                .map(|(name, dtype)| (name.to_string(), format!("{}", dtype)))
                .collect();
            (fields, None, true)
        }
        Format::Csv => {
            // default accurate mode: infer_full=true + count rows
            let mut lf = new_lazy_frame(path, &fmt, delimiter, true);
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
    fast: bool,
    cols: Option<&str>,
    delimiter: Option<u8>,
) -> Result<(Option<usize>, usize, usize, DataFrame), Box<dyn std::error::Error>> {
    // CSV --fast: skip row count scan
    if matches!(fmt, Format::Csv) && fast {
        match mode {
            Mode::Tail => return Err("CSV tail requires full scan; remove --fast to enable".into()),
            Mode::Head => {
                let mut lf = new_lazy_frame(path, fmt, delimiter, false);
                let schema = lf.collect_schema()?;
                let all_names: Vec<String> = schema.iter_names().map(|s| s.to_string()).collect();
                let total_cols = all_names.len();
                let col_names = resolve_cols(cols, &all_names)?;
                let sel_cols = col_names.len();
                let df = lf.select(col_names.iter().map(|s| col(s.as_str())).collect::<Vec<_>>())
                    .limit(n as u32).collect()?;
                return Ok((None, total_cols, sel_cols, df));
            }
        }
    }

    // Parquet: open once, parse footer once → get both schema and row count.
    // CSV: still needs collect_schema() + select([len()]) via LazyFrame.
    let (all_cols, total_rows, total_cols, lf) = match fmt {
        Format::Parquet => {
            let f = std::fs::File::open(path)?;
            let mut reader = ParquetReader::new(f);
            let total_rows = reader.num_rows()?;  // parses + caches footer
            let arrow_schema = reader.schema()?;  // reuses cached footer — no second read
            let all_names: Vec<String> = arrow_schema.iter_values().map(|f| f.name.to_string()).collect();
            let total_cols = all_names.len();
            let col_names = resolve_cols(cols, &all_names)?;
            let lf = new_lazy_frame(path, fmt, delimiter, false);
            (col_names, total_rows, total_cols, lf)
        }
        Format::Csv => {
            // infer_full=true: scan all rows for type inference (default accurate mode)
            let mut lf = new_lazy_frame(path, fmt, delimiter, true);
            let schema = lf.collect_schema()?;
            let all_names: Vec<String> = schema.iter_names().map(|s| s.to_string()).collect();
            let total_cols = all_names.len();
            let col_names = resolve_cols(cols, &all_names)?;
            let count_df = lf.clone().select([len()]).collect()?;
            let total_rows = count_df.columns()[0].as_materialized_series().u32()?.get(0).unwrap_or(0) as usize;
            (col_names, total_rows, total_cols, lf)
        }
    };

    let sel_cols = all_cols.len();
    let lf = lf.select(all_cols.iter().map(|s| col(s.as_str())).collect::<Vec<_>>());

    let df = match mode {
        Mode::Head => lf.limit(n as u32).collect()?,
        Mode::Tail => {
            let offset = ((total_rows as i64) - (n as i64)).max(0);
            lf.slice(offset, n as u32).collect()?
        }
    };

    Ok((Some(total_rows), total_cols, sel_cols, df))
}

fn new_lazy_frame(path: &PathBuf, fmt: &Format, delimiter: Option<u8>, infer_full: bool) -> LazyFrame {
    match fmt {
        Format::Parquet => LazyFrame::scan_parquet(path.to_str().unwrap().into(), ScanArgsParquet::default()).unwrap(),
        Format::Csv => {
            let mut r = LazyCsvReader::new(path.to_str().unwrap().into());
            if let Some(sep) = delimiter { r = r.with_separator(sep); }
            if infer_full { r = r.with_infer_schema_length(None); }
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

/// Resolve -c/--cols spec into an ordered list of column names.
/// - None   → all columns (in schema order)
/// - "a,b" → name list; unknown names → hard error listing all missing columns
/// - "0:5" → 0-based range [0,5), silently clamped; empty after clamping → error
/// - "5:"  → from index 5 to end  |  ":5" → from index 0 to 5
fn resolve_cols(cols: Option<&str>, all: &[String]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let spec = match cols {
        None => return Ok(all.to_vec()),
        Some(s) => s,
    };

    // Range syntax: contains ':', both sides must be empty or valid integers
    if spec.contains(':') {
        let (lhs, rhs) = spec.split_once(':').unwrap();
        let start_res = if lhs.trim().is_empty() { Ok(0usize) } else { lhs.trim().parse::<usize>() };
        let end_res   = if rhs.trim().is_empty() { Ok(all.len()) } else { rhs.trim().parse::<usize>() };
        if let (Ok(start), Ok(end)) = (start_res, end_res) {
            let start = start.min(all.len());
            let end   = end.min(all.len());
            if start >= end {
                return Err(format!(
                    "column range {}:{} is empty (file has {} columns)",
                    lhs.trim(), rhs.trim(), all.len()
                ).into());
            }
            return Ok(all[start..end].to_vec());
        }
    }

    // Name list: collect all unknown names and report together
    let name_set: std::collections::HashSet<&str> = all.iter().map(|s| s.as_str()).collect();
    let names: Vec<String> = spec.split(',').map(|s| s.trim().to_string()).collect();
    let missing: Vec<&str> = names.iter()
        .filter(|n| !name_set.contains(n.as_str()))
        .map(|n| n.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "column{} not found: {}",
            if missing.len() == 1 { "" } else { "s" },
            missing.iter().map(|n| format!("{:?}", n)).collect::<Vec<_>>().join(", ")
        ).into());
    }
    Ok(names)
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
        let (total, _, n_cols, df) = preview(&path, &fmt, 5, Mode::Head, false, None, Some(b'|')).unwrap();
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
        let (total, _, _, df) = preview(&path, &fmt, 7, Mode::Head, false, None, None).unwrap();
        assert_eq!(total, Some(891));
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn tail_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _, _, df) = preview(&path, &fmt, 7, Mode::Tail, false, None, None).unwrap();
        assert_eq!(total, Some(891));
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn head_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, _, df) = preview(&path, &fmt, 10, Mode::Head, false, None, None).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn tail_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, _, df) = preview(&path, &fmt, 10, Mode::Tail, false, None, None).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn fast_csv_head_no_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, _, df) = preview(&path, &fmt, 5, Mode::Head, true, None, None).unwrap();
        assert_eq!(total, None);
        assert_eq!(df.height(), 5);
    }

    #[test]
    fn fast_csv_tail_errors() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        assert!(preview(&path, &fmt, 5, Mode::Tail, true, None, None).is_err());
    }

    #[test]
    fn tail_n_larger_than_row_count_returns_all_rows() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        // iris has 150 rows; requesting 200 should return all 150
        let (total, _, _, df) = preview(&path, &fmt, 200, Mode::Tail, false, None, None).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 150);
    }

    #[test]
    fn head_and_tail_parquet_differ() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (head_total, _, _, head) = preview(&path, &fmt, 3, Mode::Head, false, None, None).unwrap();
        let (tail_total, _, _, tail) = preview(&path, &fmt, 3, Mode::Tail, false, None, None).unwrap();
        assert_eq!(head_total, Some(891));
        assert_eq!(tail_total, Some(891));
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }

    #[test]
    fn head_and_tail_csv_differ() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (head_total, _, _, head) = preview(&path, &fmt, 3, Mode::Head, false, None, None).unwrap();
        let (tail_total, _, _, tail) = preview(&path, &fmt, 3, Mode::Tail, false, None, None).unwrap();
        assert_eq!(head_total, Some(150));
        assert_eq!(tail_total, Some(150));
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }

    #[test]
    fn col_select_by_name() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (_, _, n_cols, df) = preview(&path, &fmt, 3, Mode::Head, false, Some("sepal_length,petal_length"), None).unwrap();
        assert_eq!(n_cols, 2);
        assert_eq!(df.get_column_names(), vec!["sepal_length", "petal_length"]);
    }

    #[test]
    fn col_select_by_range() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (_, _, n_cols, _) = preview(&path, &fmt, 3, Mode::Head, false, Some("0:2"), None).unwrap();
        assert_eq!(n_cols, 2);
    }

    #[test]
    fn col_select_range_clamps() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        // iris has 5 columns; requesting 0:100 should clamp to all 5
        let (_, _, n_cols, _) = preview(&path, &fmt, 3, Mode::Head, false, Some("0:100"), None).unwrap();
        assert_eq!(n_cols, 5);
    }

    #[test]
    fn col_select_unknown_name_errors() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let result = preview(&path, &fmt, 3, Mode::Head, false, Some("nonexistent_col"), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("column not found"), "unexpected error: {}", msg);
    }
    #[test]
    fn col_select_multiple_unknown_names_reported() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let result = preview(&path, &fmt, 3, Mode::Head, false, Some("foo,sepal_length,bar"), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("foo") && msg.contains("bar"), "unexpected error: {}", msg);
    }

    #[test]
    fn col_select_open_ended_range() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        // iris has 5 cols; "2:" -> cols 2,3,4 (3 cols)
        let (_, _, n_cols, _) = preview(&path, &fmt, 3, Mode::Head, false, Some("2:"), None).unwrap();
        assert_eq!(n_cols, 3);
    }

    #[test]
    fn col_select_empty_range_errors() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        // titanic has fewer than 20 cols, so 15:20 clamps to empty
        let result = preview(&path, &fmt, 3, Mode::Head, false, Some("15:20"), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("empty"), "unexpected error: {}", msg);
    }
}
