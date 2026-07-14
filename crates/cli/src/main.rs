mod benchmark_test;

use std::path::Path;
use std::process;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use text_document::{FindOptions, TextDocument};

#[derive(Parser)]
#[command(
    name = "text-document",
    about = "Rich text document converter and processor",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert a document between formats (detected by file extension)
    Convert {
        /// Input file (.txt, .md, .html, .htm)
        input: String,
        /// Output file (.txt, .md, .html, .htm, .tex, .latex, .docx)
        output: String,
        /// LaTeX document class (only for .tex output)
        #[arg(long, default_value = "article")]
        document_class: String,
        /// Include LaTeX preamble (only for .tex output)
        #[arg(long)]
        preamble: bool,
    },

    /// Show document statistics
    Stats {
        /// Input file
        file: String,
    },

    /// Find text occurrences (grep-like output)
    Find {
        /// Input file
        file: String,
        /// Search query
        query: String,
        #[arg(long, short = 'c')]
        case_sensitive: bool,
        #[arg(long, short = 'w')]
        whole_word: bool,
        #[arg(long, short = 'e')]
        regex: bool,
    },

    /// Find and replace text
    Replace {
        /// Input file
        file: String,
        /// Search query
        query: String,
        /// Replacement text
        replacement: String,
        /// Output file (defaults to overwriting input)
        #[arg(short, long)]
        output: Option<String>,
        #[arg(long, short = 'c')]
        case_sensitive: bool,
        #[arg(long, short = 'w')]
        whole_word: bool,
        #[arg(long, short = 'e')]
        regex: bool,
    },

    /// Print document content to stdout in a given format
    Cat {
        /// Input file
        file: String,
        /// Output format
        #[arg(short, long, default_value = "plain")]
        format: OutputFormat,
    },

    Test,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Plain,
    Markdown,
    Html,
    Latex,
}

// ── Format detection ────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum FileFormat {
    PlainText,
    Markdown,
    Html,
    Latex,
    Docx,
}

fn detect_format(path: &str) -> FileFormat {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => FileFormat::Markdown,
        Some("html" | "htm") => FileFormat::Html,
        Some("tex" | "latex") => FileFormat::Latex,
        Some("docx") => FileFormat::Docx,
        _ => FileFormat::PlainText,
    }
}

// ── Document loading ────────────────────────────────────────────

fn load_document(path: &str) -> Result<TextDocument> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("failed to read '{path}'"))?;
    let doc = TextDocument::new();
    match detect_format(path) {
        FileFormat::PlainText => {
            doc.set_plain_text(&content)?;
        }
        FileFormat::Markdown => {
            doc.set_markdown(&content)?
                .wait()
                .context("markdown import failed")?;
        }
        FileFormat::Html => {
            doc.set_html(&content)?
                .wait()
                .context("HTML import failed")?;
        }
        other => bail!("unsupported input format: {}", format_name(other)),
    }
    Ok(doc)
}

fn format_name(fmt: FileFormat) -> &'static str {
    match fmt {
        FileFormat::PlainText => "plain text",
        FileFormat::Markdown => "markdown",
        FileFormat::Html => "HTML",
        FileFormat::Latex => "LaTeX",
        FileFormat::Docx => "DOCX",
    }
}

// ── Command handlers ────────────────────────────────────────────

fn cmd_convert(input: &str, output: &str, document_class: &str, preamble: bool) -> Result<()> {
    let doc = load_document(input)?;
    let out_format = detect_format(output);

    match out_format {
        FileFormat::PlainText => {
            let text = doc.to_plain_text()?;
            std::fs::write(output, text)?;
        }
        FileFormat::Markdown => {
            let text = doc.to_markdown()?;
            std::fs::write(output, text)?;
        }
        FileFormat::Html => {
            let text = doc.to_html()?;
            std::fs::write(output, text)?;
        }
        FileFormat::Latex => {
            let text = doc.to_latex(document_class, preamble)?;
            std::fs::write(output, text)?;
        }
        FileFormat::Docx => {
            doc.to_docx(output)?.wait().context("DOCX export failed")?;
        }
    }

    eprintln!("{} -> {} ({})", input, output, format_name(out_format));
    Ok(())
}

