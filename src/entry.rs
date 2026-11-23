//! RPF9 entry-table recovery.

/// One recovered file entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Record index in the table.
    pub index: usize,
    /// Absolute byte offset in the archive.
    pub offset: u64,
    /// Physical span up to the next entry (or the trailer).
    pub size: u64,
    /// Entry-specific size-like value.
    ///
    /// In stored records this matches the logical payload size closely; for
    /// paged zlib records it is a per-page value, not the inflated length.
    pub size_hint: u32,
    /// Small opaque identifier (probably a name or index handle).
    pub aux: u32,
    /// Per-entry flags (`0x0000ff00` on full archives).
    pub flags: u32,
    /// Raw 24-byte record.
    pub raw: [u8; 24],
}

impl Entry {
    /// Whether the payload starts with a zlib stream.
    pub fn is_zlib(&self, data: &[u8]) -> bool {
        let at = self.offset as usize;
        let Some(head) = data.get(at..at + 2) else {
            return false;
        };
        head[0] & 0x0f == 8 && ((u16::from(head[0]) << 8) | u16::from(head[1])) % 31 == 0
    }
}

pub const RECORD: usize = 24;

/// The fixed graph-header magic found at the start of every archive's node
/// header.
pub const GRAPH_MAGIC: [u8; 16] = [
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x89, 0x09, 0xc5, 0xf2, 0xfa, 0x0d, 0xf7, 0x33,
];

/// Returns whether the decrypted region contains the graph-header magic.
pub fn has_graph_magic(plain: &[u8]) -> bool {
    plain.windows(GRAPH_MAGIC.len()).any(|w| w == GRAPH_MAGIC)
}

/// Locates the entry table.
///
/// Returns `(offset_in_plain, record_count)` for the longest run of
/// structurally valid 24-byte records. The run may contain one sentinel record
/// more than the header's declared count.
pub fn detect(plain: &[u8], file_len: u64, declared: usize) -> (usize, usize) {
    let limit = declared.saturating_add(2);
    let mut best = (0usize, 0usize);
    let mut start = 0usize;
    while start + RECORD <= plain.len() {
        let mut run = 0usize;
        let mut at = start;
        while at + RECORD <= plain.len() && run < limit {
            let Some(rec) = record(&plain[at..]) else {
                break;
            };
            if rec.offset == 0 {
                break;
            }
            let offset = u64::from(rec.offset) * 16;
            if offset >= file_len && u64::from(rec.offset) >= file_len {
                break;
            }
            run += 1;
            at += RECORD;
        }
        if run > best.1 {
            best = (start, run);
        }
        start += 4;
    }
    best
}

struct Fields {
    offset: u32,
}

fn record(bytes: &[u8]) -> Option<Fields> {
    if bytes.len() < RECORD {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let zero0 = word(4);
    let offset = word(8);
    let zero1 = word(20);
    if zero0 != 0 || zero1 != 0 {
        return None;
    }
    Some(Fields { offset })
}

/// Parses `count` records starting at `table_at` in `plain`.
pub fn parse(plain: &[u8], table_at: usize, run: usize, count: usize, end: u64) -> Vec<Entry> {
    let count = count.min(run);
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let at = table_at + index * RECORD;
        let Some(bytes) = plain.get(at..at + RECORD) else {
            break;
        };
        let raw: [u8; 24] = bytes.try_into().unwrap();
        let word = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
        let offset = u64::from(word(8)) * 16;
        if offset >= end {
            continue;
        }
        entries.push(Entry {
            index,
            offset,
            size: 0,
            size_hint: word(0),
            aux: word(16),
            flags: word(12),
            raw,
        });
    }
    entries
}

/// Fills in physical sizes from the sorted offsets.
pub fn assign_sizes(entries: &mut [Entry], end: u64) {
    if entries.is_empty() {
        return;
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].offset);
    order.dedup_by_key(|&mut i| entries[i].offset);
    for pair in order.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        entries[a].size = entries[b].offset.saturating_sub(entries[a].offset);
    }
    let last = *order.last().unwrap();
    entries[last].size = end.saturating_sub(entries[last].offset);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_record_detected() {
        let mut plain = vec![0u8; 64];
        for i in 0..2 {
            let at = 8 + i * RECORD;
            plain[at + 8..at + 12].copy_from_slice(&(0x100u32 + i as u32).to_le_bytes());
            plain[at + 12..at + 16].copy_from_slice(&0xff00u32.to_le_bytes());
        }
        let (start, run) = detect(&plain, 0x10000, 2);
        assert_eq!((start, run), (8, 2));
        let entries = parse(&plain, start, run, run, 0x10000);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].offset, 0x1000);
    }
}
