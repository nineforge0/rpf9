//! Inspect and extract an RPF9 archive from the command line.
//!
//! ```sh
//! cargo run --example cli -- path/to/archive.rpf
//! cargo run --example cli -- path/to/archive.rpf --extract out
//! cargo run --example cli -- path/to/archive.rpf --write rebuilt.rpf
//! ```

use clap::Parser;
use rpf9::{ArchiveWriter, Scan};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "rpf9-cli",
    bin_name = "rpf9-cli",
    version,
    about = "Inspect and extract GTA VI RPF9 archives"
)]
struct Cli {
    /// Archive to inspect.
    archive: PathBuf,

    /// Extract carved files into this directory.
    #[arg(short, long, value_name = "DIR")]
    extract: Option<PathBuf>,

    /// Write the first archive's container (header, tables, body, trailer).
    #[arg(short, long, value_name = "FILE")]
    write: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let file = rpf9::MappedFile::open(&cli.archive)?;
    let scan = Scan::new(file.as_slice());
    report(&cli.archive, &scan);

    if let Some(dir) = &cli.extract {
        extract(&scan, dir)?;
    }
    if let Some(path) = &cli.write {
        let archive = scan
            .archives()
            .first()
            .ok_or_else(|| format!("{}: no archive found", cli.archive.display()))?;
        let bytes = ArchiveWriter::from_archive(archive).to_vec()?;
        fs::write(path, &bytes)?;
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

fn report(path: &Path, scan: &Scan<'_>) {
    println!("{} ({} bytes)", path.display(), scan.data().len());

    println!("archives: {}", scan.archives().len());
    for (index, archive) in scan.archives().iter().enumerate() {
        let header = &archive.header;
        println!(
            "  #{index} at {:#x}: version {} platform {} files {} id {:#x} group {:#x} flags {:#x}",
            header.at,
            header.version,
            header.platform,
            header.file_count,
            header.id,
            header.group_id,
            header.flags
        );
        println!(
            "       header {:#x} split {:#x} trailer {:#x} | body {:#x}..{:#x} trailer {:#x}..{:#x} | records {}/{}",
            header.header_size,
            header.split,
            header.trailer_size,
            archive.body.start,
            archive.body.end,
            archive.trailer.start,
            archive.trailer.end,
            archive.first_records(),
            archive.second_records()
        );
    }

    println!("files: {}", scan.files().len());
    for (index, file) in scan.files().iter().enumerate() {
        let owner = file
            .owner
            .map_or_else(|| "-".to_owned(), |at| format!("{at:#x}"));
        println!(
            "  #{index:<3} {:<5} {:>10}  {:#010x}..{:#010x}  owner {:<8} parts {}  {}",
            file.kind.ext(),
            human(file.size()),
            file.span.start,
            file.span.end,
            owner,
            file.parts.len(),
            file.kind.name()
        );
    }

    let coverage = scan.coverage();
    println!(
        "coverage: structure {} zlib {} media {} stored {} padding {} unexplained {}",
        coverage.structure,
        coverage.zlib,
        coverage.media,
        coverage.stored,
        coverage.padding,
        coverage.unexplained
    );
}

fn extract(scan: &Scan<'_>, dir: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(dir)?;
    for (index, file) in scan.files().iter().enumerate() {
        let name = format!("{index:04}_{:08x}.{}", file.span.start, file.kind.ext());
        let path = dir.join(name);
        let bytes = file.read_to_vec(scan.data())?;
        fs::write(&path, &bytes)?;
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
