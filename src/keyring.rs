//! Archive key discovery and table decryption.

use crate::crypto::{KeySchedule, KEY_COUNT, TAG_IV};
use crate::entry::{self, Entry};
use crate::error::{Error, Result};
use crate::header::Header;

/// Minimum number of table records that must be recovered for a key to be
/// considered plausible when an archive declares no entries.
const MIN_RUN: usize = 1;

/// Result of recovering an archive's cleartext table region.
#[derive(Clone)]
pub struct Tables {
    /// Decrypted `0x40..payload_start` region (offsets match the file).
    pub plain: Vec<u8>,
    /// Index into the embedded key set that produced `plain`.
    pub key_index: usize,
    /// Whether the graph header magic was found.
    pub graph: bool,
    /// Offset of the entry table relative to the start of the archive.
    pub table_at: usize,
    /// Number of records in the entry table.
    pub records: usize,
}

impl Tables {
    /// Recovers the tables of `data` using the header's declared size.
    pub fn recover(data: &[u8], header: &Header) -> Result<Self> {
        let end = header.payload_start as usize;
        if end > data.len() || end <= crate::HEADER_SIZE {
            return Err(Error::InvalidLayout {
                reason: "header size outside archive",
            });
        }
        let ciphertext = &data[crate::HEADER_SIZE..end];
        let declared = header.entry_count as usize;
        let mut best: Option<(usize, usize, bool, Vec<u8>)> = None;
        let mut best_index = 0;
        for index in 0..KEY_COUNT {
            let schedule = KeySchedule::from_index(index)?;
            let mut plain = ciphertext.to_vec();
            schedule.decrypt_cbc(&mut plain, &TAG_IV);
            let (start, run) = entry::detect(&plain, data.len() as u64, declared);
            let graph = entry::has_graph_magic(&plain);
            let score = run + usize::from(graph) * 4;
            let replace = match &best {
                None => true,
                Some((best_score, ..)) => score > *best_score,
            };
            if replace {
                best = Some((score, start, graph, plain));
                best_index = index;
            }
        }
        let (_, start, graph, plain) = best.expect("at least one key is embedded");
        let (table_at, records) = entry::detect(&plain, data.len() as u64, declared);
        if records < MIN_RUN && declared != 0 {
            return Err(Error::InvalidLayout {
                reason: "no table records recovered",
            });
        }
        let _ = start;
        Ok(Self {
            plain,
            key_index: best_index,
            graph,
            table_at: crate::HEADER_SIZE + table_at,
            records,
        })
    }

    /// Extracts the file entries from the recovered table region.
    pub fn entries(&self, data: &[u8], header: &Header) -> Result<Vec<Entry>> {
        let declared = header.entry_count as usize;
        let count = if declared == 0 {
            self.records
        } else {
            declared.min(self.records)
        };
        let skip = usize::from(declared != 0 && self.records == declared + 1);
        let trailer = header.trailer_size as u64;
        let end = (data.len() as u64).saturating_sub(trailer);
        let mut entries = entry::parse(
            &self.plain,
            self.table_at - crate::HEADER_SIZE + skip * entry::RECORD,
            self.records.saturating_sub(skip),
            count,
            end,
        );
        entry::assign_sizes(&mut entries, end);
        Ok(entries)
    }
}
