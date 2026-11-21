use crate::error::{Error, Result};
use crate::{HEADER_SIZE, MAGIC, RECORD_SIZE};

/// A parsed RPF9 header.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    /// Offset of the header in the buffer. Not encoded.
    pub at: usize,
    /// Header length including both record tables.
    pub header_size: u32,
    /// End of the first record table, relative to the header start.
    pub split: u32,
    /// Trailer length.
    pub trailer_size: u32,
    /// Archive id.
    pub id: u64,
    /// `0x47a00`, idk.
    pub unknown_18: u32,
    /// `0x150`, idk.
    pub unknown_1c: u32,
    /// 3-7, idk.
    pub version: u16,
    /// 1 on Xbox, idk.
    pub platform: u16,
    /// Declared file count.
    pub file_count: u32,
    /// Idk.
    pub group_id: u32,
    /// `0x00090F01` / `0x00090F02`, idk.
    pub flags: u32,
    /// `0x10`, idk.
    pub unknown_30: u32,
    /// 0, idk.
    pub unknown_34: u32,
    /// 0, idk.
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

        let header_size = u32::from_le_bytes(h[0x04..0x08].try_into().unwrap());
        let split = u32::from_le_bytes(h[0x08..0x0C].try_into().unwrap());

        if (header_size as usize) < HEADER_SIZE {
            return Err(Error::InvalidHeader {
                at,
                reason: "header size below 0x40",
            });
        }
        if (split as usize) < HEADER_SIZE {
            return Err(Error::InvalidHeader {
                at,
                reason: "split before header",
            });
        }
        if split > header_size {
            return Err(Error::InvalidHeader {
                at,
                reason: "split past header size",
            });
        }
        if at
            .checked_add(header_size as usize)
            .is_none_or(|end| end > data.len())
        {
            return Err(Error::InvalidHeader {
                at,
                reason: "header ends past buffer",
            });
        }
        if !(split as usize - HEADER_SIZE).is_multiple_of(RECORD_SIZE) {
            return Err(Error::InvalidHeader {
                at,
                reason: "first table is not 16-byte aligned",
            });
        }
        if !((header_size - split) as usize).is_multiple_of(RECORD_SIZE) {
            return Err(Error::InvalidHeader {
                at,
                reason: "second table is not 16-byte aligned",
            });
        }

        Ok(Self {
            at,
            header_size,
            split,
            trailer_size: u32::from_le_bytes(h[0x0C..0x10].try_into().unwrap()),
            id: u64::from_le_bytes(h[0x10..0x18].try_into().unwrap()),
            unknown_18: u32::from_le_bytes(h[0x18..0x1C].try_into().unwrap()),
            unknown_1c: u32::from_le_bytes(h[0x1C..0x20].try_into().unwrap()),
            version: u16::from_le_bytes(h[0x20..0x22].try_into().unwrap()),
            platform: u16::from_le_bytes(h[0x22..0x24].try_into().unwrap()),
            file_count: u32::from_le_bytes(h[0x24..0x28].try_into().unwrap()),
            group_id: u32::from_le_bytes(h[0x28..0x2C].try_into().unwrap()),
            flags: u32::from_le_bytes(h[0x2C..0x30].try_into().unwrap()),
            unknown_30: u32::from_le_bytes(h[0x30..0x34].try_into().unwrap()),
            unknown_34: u32::from_le_bytes(h[0x34..0x38].try_into().unwrap()),
            reserved: [
                u32::from_le_bytes(h[0x38..0x3C].try_into().unwrap()),
                u32::from_le_bytes(h[0x3C..0x40].try_into().unwrap()),
            ],
        })
    }

    pub fn empty(version: u16, platform: u16) -> Self {
        Self {
            at: 0,
            header_size: HEADER_SIZE as u32,
            split: HEADER_SIZE as u32,
            trailer_size: 0,
            id: 0,
            unknown_18: 0,
            unknown_1c: 0,
            version,
            platform,
            file_count: 0,
            group_id: 0,
            flags: 0,
            unknown_30: 0,
            unknown_34: 0,
            reserved: [0; 2],
        }
    }

    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[0x04..0x08].copy_from_slice(&self.header_size.to_le_bytes());
        bytes[0x08..0x0C].copy_from_slice(&self.split.to_le_bytes());
        bytes[0x0C..0x10].copy_from_slice(&self.trailer_size.to_le_bytes());
        bytes[0x10..0x18].copy_from_slice(&self.id.to_le_bytes());
        bytes[0x18..0x1C].copy_from_slice(&self.unknown_18.to_le_bytes());
        bytes[0x1C..0x20].copy_from_slice(&self.unknown_1c.to_le_bytes());
        bytes[0x20..0x22].copy_from_slice(&self.version.to_le_bytes());
        bytes[0x22..0x24].copy_from_slice(&self.platform.to_le_bytes());
        bytes[0x24..0x28].copy_from_slice(&self.file_count.to_le_bytes());
        bytes[0x28..0x2C].copy_from_slice(&self.group_id.to_le_bytes());
        bytes[0x2C..0x30].copy_from_slice(&self.flags.to_le_bytes());
        bytes[0x30..0x34].copy_from_slice(&self.unknown_30.to_le_bytes());
        bytes[0x34..0x38].copy_from_slice(&self.unknown_34.to_le_bytes());
        bytes[0x38..0x3C].copy_from_slice(&self.reserved[0].to_le_bytes());
        bytes[0x3C..0x40].copy_from_slice(&self.reserved[1].to_le_bytes());
        bytes
    }

    pub fn write_to<W: std::io::Write>(&self, out: &mut W) -> std::io::Result<()> {
        out.write_all(&self.to_bytes())
    }

    /// Number of 16-byte records in the first table.
    pub fn first_records(&self) -> usize {
        (self.split as usize).saturating_sub(HEADER_SIZE) / RECORD_SIZE
    }

    /// Number of 16-byte records in the second table.
    pub fn second_records(&self) -> usize {
        (self.header_size as usize).saturating_sub(self.split as usize) / RECORD_SIZE
    }
}
