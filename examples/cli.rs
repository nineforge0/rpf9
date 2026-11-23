//! Inspect, preview and extract an RPF9 archive from the command line.
//!
//! ```sh
//! cargo run --example cli -- path/to/archive.rpf
//! cargo run --example cli -- path/to/archive.rpf --list
//! cargo run --example cli -- path/to/archive.rpf --extract out --select 1-10
//! cargo run --example cli -- path/to/archive.rpf --dump @0x81430
//! cargo run --example cli -- path/to/archive.rpf --hexdump 0 --hex-limit 256
//! cargo run --example cli -- path/to/archive.rpf --text 0
//! cargo run --example cli -- path/to/archive.rpf --copy rebuilt.rpf
//! ```

use clap::{Parser, ValueEnum};
use rpf9::{Entry, Rpf};
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Kind {
    /// zlib-compressed payload
    Zlib,
    /// stored payload
    Raw,
}

#[derive(Parser, Debug)]
#[command(
    name = "rpf9-cli",
    bin_name = "rpf9-cli",
    version,
    about = "Inspect, preview and extract GTA VI RPF9 archives"
)]
struct Cli {
    /// Archive to inspect.
    archive: PathBuf,

    /// Print every entry (or every filtered entry).
    #[arg(short, long)]
    list: bool,

    /// Restrict to entries matching a selector: `1,3-7` or `@0x81430`.
    #[arg(short, long, value_name = "SELECTOR")]
    select: Option<String>,

    /// Restrict to zlib or raw payloads.
    #[arg(long, value_enum)]
    kind: Option<Kind>,

    /// Restrict by physical size.
    #[arg(long, value_name = "BYTES")]
    min_size: Option<u64>,

    /// Restrict by physical size.
    #[arg(long, value_name = "BYTES")]
    max_size: Option<u64>,

    /// Print at most this many entries with --list.
    #[arg(long, default_value_t = usize::MAX)]
    limit: usize,

    /// Extract matching entries into this directory.
    #[arg(short, long, value_name = "DIR")]
    extract: Option<PathBuf>,

    /// Write the decoded bytes of one entry to stdout.
    #[arg(long, value_name = "SELECTOR")]
    dump: Option<String>,

    /// Write a hex dump of one entry to stdout.
    #[arg(long, value_name = "SELECTOR")]
    hexdump: Option<String>,

    /// Write a text preview of one entry to stdout.
    #[arg(long, value_name = "SELECTOR")]
    text: Option<String>,

    /// Limit a hex dump to this many bytes.
    #[arg(long, default_value_t = 4096)]
    hex_limit: usize,

    /// Print entries as JSON.
    #[arg(long)]
    json: bool,

    /// Copy the archive byte-for-byte.
    #[arg(short, long, value_name = "FILE")]
    copy: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let mapped = rpf9::MappedFile::open(&cli.archive)?;
    let rpf = mapped.parse()?;

    let quiet = cli.dump.is_some() || cli.hexdump.is_some() || cli.text.is_some();
    if !cli.json && !quiet {
        report(&cli.archive, &rpf);
    }

    let mut entries: Vec<&Entry> = rpf.entries().iter().collect();
    if let Some(selector) = selector(&cli) {
        let indices = parse_selector(&selector, &rpf)?;
        entries = indices
            .into_iter()
            .filter_map(|i| rpf.entries().get(i))
            .collect();
    }
    if let Some(kind) = cli.kind {
        entries.retain(|entry| {
            let zlib = entry.is_zlib(rpf.data());
            match kind {
                Kind::Zlib => zlib,
                Kind::Raw => !zlib,
            }
        });
    }
    if let Some(min) = cli.min_size {
        entries.retain(|entry| entry.size >= min);
    }
    if let Some(max) = cli.max_size {
        entries.retain(|entry| entry.size <= max);
    }

    if cli.json {
        print_json(&entries)?;
    }

    if cli.list {
        println!("entries:");
        for entry in entries.iter().take(cli.limit) {
            print_entry(&rpf, entry);
        }
        if entries.len() > cli.limit {
            println!("  … {} more", entries.len() - cli.limit);
        }
    }

    if let Some(dir) = &cli.extract {
        extract(&rpf, &entries, dir)?;
    }
    for (selector, mode) in [
        (cli.dump.as_deref(), Output::Raw),
        (cli.hexdump.as_deref(), Output::Hex),
        (cli.text.as_deref(), Output::Text),
    ] {
        if let Some(selector) = selector {
            let indices = parse_selector(selector, &rpf)?;
            let [index] = indices.as_slice() else {
                return Err("dump/hexdump/text require exactly one entry".into());
            };
            let entry = rpf.entries().get(*index).ok_or("entry index out of range")?;
            let bytes = rpf.read(entry)?;
            write_output(&bytes, mode, cli.hex_limit)?;
        }
    }

