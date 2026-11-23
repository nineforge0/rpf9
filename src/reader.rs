//! High-level RPF9 reader.

use std::collections::BTreeMap;
use std::io::Write;
use std::ops::Range;
use std::path::Path;

use crate::entry::Entry;
use crate::error::{Error, Result};
use crate::header::Header;
use crate::keyring::Tables;

/// A parsed RPF9 archive with its recovered directory table.
#[derive(Clone)]
pub struct Rpf<'a> {
    data: &'a [u8],
    header: Header,
    tables: Tables,
    entries: Vec<Entry>,
}

impl std::fmt::Debug for Rpf<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rpf")
            .field("header", &self.header)
            .field("entries", &self.entries.len())
            .field("key", &self.tables.key_index)
            .finish()
    }
}

impl<'a> Rpf<'a> {
    /// Parses an archive from memory.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let header = Header::parse(data, 0)?;
        let tables = Tables::recover(data, &header)?;
        let entries = tables.entries(data, &header)?;
        Ok(Self {
            data,
            header,
            tables,
            entries,
        })
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Index of the embedded key that decrypts this archive's tables.
    pub fn key_index(&self) -> usize {
        self.tables.key_index
    }

    /// Whether the graph-header magic was recovered.
    pub fn has_graph(&self) -> bool {
        self.tables.graph
    }

    /// Absolute range of an entry's payload.
    pub fn range(&self, entry: &Entry) -> Range<usize> {
        let start = entry.offset as usize;
        start..(start + entry.size as usize).min(self.data.len())
    }

    /// Reads an entry, inflating zlib streams and returning other payloads as
    /// stored. Multi-page resources are stitched across consecutive 64 KiB
    /// zlib pages.
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        const PAGE: usize = 1 << 16;
        let mut out = Vec::new();
        let mut offset = entry.offset as usize;
        let mut remaining = entry.size as usize;
        loop {
            if offset >= self.data.len() || remaining == 0 {
                break;
            }
            let end = (offset + remaining).min(self.data.len());
            let bytes = &self.data[offset..end];
            if !looks_zlib(bytes) {
                out.extend_from_slice(bytes);
                break;
            }
            match inflate(bytes) {
                Ok(page) => {
                    let full = page.len() % PAGE == 0 && page.len() != 0;
                    out.extend_from_slice(&page);
                    if !full {
                        break;
                    }
                }
                Err(_) => {
                    out.extend_from_slice(bytes);
                    break;
                }
            }
            offset = end;
            remaining = self
                .entries
                .iter()
                .find(|e| e.offset as usize == offset)
                .map_or(0, |e| e.size as usize);
            if remaining == 0 {
                break;
            }
        }
        Ok(out)
    }

    /// Writes all entries to `dir`, one file per entry. Names are not stored by
    /// the format on this platform, so outputs are named by offset.
    pub fn extract_all(&self, dir: impl AsRef<Path>) -> Result<usize> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let mut written = 0;
        for entry in &self.entries {
            let bytes = match self.read(entry) {
                Ok(bytes) => bytes,
                Err(_) => self.data[self.range(entry)].to_vec(),
            };
            let name = format!("{:08x}.{}", entry.offset, sniff_ext(&bytes));
            std::fs::write(dir.join(name), &bytes)?;
            written += 1;
        }
        Ok(written)
    }

    /// Writes this archive back byte-for-byte.
    pub fn write_identical<W: Write>(&self, out: &mut W) -> Result<()> {
        out.write_all(self.data)?;
        Ok(())
    }

    /// Rewrites the archive, replacing entry payloads by index.
    ///
    /// Replacements must have exactly the same length as the entry they
    /// replace: the table ciphertext is preserved and offsets do not move.
    pub fn replace_in_place<W: Write>(
        &self,
        replacements: &BTreeMap<usize, Vec<u8>>,
        out: &mut W,
    ) -> Result<()> {
        let mut data = self.data.to_vec();
        for (&index, payload) in replacements {
            let entry = self.entries.get(index).ok_or(Error::InvalidLayout {
                reason: "replacement index outside entry table",
            })?;
            if payload.len() != entry.size as usize {
                return Err(Error::InvalidLayout {
                    reason: "replacement length differs from entry size",
                });
            }
            let range = self.range(entry);
            data[range].copy_from_slice(payload);
        }
        out.write_all(&data)?;
        Ok(())
    }
}

fn looks_zlib(bytes: &[u8]) -> bool {
    let Some(&[cmf, flg]) = bytes.first_chunk::<2>() else {
        return false;
    };
    cmf & 0x0f == 8 && u16::from_be_bytes([cmf, flg]) % 31 == 0
}

fn inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(bytes);
    std::io::copy(&mut decoder, &mut out).map_err(|e| Error::Zlib {
        at: 0,
        message: e.to_string(),
    })?;
    Ok(out)
}

/// Best-effort extension from a payload signature.
pub fn sniff_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"<?xml") || bytes.starts_with(b"\xef\xbb\xbf<?xml") {
        "xml"
    } else if bytes.starts_with(b"PSIN") {
        "pso"
    } else if bytes.starts_with(b"TTPF") || bytes.starts_with(b"ETPF") {
        "ttpf"
    } else if bytes.starts_with(b"ADAT") {
        "awc"
    } else if bytes.starts_with(b"BIK") || bytes.starts_with(b"KB2") {
        "bik"
    } else if bytes.starts_with(b"RIFF") {
        "riff"
    } else {
        "bin"
    }
}
