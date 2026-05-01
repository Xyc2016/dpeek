mod highlight;

use std::io::IsTerminal;
use std::path::PathBuf;
use clap::{Parser, builder::Styles};
use owo_colors::OwoColorize;
use polars::prelude::*;
use highlight::rich_highlight;

/// Extremely fast data file peek — preview CSV and Parquet files instantly
#[derive(Parser)]
#[command(styles = help_styles())]
struct Cli {
    /// File to preview
    file: PathBuf,

    /// Number of rows to show
    #[arg(short = 'n', default_value = "5")]
    n: usize,
}

fn help_styles() -> Styles {
    use anstyle::{AnsiColor, Color, Style};
    Styles::styled()
        .usage(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .header(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .literal(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
}

fn main() {
    // fetch(n) already limits rows; set -1 so Polars never truncates the display
    std::env::set_var("POLARS_FMT_MAX_ROWS", "-1");
    let cli = Cli::parse();
    let colorize = std::io::stdout().is_terminal();

    if let Err(e) = run(&cli.file, cli.n, colorize) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn run(path: &PathBuf, n: usize, colorize: bool) -> Result<(), Box<dyn std::error::Error>> {
    let fmt = format(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let (total_rows, n_cols) = match fmt {
        Format::Parquet => {
            let f = std::fs::File::open(path)?;
            let mut reader = ParquetReader::new(f);
            let total = reader.num_rows()?;
            let cols = reader.schema()?.len();
            (Some(total), cols)
        }
        Format::Csv => {
            let mut lf = LazyCsvReader::new(path).finish()?;
            let schema = lf.collect_schema()?;
            (None, schema.len())
        }
    };

    if colorize {
        if let Some(rows) = total_rows {
            println!("{}  {} rows × {} cols  (showing top {})",
                path.display().to_string().bold(), rows, n_cols, n.min(rows));
        } else {
            println!("{}  {} cols  (showing top {})",
                path.display().to_string().bold(), n_cols, n);
        }
    } else {
        if let Some(rows) = total_rows {
            println!("{}  {} rows × {} cols  (showing top {})",
                path.display(), rows, n_cols, n.min(rows));
        } else {
            println!("{}  {} cols  (showing top {})",
                path.display(), n_cols, n);
        }
    }

    let df = new_lazy_frame(path, &fmt).fetch(n)?;
    let text: String = df.to_string().lines().skip(1).collect::<Vec<_>>().join("\n");
    if colorize {
        println!("{}", rich_highlight(&text));
    } else {
        println!("{}", text);
    }
    Ok(())
}

fn new_lazy_frame(path: &PathBuf, fmt: &Format) -> LazyFrame {
    match fmt {
        Format::Parquet => LazyFrame::scan_parquet(path, ScanArgsParquet::default()).unwrap(),
        Format::Csv     => LazyCsvReader::new(path).finish().unwrap(),
    }
}

enum Format { Parquet, Csv }

fn format(path: &PathBuf) -> Result<Format, &'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("parquet") => Ok(Format::Parquet),
        Some("csv")     => Ok(Format::Csv),
        _               => Err("unsupported format: expected .parquet or .csv"),
    }
}