    if let Some(path) = &cli.copy {
        let mut bytes = Vec::new();
        rpf.write_identical(&mut bytes)?;
        fs::write(path, &bytes)?;
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

fn selector(cli: &Cli) -> Option<String> {
    cli.select
        .clone()
        .or_else(|| cli.dump.clone())
        .or_else(|| cli.hexdump.clone())
        .or_else(|| cli.text.clone())
}

fn report(path: &Path, rpf: &Rpf<'_>) {
    let header = rpf.header();
    println!(
        "{} ({} bytes, key #{}, graph {})",
        path.display(),
        rpf.data().len(),
        rpf.key_index(),
        rpf.has_graph()
    );
    println!(
        "  version {}.{} gen {:#x} files {} group {:#x} flags {:#x}",
        header.version_high,
        header.version_low,
        header.generation(),
        header.entry_count,
        header.profile_id,
        header.flags
    );
    println!(
        "  payload {:#x} split {:#x} trailer {:#x}",
        header.payload_start, header.split, header.trailer_size
    );
    let zlib = rpf
        .entries()
        .iter()
        .filter(|e| e.is_zlib(rpf.data()))
        .count();
    let total: u64 = rpf.entries().iter().map(|e| e.size).sum();
    println!(
        "entries: {} ({} zlib, {} raw, {} bytes physical)",
        rpf.entries().len(),
        zlib,
        rpf.entries().len() - zlib,
        total
    );
}

fn print_entry(rpf: &Rpf<'_>, entry: &Entry) {
    println!(
        "  #{:<4} {:#010x}..{:#010x}  {:>10}  size {:#010x} aux {:#010x} flags {:#06x}{}",
        entry.index,
        entry.offset,
        entry.offset + entry.size,
        entry.size,
        entry.size_hint,
        entry.aux,
        entry.flags,
        if entry.is_zlib(rpf.data()) { "  zlib" } else { "  raw" }
    );
}

fn parse_selector(text: &str, rpf: &Rpf<'_>) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut indices = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(offset) = part.strip_prefix('@') {
            let offset = parse_offset(offset)?;
            let entry = rpf
                .entries()
                .iter()
                .find(|entry| entry.offset == offset)
                .ok_or_else(|| format!("no entry at {offset:#x}"))?;
            indices.push(entry.index);
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start: usize = start.trim().parse()?;
            let end: usize = end.trim().parse()?;
            if start > end {
                return Err(format!("invalid range {part}").into());
            }
            indices.extend(start..=end);
        } else {
            indices.push(part.parse::<usize>()?);
        }
    }
    for &index in &indices {
        if index >= rpf.entries().len() {
            return Err(format!(
                "entry {index} out of range (0..{})",
                rpf.entries().len()
            )
            .into());
        }
    }
    Ok(indices)
}

fn parse_offset(text: &str) -> Result<u64, Box<dyn Error>> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(text.parse()?)
    }
}

fn extract(
    rpf: &Rpf<'_>,
    entries: &[&Entry],
    dir: &Path,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(dir)?;
    let mut written = 0;
    for entry in entries {
        let bytes = rpf.read(entry)?;
        let name = format!("{:08x}.{}", entry.offset, rpf9::reader::sniff_ext(&bytes));
        fs::write(dir.join(name), &bytes)?;
        written += 1;
    }
    println!("extracted {written} entries → {}", dir.display());
    Ok(())
}

#[derive(Copy, Clone)]
enum Output {
    Raw,
    Hex,
    Text,
}

fn write_output(bytes: &[u8], mode: Output, hex_limit: usize) -> Result<(), Box<dyn Error>> {
    let mut out = io::stdout().lock();
    match mode {
        Output::Raw => out.write_all(bytes)?,
        Output::Text => {
            let text = String::from_utf8_lossy(bytes);
            write!(out, "{text}")?;
        }
        Output::Hex => {
            let slice = &bytes[..bytes.len().min(hex_limit)];
            for (row, chunk) in slice.chunks(16).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                let ascii: String = chunk
                    .iter()
                    .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                    .collect();
                writeln!(out, "{:08x}  {:<47}  |{ascii}|", row * 16, hex.join(" "))?;
            }
            if bytes.len() > hex_limit {
                writeln!(out, "… {} bytes total", bytes.len())?;
            }
        }
    }
    Ok(())
}

fn print_json(entries: &[&Entry]) -> Result<(), Box<dyn Error>> {
    let mut out = io::stdout().lock();
    writeln!(out, "[")?;
    for (i, entry) in entries.iter().enumerate() {
        let comma = if i + 1 == entries.len() { "" } else { "," };
        writeln!(
            out,
            "  {{\"index\":{},\"offset\":{},\"size\":{},\"size_hint\":{},\"aux\":{},\"flags\":{}}}{}",
            entry.index,
            entry.offset,
            entry.size,
            entry.size_hint,
            entry.aux,
            entry.flags,
            comma
        )?;
    }
    writeln!(out, "]")?;
    Ok(())
}
