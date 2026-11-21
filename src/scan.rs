use crate::archive::{self, Archive, gaps};
use crate::carve::{self, KnownFormat};
use crate::error::{Error, Result};
use crate::zlib::{CHUNK, Inflater};
use crate::{ALIGNMENT, SECTOR_SIZE};
use std::io::{self, Write};
use std::ops::Range;

const RESOURCE_HEADER: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Resource payload.
    Resource,
    /// A known media or text format located by signature.
    Known(KnownFormat),
    /// Stored bytes whose format could not be identified.
    Unknown,
}

impl FileKind {
    /// Suggested file extension.
    pub fn ext(&self) -> &'static str {
        match self {
            Self::Resource => "rsc",
            Self::Known(format) => format.ext,
            Self::Unknown => "bin",
        }
    }

    /// Human-readable format name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::Known(format) => format.name,
            Self::Unknown => "unknown",
        }
    }

    /// Format-specific note, or an empty string.
    pub fn note(&self) -> &str {
        match self {
            Self::Known(format) => &format.note,
            _ => "",
        }
    }
}

/// One physical span of a carved file. A joined resource may have several
/// zlib chunks.
#[derive(Clone, Debug)]
pub struct Part {
    /// Absolute range in the scanned buffer.
    pub range: Range<usize>,
    /// Inflated length when the span is a zlib stream.
    pub inflated: Option<u64>,
}

/// A file carved out of the scanned buffer.
#[derive(Clone, Debug)]
pub struct CarvedFile {
    /// Absolute span covering every part, padding between parts included.
    pub span: Range<usize>,
    pub parts: Vec<Part>,
    pub kind: FileKind,
    /// Header offset of the innermost archive whose body contains the file.
    pub owner: Option<usize>,
}

impl CarvedFile {
    fn new(range: Range<usize>, inflated: Option<u64>, kind: FileKind) -> Self {
        Self {
            span: range.clone(),
            parts: vec![Part { range, inflated }],
            kind,
            owner: None,
        }
    }

    /// Total extracted size: inflated lengths plus raw span lengths.
    pub fn size(&self) -> u64 {
        self.parts
            .iter()
            .map(|part| part.inflated.unwrap_or(part.range.len() as u64))
            .sum()
    }

    /// Writes the extracted contents in part order and returns the byte count.
    pub fn write_to<W: Write>(&self, data: &[u8], out: &mut W) -> Result<u64> {
        let mut written = 0u64;
        for part in &self.parts {
            let bytes = &data[part.range.clone()];
            match part.inflated {
                Some(expected) => {
                    let mut decoder = flate2::read::ZlibDecoder::new(bytes);
                    let mut sink = Counting { out, bytes: 0 };
                    io::copy(&mut decoder, &mut sink).map_err(|e| Error::Zlib {
                        at: part.range.start,
                        message: e.to_string(),
                    })?;
                    if sink.bytes != expected {
                        return Err(Error::InflatedSize {
                            at: part.range.start,
                            expected,
                            actual: sink.bytes,
                        });
                    }
                    written += sink.bytes;
                }
                None => {
                    out.write_all(bytes)?;
                    written += bytes.len() as u64;
                }
            }
        }
        Ok(written)
    }

    /// Extracts the contents into a new buffer.
    pub fn read_to_vec(&self, data: &[u8]) -> Result<Vec<u8>> {
        let size = self.size();
        let capacity = usize::try_from(size).map_err(|_| Error::TooLarge { size })?;
        let mut bytes = Vec::with_capacity(capacity);
        self.write_to(data, &mut bytes)?;
        Ok(bytes)
    }

    fn continues_in(&self, next: &CarvedFile, data: &[u8]) -> bool {
        let inflated = |file: &CarvedFile| file.parts.last().and_then(|part| part.inflated);
        let chunked = inflated(self) == Some(CHUNK as u64) && inflated(next).is_some();
        let adjacent = self.span.end.next_multiple_of(ALIGNMENT) == next.span.start;
        let starts_file = !matches!(next.kind, FileKind::Unknown) || behind_header(data, next);
        adjacent && (matches!(self.kind, FileKind::Resource) || chunked) && !starts_file
    }
}

struct Counting<'a, W: Write> {
    out: &'a mut W,
    bytes: u64,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.out.write(buf)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Byte accounting over the scanned buffer.
///
/// For archives whose carved spans do not overlap container structure the six
/// fields sum to `data.len()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub structure: u64,
    pub zlib: u64,
    pub media: u64,
    pub stored: u64,
    pub padding: u64,
    pub unexplained: u64,
}

/// A scanned buffer: archives, carved files, and coverage.
pub struct Scan<'a> {
    data: &'a [u8],
    archives: Vec<Archive<'a>>,
    files: Vec<CarvedFile>,
    coverage: Coverage,
}

