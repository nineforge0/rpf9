#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownFormat {
    pub ext: &'static str,
    pub name: &'static str,
    pub note: String,
}

type Media = Option<(usize, KnownFormat)>;

pub(crate) fn probe(data: &[u8]) -> Media {
    match data.first()? {
        b'B' | b'K' => bink(data),
        b'R' => riff(data),
        b'O' => ogg(data),
        0x89 => png(data),
        b'#' => amr(data),
        b'A' => awc(data),
        _ => None,
    }
}

fn bink(d: &[u8]) -> Media {
    const MAX_FRAMES: u32 = 1 << 20;
    let ext = match d.get(..3)? {
        b"BIK" => "bik",
        b"KB2" => "bk2",
        _ => return None,
    };
    let (len, frames) = (le32(d, 4)? as usize + 8, le32(d, 8)?);
    let revision = d[3].is_ascii_lowercase();
    let plausible = (1..=MAX_FRAMES).contains(&frames) && (12..=d.len()).contains(&len);
    let format = || KnownFormat {
        ext,
        name: "Bink video",
        note: format!("{frames} frames"),
    };
    (revision && plausible).then(|| (len, format()))
}

fn riff(d: &[u8]) -> Media {
    if !d.starts_with(b"RIFF") {
        return None;
    }
    let len = le32(d, 4)? as usize + 8;
    let (ext, name, note) = match (d.get(8..12)?, d.get(12..16)?) {
        (b"WAVE", b"fmt ") => {
            let (format, channels) = (le16(d, 20)?, le16(d, 22)?);
            let (rate, bits) = (le32(d, 24)?, le16(d, 34)?);
            let note = format!("{channels} ch, {rate} Hz, {bits}-bit, format {format:#06x}");
            ("wav", "WAVE audio", note)
        }
        (b"AVI ", b"LIST") => ("avi", "AVI video", String::new()),
        (b"WEBP", _) => ("webp", "WebP image", String::new()),
        _ => return None,
    };
    (16..=d.len())
        .contains(&len)
        .then_some((len, KnownFormat { ext, name, note }))
}

fn ogg(d: &[u8]) -> Media {
    const BEGINS_STREAM: u8 = 0x02;
    const ENDS_STREAM: u8 = 0x04;
    const CODECS: [(&[u8], &str); 5] = [
        (b"\x01vorbis", "Vorbis"),
        (b"OpusHead", "Opus"),
        (b"\x7fFLAC", "FLAC"),
        (b"\x80theora", "Theora"),
        (b"Speex   ", "Speex"),
    ];
    let (_, first) = ogg_page(d).filter(|(flags, _)| flags & BEGINS_STREAM != 0)?;
    let payload = &first[27 + usize::from(first[26])..];
    let codec = CODECS.iter().find(|(magic, _)| payload.starts_with(magic));
    let codec = codec.map_or("unknown codec", |(_, name)| name);

    let (mut len, mut pages, mut ended) = (0, 0, false);
    while let Some((flags, page)) = ogg_page(&d[len..]) {
        if ended && flags & BEGINS_STREAM != 0 {
            break;
        }
        len += page.len();
        pages += 1;
        ended |= flags & ENDS_STREAM != 0;
    }
    let note = format!("{codec}, {pages} pages");
    ended.then_some((
        len,
        KnownFormat {
            ext: "ogg",
            name: "Ogg stream",
            note,
        },
    ))
}

fn ogg_page(d: &[u8]) -> Option<(u8, &[u8])> {
    let header = d.first_chunk::<27>()?;
    let segments = d.get(27..27 + usize::from(header[26]))?;
    let payload: usize = segments.iter().map(|&len| usize::from(len)).sum();
    let page = d.get(..27 + segments.len() + payload)?;
    header.starts_with(b"OggS\0").then_some((header[5], page))
}

fn png(d: &[u8]) -> Media {
    if !d.starts_with(b"\x89PNG\r\n\x1a\n") || d.get(8..16)? != b"\0\0\0\x0dIHDR" {
        return None;
    }
    let note = format!("{}x{}", be32(d, 16)?, be32(d, 20)?);
    let mut len = 8;
    loop {
        let (size, chunk) = (be32(d, len)? as usize, d.get(len + 4..len + 8)?);
        len += size + 12;
        if chunk == b"IEND" {
            break;
        }
    }
    (len <= d.len()).then_some((
        len,
        KnownFormat {
            ext: "png",
            name: "PNG image",
            note,
        },
    ))
}