fn cmd_stats(file: &str) -> Result<()> {
    let doc = load_document(file)?;
    let stats = doc.stats();
    println!("File:       {file}");
    println!("Characters: {}", stats.character_count);
    println!("Words:      {}", stats.word_count);
    println!("Blocks:     {}", stats.block_count);
    println!("Frames:     {}", stats.frame_count);
    println!("Images:     {}", stats.image_count);
    println!("Lists:      {}", stats.list_count);
    println!("Tables:     {}", stats.table_count);
    Ok(())
}

fn cmd_find(
    file: &str,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<()> {
    let doc = load_document(file)?;
    let opts = FindOptions {
        case_sensitive,
        whole_word,
        use_regex,
        search_backward: false,
    };
    let matches = doc.find_all(query, &opts)?;

    if matches.is_empty() {
        eprintln!("no matches found");
        return Ok(());
    }

    for m in &matches {
        let context_start = m.position.saturating_sub(20);
        let context_len = (m.length + 40).min(200);
        let text = doc.text_at(context_start, context_len).unwrap_or_default();
        let offset_in_context = m.position - context_start;
        let display = text.replace('\n', "\\n");
        println!("{}:{} {}", m.position, m.length, display);
        let _ = offset_in_context; // available for future highlighting
    }

    eprintln!("{} match(es) found", matches.len());
    Ok(())
}

fn cmd_replace(
    file: &str,
    query: &str,
    replacement: &str,
    output: Option<&str>,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<()> {
    let doc = load_document(file)?;
    let opts = FindOptions {
        case_sensitive,
        whole_word,
        use_regex,
        search_backward: false,
    };
    let count = doc.replace_text(
        query,
        replacement,
        true,
        &text_document::ReplaceOptions::new(opts.clone()),
    )?;

    if count == 0 {
        eprintln!("no matches found, file unchanged");
        return Ok(());
    }

    let out_path = output.unwrap_or(file);
    let out_format = detect_format(out_path);
    let content = match out_format {
        FileFormat::PlainText => doc.to_plain_text()?,
        FileFormat::Markdown => doc.to_markdown()?,
        FileFormat::Html => doc.to_html()?,
        _ => doc.to_plain_text()?,
    };
    std::fs::write(out_path, content)?;
    eprintln!("{count} replacement(s), written to {out_path}");
    Ok(())
}

fn cmd_cat(file: &str, format: &OutputFormat) -> Result<()> {
    let doc = load_document(file)?;
    let content = match format {
        OutputFormat::Plain => doc.to_plain_text()?,
        OutputFormat::Markdown => doc.to_markdown()?,
        OutputFormat::Html => doc.to_html()?,
        OutputFormat::Latex => doc.to_latex("article", false)?,
    };
    print!("{content}");
    Ok(())
}

// ── Main ────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let result = match &cli.command {
        Commands::Convert {
            input,
            output,
            document_class,
            preamble,
        } => cmd_convert(input, output, document_class, *preamble),

        Commands::Stats { file } => cmd_stats(file),

        Commands::Find {
            file,
            query,
            case_sensitive,
            whole_word,
            regex,
        } => cmd_find(file, query, *case_sensitive, *whole_word, *regex),

        Commands::Replace {
            file,
            query,
            replacement,
            output,
            case_sensitive,
            whole_word,
            regex,
        } => cmd_replace(
            file,
            query,
            replacement,
            output.as_deref(),
            *case_sensitive,
            *whole_word,
            *regex,
        ),

        Commands::Cat { file, format } => cmd_cat(file, format),
        Commands::Test => benchmark_test::run_benchmark_test(),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}
