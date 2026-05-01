mod highlight;

use std::io::IsTerminal;
use std::path::PathBuf;
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
        Some(SubCmd::Tail { file, n }) => run(&file, n, Mode::Tail, colorize),
        None => match cli.file {
            Some(file) => run(&file, cli.n, Mode::Head, colorize),
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

fn run(path: &PathBuf, n: usize, mode: Mode, colorize: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fmt = detect_format(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let (total_rows, n_cols) = metadata(path, &fmt)?;

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

    let df = fetch_df(path, &fmt, n, mode, total_rows)?;
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

fn new_lazy_frame(path: &PathBuf, fmt: &Format) -> LazyFrame {
    match fmt {
        Format::Parquet => LazyFrame::scan_parquet(path, ScanArgsParquet::default()).unwrap(),
        Format::Csv     => LazyCsvReader::new(path).finish().unwrap(),
    }
}

pub enum Format { Parquet, Csv }

pub fn detect_format(path: &PathBuf) -> Result<Format, &'static str> {
    match path.extension().and_then(|e| e.to_str()) {
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
    fn head_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _) = metadata(&path, &fmt).unwrap();
        let df = fetch_df(&path, &fmt, 7, Mode::Head, total).unwrap();
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn tail_parquet_row_count() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _) = metadata(&path, &fmt).unwrap();
        let df = fetch_df(&path, &fmt, 7, Mode::Tail, total).unwrap();
        assert_eq!(df.height(), 7);
    }

    #[test]
    fn head_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _) = metadata(&path, &fmt).unwrap();
        let df = fetch_df(&path, &fmt, 10, Mode::Head, total).unwrap();
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn tail_csv_row_count() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _) = metadata(&path, &fmt).unwrap();
        let df = fetch_df(&path, &fmt, 10, Mode::Tail, total).unwrap();
        assert_eq!(df.height(), 10);
    }

    #[test]
    fn head_and_tail_parquet_differ() {
        let path = examples_dir().join("titanic.parquet");
        let fmt = detect_format(&path).unwrap();
        let (total, _) = metadata(&path, &fmt).unwrap();
        let head = fetch_df(&path, &fmt, 3, Mode::Head, total).unwrap();
        let tail = fetch_df(&path, &fmt, 3, Mode::Tail, total).unwrap();
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }

    #[test]
    fn head_and_tail_csv_differ() {
        let path = examples_dir().join("iris.csv");
        let fmt = detect_format(&path).unwrap();
        let (total, _) = metadata(&path, &fmt).unwrap();
        let head = fetch_df(&path, &fmt, 3, Mode::Head, total).unwrap();
        let tail = fetch_df(&path, &fmt, 3, Mode::Tail, total).unwrap();
        assert_ne!(head.get(0).unwrap(), tail.get(0).unwrap());
    }
}
