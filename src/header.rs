use crate::error::{Error, Result};
use crate::{HEADER_SIZE, MAGIC, RECORD_SIZE};

/// A parsed RPF9 header.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    /// Offset of the header in the buffer. Not encoded.
    pub at: usize,
    /// Payload start, including both record regions.
    pub payload_start: u32,
    /// End of the first region / start of the second region.
    pub split: u32,
    /// Trailer length at the end of the archive.
    pub trailer_size: u32,
    /// Archive id.
    pub id: u64,
    /// `(generation << 16) | 0x7A00`.
    pub generation_word: u32,
    /// Constant `0x150` in every observed header.
    pub constant_1c: u32,
    /// High half of the version pair at `0x20`.
    pub version_high: u16,
    /// Low half of the version pair at `0x22`.
    pub version_low: u16,
    /// Declared entry count, directories included.
    pub entry_count: u32,
    /// Profile / key-slot style id.
    pub profile_id: u32,
    /// `0x90F01` or `0x90F02`.
    pub flags: u32,
    /// Constant `0x10` in every observed header.
    pub constant_30: u32,
    /// Zero in every observed header.
    pub zero_34: u32,
    /// Zero in every observed header.
    pub reserved: [u32; 2],
}

impl Header {
    pub fn parse(data: &[u8], at: usize) -> Result<Self> {
        let end = at
            .checked_add(HEADER_SIZE)
            .filter(|&end| end <= data.len())
            .ok_or(Error::InvalidHeader {
                at,
                reason: "truncated header",
            })?;
        let h = &data[at..end];
        if !h.starts_with(&MAGIC) {
            return Err(Error::InvalidHeader {
                at,
                reason: "bad magic",
            });
        }

        let payload_start = u32::from_le_bytes(h[0x04..0x08].try_into().unwrap());
        let split = u32::from_le_bytes(h[0x08..0x0C].try_into().unwrap());

        if (payload_start as usize) < HEADER_SIZE {
            return Err(Error::InvalidHeader {
                at,
                reason: "payload start below 0x40",
            });
        }
        if (split as usize) < HEADER_SIZE {
            return Err(Error::InvalidHeader {
                at,
                reason: "split before header",
            });
        }
        if split > payload_start {
            return Err(Error::InvalidHeader {
                at,
                reason: "split past payload start",
            });
        }
        if at
            .checked_add(payload_start as usize)
            .is_none_or(|end| end > data.len())
        {
            return Err(Error::InvalidHeader {
                at,
                reason: "payload start past buffer",
            });
        }
        if !(split as usize - HEADER_SIZE).is_multiple_of(RECORD_SIZE) {
            return Err(Error::InvalidHeader {
                at,
                reason: "first region is not 16-byte aligned",
            });
        }
        if !((payload_start - split) as usize).is_multiple_of(RECORD_SIZE) {
            return Err(Error::InvalidHeader {
                at,
                reason: "second region is not 16-byte aligned",
            });
        }

        Ok(Self {
            at,
            payload_start,
            split,
            trailer_size: u32::from_le_bytes(h[0x0C..0x10].try_into().unwrap()),
            id: u64::from_le_bytes(h[0x10..0x18].try_into().unwrap()),
            generation_word: u32::from_le_bytes(h[0x18..0x1C].try_into().unwrap()),
            constant_1c: u32::from_le_bytes(h[0x1C..0x20].try_into().unwrap()),
            version_high: u16::from_le_bytes(h[0x20..0x22].try_into().unwrap()),
            version_low: u16::from_le_bytes(h[0x22..0x24].try_into().unwrap()),
            entry_count: u32::from_le_bytes(h[0x24..0x28].try_into().unwrap()),
            profile_id: u32::from_le_bytes(h[0x28..0x2C].try_into().unwrap()),
            flags: u32::from_le_bytes(h[0x2C..0x30].try_into().unwrap()),
            constant_30: u32::from_le_bytes(h[0x30..0x34].try_into().unwrap()),
            zero_34: u32::from_le_bytes(h[0x34..0x38].try_into().unwrap()),
            reserved: [
                u32::from_le_bytes(h[0x38..0x3C].try_into().unwrap()),
                u32::from_le_bytes(h[0x3C..0x40].try_into().unwrap()),
            ],
        })
    }

    pub fn empty(version_high: u16, version_low: u16) -> Self {
        Self {
            at: 0,
            payload_start: HEADER_SIZE as u32,
            split: HEADER_SIZE as u32,
            trailer_size: 0,
            id: 0,
            generation_word: 0,
            constant_1c: 0,
            version_high,
            version_low,
            entry_count: 0,
            profile_id: 0,
            flags: 0,
            constant_30: 0,
            zero_34: 0,
            reserved: [0; 2],
        }
    }

    /// Generation extracted from the generation word.
    pub fn generation(&self) -> u16 {
        (self.generation_word >> 16) as u16
    }

    /// Version pair as `(high, low)`.
    pub fn version(&self) -> (u16, u16) {
        (self.version_high, self.version_low)
    }

    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[0x04..0x08].copy_from_slice(&self.payload_start.to_le_bytes());
        bytes[0x08..0x0C].copy_from_slice(&self.split.to_le_bytes());
        bytes[0x0C..0x10].copy_from_slice(&self.trailer_size.to_le_bytes());
        bytes[0x10..0x18].copy_from_slice(&self.id.to_le_bytes());
        bytes[0x18..0x1C].copy_from_slice(&self.generation_word.to_le_bytes());
        bytes[0x1C..0x20].copy_from_slice(&self.constant_1c.to_le_bytes());
        bytes[0x20..0x22].copy_from_slice(&self.version_high.to_le_bytes());
        bytes[0x22..0x24].copy_from_slice(&self.version_low.to_le_bytes());
        bytes[0x24..0x28].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[0x28..0x2C].copy_from_slice(&self.profile_id.to_le_bytes());
        bytes[0x2C..0x30].copy_from_slice(&self.flags.to_le_bytes());
        bytes[0x30..0x34].copy_from_slice(&self.constant_30.to_le_bytes());
        bytes[0x34..0x38].copy_from_slice(&self.zero_34.to_le_bytes());
        bytes[0x38..0x3C].copy_from_slice(&self.reserved[0].to_le_bytes());
        bytes[0x3C..0x40].copy_from_slice(&self.reserved[1].to_le_bytes());
        bytes
    }

    pub fn write_to<W: std::io::Write>(&self, out: &mut W) -> std::io::Result<()> {
        out.write_all(&self.to_bytes())
    }

    /// Number of 16-byte records in the first region.
    pub fn first_records(&self) -> usize {
        (self.split as usize).saturating_sub(HEADER_SIZE) / RECORD_SIZE
    }

    /// Number of 16-byte records in the second region.
    pub fn second_records(&self) -> usize {
        (self.payload_start as usize).saturating_sub(self.split as usize) / RECORD_SIZE
    }
}
