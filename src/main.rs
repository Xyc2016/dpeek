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
        Some(SubCmd::Tail { file, n, lazy }) => run(&file, n, Mode::Tail, colorize, lazy),
        None => match cli.file {
            Some(file) => run(&file, cli.n, Mode::Head, colorize, cli.lazy),
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

fn run(path: &PathBuf, n: usize, mode: Mode, colorize: bool, lazy: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fmt = detect_format(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let remote = is_remote_source(path);
    let (total_rows, n_cols, df) = preview(path, &fmt, n, mode, remote, lazy)?;

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

pub fn metadata(path: &PathBuf, fmt: &Format) -> Result<(Option<usize>, usize), Box<dyn std::error::Error>> {
    match fmt {
        Format::Parquet => {
            let f = std::fs::File::open(path)?;
            let mut reader = ParquetReader::new(f);
            let total = reader.num_rows()?;
            let cols = reader.schema()?.len();
            Ok((Some(total), cols))
        }
        Format::Csv => {
            let mut lf = LazyCsvReader::new(path).finish()?;
            let schema = lf.collect_schema()?;
            Ok((None, schema.len()))
        }
    }
}

pub fn fetch_df(path: &PathBuf, fmt: &Format, n: usize, mode: Mode, total_rows: Option<usize>) -> PolarsResult<DataFrame> {
    let lf = new_lazy_frame(path, fmt);
    match mode {
        Mode::Head => lf.fetch(n),
        Mode::Tail => match fmt {
            Format::Parquet => {
                let total = total_rows.expect("parquet always has total_rows");
                let offset = (total as i64).saturating_sub(n as i64);
                lf.slice(offset, n as u32).collect()
            }
            Format::Csv => Ok(lf.collect()?.tail(Some(n))),
        },
    }
}

pub fn preview(
    path: &PathBuf,
    fmt: &Format,
    n: usize,
    mode: Mode,
    remote: bool,
    lazy: bool,
) -> Result<(Option<usize>, usize, DataFrame), Box<dyn std::error::Error>> {
    // CSV --lazy: fast path, no full scan/download
    if matches!(fmt, Format::Csv) && lazy {
        match mode {
            Mode::Tail => return Err("CSV tail requires full scan; remove --lazy to enable".into()),
            Mode::Head => {
                let mut lf = new_lazy_frame(path, fmt);
                let n_cols = lf.collect_schema()?.len();
                let df = lf.fetch(n)?;
                return Ok((None, n_cols, df));
            }
        }
    }

    // CSV eager (default): full scan/download — gives total rows and supports tail
    if matches!(fmt, Format::Csv) {
        let df = new_lazy_frame(path, fmt).collect()?;
        let total_rows = df.height();
        let n_cols = df.width();
        let result_df = match mode {
            Mode::Head => df.head(Some(n)),
            Mode::Tail => df.tail(Some(n)),
        };
        return Ok((Some(total_rows), n_cols, result_df));
    }

    // Parquet remote: read footer for row count, slice pushdown for data
    if remote {
        let mut lf = new_lazy_frame(path, fmt);
        let n_cols = lf.collect_schema()?.len();
        let (total_rows, df) = match mode {
            Mode::Head => {
                let count_df = lf.clone().select([len()]).collect()?;
                let total_rows = count_df.get_columns()[0].u32()?.get(0).unwrap_or(0) as usize;
                (Some(total_rows), lf.fetch(n)?)
            }
            Mode::Tail => {
                let count_df = lf.clone().select([len()]).collect()?;
                let total_rows = count_df.get_columns()[0].u32()?.get(0).unwrap_or(0) as usize;
                let offset = (total_rows as i64).saturating_sub(n as i64);
                let df = lf.slice(offset, n as u32).collect()?;
                (Some(total_rows), df)
            }
        };
        return Ok((total_rows, n_cols, df));
    }

    // Parquet local
    let (total_rows, n_cols) = metadata(path, fmt)?;
    let df = fetch_df(path, fmt, n, mode, total_rows)?;
    Ok((total_rows, n_cols, df))
}

pub fn is_remote_source(path: &PathBuf) -> bool {
    let raw = path.to_string_lossy();
    let raw = raw.split(['?', '#']).next().unwrap_or(&raw);
    raw.contains("://")
}

fn new_lazy_frame(path: &PathBuf, fmt: &Format) -> LazyFrame {
    match fmt {
        Format::Parquet => LazyFrame::scan_parquet(path, ScanArgsParquet::default()).unwrap(),
        Format::Csv     => LazyCsvReader::new(path).finish().unwrap(),
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
    fn remote_source_detection() {
        assert!(is_remote_source(&PathBuf::from("https://example.com/data.parquet")));
        assert!(is_remote_source(&PathBuf::from("s3://bucket/data.parquet")));
        assert!(!is_remote_source(&PathBuf::from("examples/titanic.parquet")));
    }

    #[test]
    fn head_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 7, Mode::Head, false, false).unwrap();
        assert_eq!(total, Some(891));
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn tail_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 7, Mode::Tail, false, false).unwrap();
        assert_eq!(total, Some(891));
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn head_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 10, Mode::Head, false, false).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn tail_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 10, Mode::Tail, false, false).unwrap();
        assert_eq!(total, Some(150));
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn lazy_csv_head_no_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _, df) = preview(&path, &fmt, 5, Mode::Head, false, true).unwrap();
        assert_eq!(total, None);
        assert_eq!(df.height(), 5);
    }

    #[test]
    fn lazy_csv_tail_errors() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        assert!(preview(&path, &fmt, 5, Mode::Tail, false, true).is_err());
    }

    #[test]
    fn head_and_tail_parquet_differ() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (head_total, _, head) = preview(&path, &fmt, 3, Mode::Head, false, false).unwrap();
        let (tail_total, _, tail) = preview(&path, &fmt, 3, Mode::Tail, false, false).unwrap();
        assert_eq!(head_total, Some(891));
        assert_eq!(tail_total, Some(891));
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }

    #[test]
    fn head_and_tail_csv_differ() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (head_total, _, head) = preview(&path, &fmt, 3, Mode::Head, false, false).unwrap();
        let (tail_total, _, tail) = preview(&path, &fmt, 3, Mode::Tail, false, false).unwrap();
        assert_eq!(head_total, Some(150));
        assert_eq!(tail_total, Some(150));
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }
}
