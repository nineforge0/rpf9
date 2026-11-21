pub mod archive;
pub mod carve;
pub mod error;
pub mod header;
#[cfg(feature = "mmap")]
pub mod mmap;
pub mod scan;
pub mod write;
mod zlib;

pub const MAGIC: [u8; 4] = *b"9FPR";
pub const SECTOR_SIZE: usize = 0x1000;
pub const HEADER_SIZE: usize = 0x40;
pub const RECORD_SIZE: usize = 16;
pub const ALIGNMENT: usize = 16;

pub use archive::{Archive, Tables};
pub use carve::KnownFormat;
pub use error::{Error, Result};
pub use header::Header;
#[cfg(feature = "mmap")]
pub use mmap::MappedFile;
pub use scan::{CarvedFile, Coverage, FileKind, Part, Scan};
pub use write::ArchiveWriter;
