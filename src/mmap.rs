use std::fs::File;
use std::path::Path;

pub struct MappedFile {
    map: memmap2::Mmap,
}

impl MappedFile {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self { map })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.map
    }

    /// Parses this mapping as an RPF9 archive.
    pub fn parse(&self) -> crate::Result<crate::Rpf<'_>> {
        crate::Rpf::parse(self.as_slice())
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}