fn amr(d: &[u8]) -> Media {
    const NARROW_FRAMES: [usize; 16] = [13, 14, 16, 18, 20, 21, 27, 32, 6, 0, 0, 0, 0, 0, 0, 1];
    const WIDE_FRAMES: [usize; 16] = [18, 24, 33, 37, 41, 47, 51, 59, 61, 6, 0, 0, 0, 0, 1, 1];
    const MIN_FRAMES: usize = 8;
    const FRAMES_PER_SECOND: usize = 50;
    let (ext, name, magic, sizes) = if d.starts_with(b"#!AMR-WB\n") {
        ("awb", "AMR-WB audio", 9, WIDE_FRAMES)
    } else if d.starts_with(b"#!AMR\n") {
        ("amr", "AMR-NB audio", 6, NARROW_FRAMES)
    } else {
        return None;
    };
    let (mut len, mut frames) = (magic, 0);
    while let Some(&head) = d.get(len) {
        let size = sizes[usize::from(head >> 3 & 0xf)];
        if size == 0 || head & 0x83 != 0 || len + size > d.len() {
            break;
        }
        len += size;
        frames += 1;
    }
    let note = format!("{frames} frames, {} s", frames / FRAMES_PER_SECOND);
    (frames >= MIN_FRAMES).then_some((len, KnownFormat { ext, name, note }))
}

fn awc(d: &[u8]) -> Media {
    const KNOWN_FLAGS: [u32; 4] = [0xff02_0001, 0xff21_0001, 0xff25_0001, 0xff61_0001];
    const HAS_WORD: u32 = 0x40_0000;
    const HAS_FIRST_TAGS: u32 = 0x01_0000;
    const HAS_TABLE: u32 = 0x20_0000;
    const DATA_TAG: u64 = 0x55;

    if !d.starts_with(b"ADAT") {
        return None;
    }
    let (flags, streams) = (le32(d, 4)?, le32(d, 8)? as usize);
    let header = d.get(..le32(d, 12)? as usize)?;
    if !KNOWN_FLAGS.contains(&flags) {
        return None;
    }
    let has = |flag| flags & flag != 0;
    let first_tags = 16 + if has(HAS_WORD) { 4 } else { 0 };
    let table = first_tags + if has(HAS_FIRST_TAGS) { 2 * streams } else { 0 };
    let ids = table + if has(HAS_TABLE) { 4 * (streams + 1) } else { 0 };
    let tags = ids + 4 * streams;

    let mut count = 0;
    for stream in 0..streams {
        let first_tag = le16(header, first_tags + 2 * stream).map(usize::from);
        if has(HAS_FIRST_TAGS) && first_tag? != count {
            return None;
        }
        count += (le32(header, ids + 4 * stream)? >> 29) as usize;
    }
    if tags + 8 * count != header.len() {
        return None;
    }

    let (mut len, mut has_data) = (header.len(), false);
    for &tag in header[tags..].as_chunks::<8>().0 {
        let tag = u64::from_le_bytes(tag);
        let (kind, size, at) = (tag >> 56, tag >> 28 & 0xfff_ffff, tag & 0xfff_ffff);
        let (size, at) = (size as usize, at as usize);
        if (size > 0 && at < header.len()) || at + size > d.len() {
            return None;
        }
        len = len.max(at + size);
        has_data |= kind == DATA_TAG && size > 0;
    }
    let note = format!("{streams} streams, flags {flags:#x}");
    (streams == 0 || has_data).then_some((
        len,
        KnownFormat {
            ext: "awc",
            name: "AWC audio bank",
            note,
        },
    ))
}

fn array<const N: usize>(data: &[u8], at: usize) -> Option<[u8; N]> {
    data.get(at..)?.first_chunk().copied()
}

fn le16(data: &[u8], at: usize) -> Option<u16> {
    array(data, at).map(u16::from_le_bytes)
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    array(data, at).map(u32::from_le_bytes)
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    array(data, at).map(u32::from_be_bytes)
}