impl<'a> Scan<'a> {
    /// Carves every recognizable payload, discovers archives, assigns stored
    /// spans inside archive bodies, joins resource chunks, and accounts for
    /// every byte.
    pub fn new(data: &'a [u8]) -> Self {
        let mut pieces = recognized(data);
        let known: Vec<_> = pieces.iter().map(|piece| piece.span.clone()).collect();
        let archives = archive::discover(data, &known);
        pieces.extend(stored(data, &archives, &known));
        pieces.sort_by_key(|piece| piece.span.start);

        let mut files = joined(data, pieces);
        for file in &mut files {
            file.owner = archives
                .iter()
                .rfind(|archive| archive.body.contains(&file.span.start))
                .map(|archive| archive.header.at);
        }
        let coverage = coverage(data, &archives, &files);
        Self {
            data,
            archives,
            files,
            coverage,
        }
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn archives(&self) -> &[Archive<'a>] {
        &self.archives
    }

    pub fn files(&self) -> &[CarvedFile] {
        &self.files
    }

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// Header offset of the innermost archive whose body contains `at`.
    pub fn owner(&self, at: usize) -> Option<usize> {
        self.archives
            .iter()
            .rfind(|archive| archive.body.contains(&at))
            .map(|archive| archive.header.at)
    }
}

/// Byte-scans for media signatures and zlib streams.
fn recognized(data: &[u8]) -> Vec<CarvedFile> {
    let mut inflater = Inflater::new();
    let (mut pieces, mut at) = (Vec::new(), 0);
    while at < data.len() {
        let rest = &data[at..];
        let (len, inflated, kind) = if let Some((len, media)) = carve::probe(rest) {
            (len, None, FileKind::Known(media))
        } else if let Some((consumed, len)) = inflater.inflate(rest) {
            (consumed, Some(len), signature(&inflater.out))
        } else {
            at += 1;
            continue;
        };
        pieces.push(CarvedFile::new(at..at + len, inflated, kind));
        at += len;
    }
    pieces
}

/// Classifies inflated content by signature.
fn signature(content: &[u8]) -> FileKind {
    let known = |ext, name| {
        FileKind::Known(KnownFormat {
            ext,
            name,
            note: String::new(),
        })
    };
    let text = content.strip_prefix(b"\xef\xbb\xbf").unwrap_or(content);
    if let Some((_, media)) = carve::probe(content) {
        FileKind::Known(media)
    } else if content.starts_with(b"PSIN") {
        known("pso", "PSO")
    } else if text.starts_with(b"<?xml") {
        known("xml", "XML")
    } else {
        FileKind::Unknown
    }
}

/// Emits stored spans from non-zero gaps inside archive bodies.
fn stored(data: &[u8], archives: &[Archive<'_>], known: &[Range<usize>]) -> Vec<CarvedFile> {
    let mut claimed: Vec<_> = archives.iter().map(Archive::extent).collect();
    claimed.extend_from_slice(known);
    claimed.sort_by_key(|range| range.start);

    let mut stored = Vec::new();
    let used = |&byte: &u8| byte != 0;
    for archive in archives {
        let body = &archive.body;
        let inside = claimed.iter().filter(|range| body.contains(&range.start));
        for gap in gaps(body, inside) {
            let bytes = &data[gap.clone()];
            if let Some(first) = bytes.iter().position(used)
                && let Some(last) = bytes.iter().rposition(used)
            {
                let from = (gap.start + first) / ALIGNMENT * ALIGNMENT;
                let to = (gap.start + last + 1).next_multiple_of(ALIGNMENT);
                stored.push(CarvedFile::new(
                    from.max(gap.start)..to.min(gap.end),
                    None,
                    FileKind::Unknown,
                ));
            }
        }
    }
    stored
}

/// A resource chunk is preceded by 16 zero bytes at a sector-relative offset.
fn behind_header(data: &[u8], file: &CarvedFile) -> bool {
    let at = file.span.start;
    at % SECTOR_SIZE == RESOURCE_HEADER
        && data[at - RESOURCE_HEADER..at].iter().all(|&byte| byte == 0)
}

/// Merges adjacent chunks into resources and promotes stored resources.
fn joined(data: &[u8], pieces: Vec<CarvedFile>) -> Vec<CarvedFile> {
    let mut files: Vec<CarvedFile> = Vec::new();
    for mut piece in pieces {
        match files.last_mut() {
            Some(file) if file.continues_in(&piece, data) => {
                file.span.end = piece.span.end;
                file.parts.append(&mut piece.parts);
            }
            _ => {
                if matches!(piece.kind, FileKind::Unknown) && behind_header(data, &piece) {
                    piece.kind = FileKind::Resource;
                }
                files.push(piece);
            }
        }
    }
    files
}

fn coverage(data: &[u8], archives: &[Archive<'_>], files: &[CarvedFile]) -> Coverage {
    let mut coverage = Coverage::default();
    let mut claimed: Vec<_> = archives.iter().flat_map(Archive::structure).collect();
    coverage.structure = claimed.iter().map(|range| range.len() as u64).sum();
    for file in files {
        for part in &file.parts {
            let bytes = match (part.inflated, &file.kind) {
                (Some(_), _) => &mut coverage.zlib,
                (None, FileKind::Known(_)) => &mut coverage.media,
                (None, _) => &mut coverage.stored,
            };
            *bytes += part.range.len() as u64;
            claimed.push(part.range.clone());
        }
    }
    claimed.sort_by_key(|range| range.start);
    for gap in gaps(&(0..data.len()), &claimed) {
        let zeros = data[gap.clone()].iter().filter(|&&byte| byte == 0).count() as u64;
        coverage.padding += zeros;
        coverage.unexplained += gap.len() as u64 - zeros;
    }
    coverage
}
