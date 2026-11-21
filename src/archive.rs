use crate::header::Header;
use crate::{ALIGNMENT, HEADER_SIZE, SECTOR_SIZE};
use std::fmt;
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub struct Tables<'a> {
    pub first: &'a [u8],
    pub second: &'a [u8],
}

#[derive(Clone)]
pub struct Archive<'a> {
    pub header: Header,
    pub tables: Tables<'a>,
    pub body: Range<usize>,
    pub trailer: Range<usize>,
    data: &'a [u8],
}

impl fmt::Debug for Archive<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Archive")
            .field("header", &self.header)
            .field("body", &self.body)
            .field("trailer", &self.trailer)
            .finish_non_exhaustive()
    }
}

impl<'a> Archive<'a> {
    pub fn extent(&self) -> Range<usize> {
        self.header.at..self.trailer.end
    }

    pub fn structure(&self) -> [Range<usize>; 2] {
        [self.header.at..self.body.start, self.trailer.clone()]
    }

    pub fn first_table(&self) -> &'a [u8] {
        self.tables.first
    }

    pub fn second_table(&self) -> &'a [u8] {
        self.tables.second
    }

    pub fn body(&self) -> &'a [u8] {
        &self.data[self.body.clone()]
    }

    pub fn trailer(&self) -> &'a [u8] {
        &self.data[self.trailer.clone()]
    }

    pub fn first_records(&self) -> usize {
        self.header.first_records()
    }

    pub fn second_records(&self) -> usize {
        self.header.second_records()
    }
}

pub(crate) fn discover<'a>(data: &'a [u8], known: &[Range<usize>]) -> Vec<Archive<'a>> {
    let mut headers = Vec::new();
    let mut at = 0;
    while at < data.len() {
        if !known.iter().any(|range| range.contains(&at))
            && let Ok(header) = Header::parse(data, at)
        {
            headers.push(header);
        }
        at = match at.checked_add(SECTOR_SIZE) {
            Some(next) => next,
            None => break,
        };
    }
    headers.sort_by_key(|header| header.at);

    let Some((&outer, nested)) = headers.split_first() else {
        return Vec::new();
    };
    let outer = place(data, outer, data.len(), known);
    let trailer_start = outer.trailer.start;
    let mut archives = Vec::with_capacity(headers.len());
    archives.push(outer);
    for (index, &header) in nested.iter().enumerate() {
        let limit = nested.get(index + 1).map_or(trailer_start, |next| next.at);
        archives.push(place(data, header, limit, known));
    }
    archives
}

fn place<'a>(data: &'a [u8], header: Header, limit: usize, known: &[Range<usize>]) -> Archive<'a> {
    let start = header.at.saturating_add(header.header_size as usize);
    let limit = limit.max(start).min(data.len());
    let end = gaps(&(start..limit), known)
        .iter()
        .rev()
        .find_map(|gap| last_non_zero(data, gap))
        .map_or(start, |last| last.next_multiple_of(ALIGNMENT).min(limit));
    let trailer_start = end.saturating_sub(header.trailer_size as usize).max(start);
    let tables = Tables {
        first: &data[header.at + HEADER_SIZE..header.at + header.split as usize],
        second: &data[header.at + header.split as usize..header.at + header.header_size as usize],
    };
    Archive {
        header,
        tables,
        body: start..trailer_start,
        trailer: trailer_start..end,
        data,
    }
}

fn last_non_zero(data: &[u8], gap: &Range<usize>) -> Option<usize> {
    const CHUNK: usize = 1 << 20;
    let mut end = gap.end;
    while end > gap.start {
        let begin = end.saturating_sub(CHUNK).max(gap.start);
        if let Some(position) = data[begin..end].iter().rposition(|&byte| byte != 0) {
            return Some(begin + position + 1);
        }
        end = begin;
    }
    None
}

pub(crate) fn gaps<'r>(
    within: &Range<usize>,
    sorted: impl IntoIterator<Item = &'r Range<usize>>,
) -> Vec<Range<usize>> {
    let (mut gaps, mut at) = (Vec::new(), within.start);
    for taken in sorted {
        let until = taken.start.min(within.end);
        if at < until {
            gaps.push(at..until);
        }
        at = at.max(taken.end);
    }
    if at < within.end {
        gaps.push(at..within.end);
    }
    gaps
}
