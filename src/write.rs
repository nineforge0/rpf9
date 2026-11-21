use crate::archive::Archive;
use crate::error::{Error, Result};
use crate::header::Header;
use crate::{HEADER_SIZE, RECORD_SIZE};
use std::io::Write;

/// Serializes a header, caller-supplied tables, a body, and a trailer.
pub struct ArchiveWriter<'a> {
    pub header: Header,
    pub first_table: &'a [u8],
    pub second_table: &'a [u8],
    pub body: &'a [u8],
    pub trailer: &'a [u8],
}

impl<'a> ArchiveWriter<'a> {
    pub fn new(
        header: Header,
        first_table: &'a [u8],
        second_table: &'a [u8],
        body: &'a [u8],
        trailer: &'a [u8],
    ) -> Self {
        Self {
            header,
            first_table,
            second_table,
            body,
            trailer,
        }
    }

    /// Borrows an archive's header and byte ranges.
    pub fn from_archive(archive: &Archive<'a>) -> Self {
        Self {
            header: archive.header,
            first_table: archive.first_table(),
            second_table: archive.second_table(),
            body: archive.body(),
            trailer: archive.trailer(),
        }
    }

    /// Encoded length in bytes, after validating table alignment and size
    /// fields.
    pub fn encoded_len(&self) -> Result<u64> {
        let (header_size, _, _) = self.sizes()?;
        Ok(u64::from(header_size) + self.body.len() as u64 + self.trailer.len() as u64)
    }

    /// Writes the container. The header's size fields are recomputed from the
    /// supplied tables and trailer.
    pub fn write_to<W: Write>(&self, out: &mut W) -> Result<()> {
        let (header_size, split, trailer_size) = self.sizes()?;
        let mut header = self.header;
        header.header_size = header_size;
        header.split = split;
        header.trailer_size = trailer_size;
        header.write_to(out)?;
        out.write_all(self.first_table)?;
        out.write_all(self.second_table)?;
        out.write_all(self.body)?;
        out.write_all(self.trailer)?;
        Ok(())
    }

    /// Serializes into a new buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>> {
        let len = self.encoded_len()?;
        let capacity = usize::try_from(len).map_err(|_| Error::TooLarge { size: len })?;
        let mut bytes = Vec::with_capacity(capacity);
        self.write_to(&mut bytes)?;
        Ok(bytes)
    }

    fn sizes(&self) -> Result<(u32, u32, u32)> {
        if !self.first_table.len().is_multiple_of(RECORD_SIZE) {
            return Err(Error::InvalidLayout {
                reason: "first table is not a multiple of 16 bytes",
            });
        }
        if !self.second_table.len().is_multiple_of(RECORD_SIZE) {
            return Err(Error::InvalidLayout {
                reason: "second table is not a multiple of 16 bytes",
            });
        }
        let header_size = HEADER_SIZE
            .checked_add(self.first_table.len())
            .and_then(|len| len.checked_add(self.second_table.len()))
            .and_then(|len| u32::try_from(len).ok())
            .ok_or(Error::InvalidLayout {
                reason: "header size exceeds u32",
            })?;
        let split = HEADER_SIZE
            .checked_add(self.first_table.len())
            .and_then(|len| u32::try_from(len).ok())
            .ok_or(Error::InvalidLayout {
                reason: "split exceeds u32",
            })?;
        let trailer_size = u32::try_from(self.trailer.len()).map_err(|_| Error::InvalidLayout {
            reason: "trailer size exceeds u32",
        })?;
        Ok((header_size, split, trailer_size))
    }
}
