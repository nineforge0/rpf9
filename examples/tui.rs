//! Browse an RPF9 archive in a terminal UI.
//!
//! ```sh
//! cargo run --example tui -- path/to/archive.rpf
//! cargo run --example tui -- path/to/archive.rpf --out extracted
//! ```

use clap::Parser;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use rpf9::{CarvedFile, Coverage, FileKind, Header, Scan};
use std::collections::BTreeSet;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const TEXT_LIMIT: u64 = 1 << 20;
const TEXT_LINES: usize = 2000;
const HEX_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Files,
    Details,
}

impl Focus {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Details => "details",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "rpf9-tui",
    bin_name = "rpf9-tui",
    version,
    about = "Browse a GTA VI RPF9 archive"
)]
struct Args {
    /// Archive to browse.
    archive: PathBuf,

    /// Directory for extracted files.
    #[arg(short, long, value_name = "DIR", default_value = "rpf9-out")]
    out: PathBuf,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let file = rpf9::MappedFile::open(&args.archive)?;
    let name = args.archive.display().to_string();
    let out = args.out;
    let data = file.as_slice();
    let total = data.len() as u64;

    let mut terminal = ratatui::init();
    let cancelled = AtomicBool::new(false);
    let result = thread::scope(|scope| -> std::io::Result<()> {
        let cancel_flag = &cancelled;
        let (sender, receiver) = mpsc::sync_channel(1);
        let handle = scope.spawn(move || {
            Scan::try_new_with_progress(data, |done, total| {
                let _ = sender.try_send((done, total));
                !cancel_flag.load(Ordering::Relaxed)
            })
        });

        let started = Instant::now();
        let (mut done, mut seen_total) = (0u64, total);
        let mut cancelling = false;
        while !handle.is_finished() {
            while let Ok((at, end)) = receiver.try_recv() {
                done = at;
                seen_total = end;
            }
            terminal.draw(|frame| {
                render_scan(
                    frame,
                    &name,
                    done,
                    seen_total,
                    started.elapsed(),
                    cancelling,
                )
            })?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            {
                cancelled.store(true, Ordering::Relaxed);
                cancelling = true;
            }
        }

        let scan = handle.join().expect("scan thread panicked");
        let Some(scan) = scan else {
            return Ok(());
        };
        let mut app = App::new(name, &scan, out);
        app.run(&mut terminal)
    });
    ratatui::restore();
    result
}

fn render_scan(
    frame: &mut Frame,
    name: &str,
    done: u64,
    total: u64,
    elapsed: Duration,
    cancelling: bool,
) {
    let [gauge_area, footer_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    let ratio = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };
    let seconds = elapsed.as_secs_f64().max(0.001);
    let speed = done as f64 / seconds;
    let eta = if speed > 1.0 {
        total.saturating_sub(done) as f64 / speed
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .block(Block::bordered().title(format!(" scanning {name} ")))
        .gauge_style(Style::new().fg(Color::Cyan))
        .ratio(ratio)
        .label(format!(
            "{:.1}% · {} / {} · {}/s · ETA {:.0}s",
            ratio * 100.0,
            human(done),
            human(total),
            human(speed as u64),
            eta
        ));
    frame.render_widget(gauge, gauge_area);
    let footer = if cancelling {
        " cancelling…"
    } else {
        " q/esc cancel"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::new().fg(Color::DarkGray)),
        footer_area,
    );
}

struct ArchiveRow {
    header: Header,
    body: Range<usize>,
    trailer: Range<usize>,
    first_records: usize,
    second_records: usize,
}

struct App<'a> {
    scan: &'a Scan<'a>,
    name: String,
    archives: Vec<ArchiveRow>,
    files: Vec<CarvedFile>,
    coverage: Coverage,
    state: ListState,
    marked: BTreeSet<usize>,
    focus: Focus,
    out: PathBuf,
    status: String,
    status_error: bool,
    preview: Option<Vec<Line<'static>>>,
    preview_for: Option<usize>,
    scroll: u16,
    detail_height: u16,
}

impl<'a> App<'a> {
    fn new(name: String, scan: &'a Scan<'a>, out: PathBuf) -> Self {
        let archives = scan
            .archives()
            .iter()
            .map(|archive| ArchiveRow {
                header: archive.header,
                body: archive.body.clone(),
                trailer: archive.trailer.clone(),
                first_records: archive.first_records(),
                second_records: archive.second_records(),
            })
            .collect();
        let files = scan.files().to_vec();
        let selected = (!files.is_empty()).then_some(0);
        Self {
            scan,
            name,
            archives,
            files,
            coverage: *scan.coverage(),
            state: ListState::default().with_selected(selected),
            marked: BTreeSet::new(),
            focus: Focus::Files,
            out,
            status: String::new(),
            status_error: false,
            preview: None,
            preview_for: None,
            scroll: 0,
            detail_height: 1,
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        loop {
            terminal.draw(|frame| self.render(frame))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if self.handle_key(key.code) {
                return Ok(());
            }
        }
    }

    /// Applies one key press. Returns true when the app should quit.
    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Files => Focus::Details,
                    Focus::Details => Focus::Files,
                };
            }
            KeyCode::Left => self.focus = Focus::Files,
            KeyCode::Right => self.focus = Focus::Details,
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Files => self.next(),
                Focus::Details => self.scroll_by(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Files => self.previous(),
                Focus::Details => self.scroll_by(-1),
            },
            KeyCode::PageDown => match self.focus {
                Focus::Files => self.move_by(8),
                Focus::Details => self.scroll_by(8),
            },
            KeyCode::PageUp => match self.focus {
                Focus::Files => self.move_by(-8),
                Focus::Details => self.scroll_by(-8),
            },
            KeyCode::Home => match self.focus {
                Focus::Files => self.first(),
                Focus::Details => self.scroll = 0,
            },
            KeyCode::End => match self.focus {
                Focus::Files => self.last(),
                Focus::Details => self.scroll_to_end(),
            },
            KeyCode::Char(' ') if self.focus == Focus::Files => self.toggle_mark_and_next(),
            KeyCode::Char('a') if self.focus == Focus::Files => self.toggle_all(),
            KeyCode::Char('e') => self.extract(),
            _ => {}
        }
        false
    }

    fn move_by(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let current = self.state.selected().unwrap_or(0) as isize;
        let last = self.files.len() as isize - 1;
        self.state
            .select(Some((current + delta).clamp(0, last) as usize));
    }

    fn next(&mut self) {
        self.move_by(1);
    }

    fn previous(&mut self) {
        self.move_by(-1);
    }

    fn first(&mut self) {
        if !self.files.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn last(&mut self) {
        if !self.files.is_empty() {
            self.state.select(Some(self.files.len() - 1));
        }
    }

    fn toggle_mark(&mut self) {
        let Some(index) = self.state.selected() else {
            return;
        };
        if !self.marked.remove(&index) {
            self.marked.insert(index);
        }
    }

    fn toggle_mark_and_next(&mut self) {
        self.toggle_mark();
        self.next();
    }

    fn toggle_all(&mut self) {
        if self.marked.len() == self.files.len() {
            self.marked.clear();
        } else {
            self.marked = (0..self.files.len()).collect();
        }
    }

    fn scroll_by(&mut self, delta: i16) {
        let Some(preview) = &self.preview else {
            return;
        };
        let visible = usize::from(self.detail_height.max(1));
        let max = preview.len().saturating_sub(visible) as u16;
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.saturating_add(delta as u16)
        };
        self.scroll = next.min(max);
    }

    fn scroll_to_end(&mut self) {
        let Some(preview) = &self.preview else {
            return;
        };
        let visible = usize::from(self.detail_height.max(1));
        self.scroll = preview.len().saturating_sub(visible) as u16;
    }

    fn extract(&mut self) {
        let targets: Vec<usize> = if self.marked.is_empty() {
            self.state.selected().into_iter().collect()
        } else {
            self.marked.iter().copied().collect()
        };
        if targets.is_empty() {
            self.status = "nothing to extract".to_owned();
            self.status_error = true;
            return;
        }

        let out = self.out.clone();
        if let Err(error) = fs::create_dir_all(&out) {
            self.status = format!("create {}: {error}", out.display());
            self.status_error = true;
            return;
        }

        let data = self.scan.data();
        let (mut count, mut bytes_written) = (0usize, 0u64);
        for index in targets {
            let Some(file) = self.files.get(index) else {
                continue;
            };
            let path = out.join(format!(
                "{index:04}_{:08x}.{}",
                file.span.start,
                file.kind.ext()
            ));
            let bytes = match file.read_to_vec(data) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.status = format!("extract #{index}: {error}");
                    self.status_error = true;
                    return;
                }
            };
            if let Err(error) = fs::write(&path, &bytes) {
                self.status = format!("write {}: {error}", path.display());
                self.status_error = true;
                return;
            }
            count += 1;
            bytes_written += bytes.len() as u64;
        }

        self.marked.clear();
        self.status = format!(
            "extracted {count} file(s), {} → {}",
            human(bytes_written),
            out.display()
        );
        self.status_error = false;
    }

    fn ensure_preview(&mut self) {
        let selected = self.state.selected();
        if selected.is_some() && selected == self.preview_for {
            return;
        }
        self.preview = selected
            .and_then(|index| self.files.get(index))
            .map(|file| build_preview(file, self.scan.data(), &self.archives));
        self.preview_for = selected;
        self.scroll = 0;
    }

    fn render(&mut self, frame: &mut Frame) {
        self.ensure_preview();

        let [header_area, body_area, footer_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .areas(frame.area());

        let total: u64 = self.files.iter().map(|file| file.size()).sum();
        let title = format!(
            " {} · {} archives · {} files · {} ",
            self.name,
            self.archives.len(),
            self.files.len(),
            human(total)
        );
        let coverage = self.coverage;
        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                "RPF9",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  structure {} · zlib {} · media {} · stored {} · padding {} · unexplained {}",
                coverage.structure,
                coverage.zlib,
                coverage.media,
                coverage.stored,
                coverage.padding,
                coverage.unexplained
            )),
        ]))
        .block(Block::bordered().title(title));
        frame.render_widget(header, header_area);

        let [list_area, detail_area] =
            Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                .areas(body_area);
        let files_focused = self.focus == Focus::Files;

        let items: Vec<ListItem> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let owner = file
                    .owner
                    .map_or_else(|| "-".to_owned(), |at| format!("{at:#x}"));
                let marked = self.marked.contains(&index);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if marked { "[x] " } else { "[ ] " },
                        Style::new().fg(if marked {
                            Color::LightYellow
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(format!("{index:>3} "), Style::new().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{:<5}", file.kind.ext()),
                        Style::new().fg(kind_color(&file.kind)),
                    ),
                    Span::raw(format!(" {:>9}  ", human(file.size()))),
                    Span::raw(format!("{:#010x}", file.span.start)),
                    Span::styled(format!("  owner {owner}"), Style::new().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::bordered()
                    .title(format!(
                        " files ({}, {} marked) ",
                        self.files.len(),
                        self.marked.len()
                    ))
                    .border_style(Style::new().fg(if files_focused {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    })),
            )
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, list_area, &mut self.state);

        self.detail_height = detail_area.height.saturating_sub(2);
        let lines = self
            .preview
            .clone()
            .unwrap_or_else(|| vec![Line::from("no carved files")]);
        let detail = Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" details ")
                    .border_style(Style::new().fg(if files_focused {
                        Color::DarkGray
                    } else {
                        Color::Cyan
                    })),
            )
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(detail, detail_area);

        let help = Line::from(Span::styled(
            " tab focus · j/k move/scroll · space mark · a all · e extract · q quit",
            Style::new().fg(Color::DarkGray),
        ));
        let status = if self.status.is_empty() {
            Line::from(Span::styled(
                format!(
                    " focus: {} · {} marked · out: {}",
                    self.focus.label(),
                    self.marked.len(),
                    self.out.display()
                ),
                Style::new().fg(Color::DarkGray),
            ))
        } else {
            Line::from(Span::styled(
                format!(" {}", self.status),
                Style::new().fg(if self.status_error {
                    Color::Red
                } else {
                    Color::Green
                }),
            ))
        };
        let footer = Paragraph::new(vec![help, status]);
        frame.render_widget(footer, footer_area);
    }
}

fn build_preview(file: &CarvedFile, data: &[u8], archives: &[ArchiveRow]) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                file.kind.name().to_owned(),
                Style::new()
                    .fg(kind_color(&file.kind))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" ({})", file.kind.ext())),
        ]),
        row("size", human(file.size())),
        row(
            "span",
            format!("{:#x}..{:#x}", file.span.start, file.span.end),
        ),
        row(
            "owner",
            file.owner
                .map_or_else(|| "-".to_owned(), |at| format!("{at:#x}")),
        ),
        row("parts", file.parts.len().to_string()),
    ];
    if !file.kind.note().is_empty() {
        lines.push(row("note", file.kind.note().to_owned()));
    }
    for (index, part) in file.parts.iter().enumerate() {
        let status = part.inflated.map_or_else(
            || "raw".to_owned(),
            |size| format!("zlib → {}", human(size)),
        );
        lines.push(Line::from(format!(
            "  [{index}] {:#x}..{:#x}  {status}",
            part.range.start, part.range.end
        )));
    }

    match &file.kind {
        FileKind::Known(format) => match format.ext {
            "xml" => {
                section(&mut lines, "XML content");
                text_preview(&mut lines, file, data);
            }
            "png" => {
                section(&mut lines, "PNG");
                png_metadata(&mut lines, raw_asset(file, data));
            }
            "webp" => {
                section(&mut lines, "WebP");
                webp_metadata(&mut lines, raw_asset(file, data));
            }
            "wav" => {
                section(&mut lines, "WAVE");
                wav_metadata(&mut lines, raw_asset(file, data));
            }
            "awc" => {
                section(&mut lines, "AWC");
                awc_metadata(&mut lines, raw_asset(file, data));
            }
            "ogg" => {
                section(&mut lines, "Ogg");
                ogg_metadata(&mut lines, raw_asset(file, data));
            }
            "amr" | "awb" => {
                section(&mut lines, "AMR");
                amr_metadata(&mut lines, raw_asset(file, data));
            }
            "bik" | "bk2" => {
                section(&mut lines, "Bink");
                bink_metadata(&mut lines, raw_asset(file, data));
            }
            _ => {
                section(&mut lines, "hex");
                hex_preview(&mut lines, raw_prefix(file, data, HEX_LIMIT));
            }
        },
        _ => {
            section(&mut lines, "hex");
            hex_preview(&mut lines, raw_prefix(file, data, HEX_LIMIT));
        }
    }

    if let Some(archive) = file
        .owner
        .and_then(|at| archives.iter().find(|archive| archive.header.at == at))
    {
        let header = &archive.header;
        section(&mut lines, &format!("archive {:#x}", header.at));
        lines.push(row(
            "header",
            format!(
                "size {:#x}  split {:#x}  trailer {:#x}",
                header.header_size, header.split, header.trailer_size
            ),
        ));
        lines.push(row(
            "version",
            format!(
                "{}  platform {}  files {}",
                header.version, header.platform, header.file_count
            ),
        ));
        lines.push(row(
            "body",
            format!("{:#x}..{:#x}", archive.body.start, archive.body.end),
        ));
        lines.push(row(
            "trailer",
            format!("{:#x}..{:#x}", archive.trailer.start, archive.trailer.end),
        ));
        lines.push(row(
            "records",
            format!("{}/{}", archive.first_records, archive.second_records),
        ));
    }
    lines
}

fn section(lines: &mut Vec<Line<'static>>, title: &str) {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        title.to_owned(),
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
}

fn row(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::new().fg(Color::DarkGray)),
        Span::raw(value.into()),
    ])
}

fn raw_asset<'a>(file: &CarvedFile, data: &'a [u8]) -> &'a [u8] {
    match file.parts.as_slice() {
        [part] if part.inflated.is_none() => &data[part.range.clone()],
        _ => &[],
    }
}

fn raw_prefix<'a>(file: &CarvedFile, data: &'a [u8], limit: usize) -> &'a [u8] {
    let Some(part) = file.parts.iter().find(|part| part.inflated.is_none()) else {
        return &[];
    };
    let end = (part.range.start + limit).min(part.range.end);
    &data[part.range.start..end]
}

fn text_preview(lines: &mut Vec<Line<'static>>, file: &CarvedFile, data: &[u8]) {
    if file.size() > TEXT_LIMIT {
        lines.push(Line::from(format!(
            "skipped: {} exceeds 1 MiB text preview limit",
            human(file.size())
        )));
        return;
    }
    match file.read_to_vec(data) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let display = text.replace("><", ">\n<");
            let total = display.lines().count();
            for line in display.lines().take(TEXT_LINES) {
                let style = if line.trim_start().starts_with('<') {
                    Style::new().fg(Color::LightBlue)
                } else {
                    Style::new()
                };
                lines.push(Line::from(Span::styled(line.replace('\t', "    "), style)));
            }
            if total > TEXT_LINES {
                lines.push(Line::from(format!("… {} more lines", total - TEXT_LINES)));
            }
            if total == 0 {
                lines.push(Line::from("(empty)"));
            }
        }
        Err(error) => lines.push(Line::from(format!("cannot extract: {error}"))),
    }
}

fn hex_preview(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.is_empty() {
        lines.push(Line::from("(no raw bytes; payload is inflated)"));
        return;
    }
    for (index, chunk) in bytes.chunks(16).take(HEX_LIMIT / 16).enumerate() {
        let mut hex = String::with_capacity(50);
        let mut ascii = String::with_capacity(16);
        for (offset, byte) in chunk.iter().enumerate() {
            hex.push_str(&format!("{byte:02x} "));
            if offset == 7 {
                hex.push(' ');
            }
            ascii.push(if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            });
        }
        lines.push(Line::from(format!(
            "{:08x}  {hex:<49}  {ascii}",
            index * 16
        )));
    }
    if bytes.len() > HEX_LIMIT {
        lines.push(Line::from(format!(
            "… {} more bytes",
            bytes.len() - HEX_LIMIT
        )));
    }
}

fn png_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        lines.push(Line::from("not a complete PNG"));
        return;
    }
    let width = u32be(bytes, 16).unwrap_or(0);
    let height = u32be(bytes, 20).unwrap_or(0);
    lines.push(row("size", format!("{width}x{height} px")));
    lines.push(row("bit depth", bytes[24].to_string()));
    lines.push(row("color", png_color_name(bytes[25]).to_owned()));
    lines.push(row(
        "interlace",
        if bytes[28] == 1 { "Adam7" } else { "none" }.to_owned(),
    ));

    let mut at = 8;
    let mut chunks = 0usize;
    let mut ancillary = 0usize;
    let mut idat = 0usize;
    let mut idat_bytes = 0u64;
    while at + 12 <= bytes.len() && chunks < 128 {
        let Some(len) = u32be(bytes, at) else {
            break;
        };
        let Some(end) = at
            .checked_add(12)
            .and_then(|end| end.checked_add(len as usize))
        else {
            break;
        };
        if end > bytes.len() {
            lines.push(Line::from("(truncated PNG chunk)"));
            break;
        }
        let kind = &bytes[at + 4..at + 8];
        if kind == b"IEND" {
            break;
        }
        chunks += 1;
        if kind == b"IDAT" {
            idat += 1;
            idat_bytes += u64::from(len);
        } else if kind[0].is_ascii_lowercase() {
            ancillary += 1;
        }
        at = end;
    }
    lines.push(row(
        "chunks",
        format!("{chunks} ({idat} IDAT, {} data)", human(idat_bytes)),
    ));
    if ancillary > 0 {
        lines.push(row("ancillary", ancillary.to_string()));
    }
}

fn png_color_name(color: u8) -> &'static str {
    match color {
        0 => "grayscale",
        2 => "RGB",
        3 => "palette",
        4 => "grayscale + alpha",
        6 => "RGBA",
        _ => "unknown",
    }
}

fn webp_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() < 16 || !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        lines.push(Line::from("not a complete WebP"));
        return;
    }
    lines.push(row(
        "riff",
        human(u64::from(u32le(bytes, 4).unwrap_or(0)) + 8),
    ));

    let mut at = 12;
    let mut dimensions = None;
    let mut kinds = Vec::new();
    while at + 8 <= bytes.len() && kinds.len() < 32 {
        let kind = &bytes[at..at + 4];
        let Some(len) = u32le(bytes, at + 4).map(|len| len as usize) else {
            break;
        };
        let start = at + 8;
        let Some(end) = start.checked_add(len) else {
            break;
        };
        if end > bytes.len() {
            break;
        }
        let body = &bytes[start..end];
        match kind {
            b"VP8X" if body.len() >= 10 => {
                dimensions = Some((1 + u24le(body, 4), 1 + u24le(body, 7)));
            }
            b"VP8L" if body.len() >= 5 => {
                let bits = u32le(body, 1).unwrap_or(0);
                dimensions = Some((1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff)));
            }
            b"VP8 " if body.len() >= 10 && &body[3..6] == b"\x9d\x01\x2a" => {
                let width = u32::from(u16le(body, 6).unwrap_or(0) & 0x3fff);
                let height = u32::from(u16le(body, 8).unwrap_or(0) & 0x3fff);
                dimensions = Some((width, height));
            }
            _ => {}
        }
        kinds.push(String::from_utf8_lossy(kind).trim_end().to_owned());
        at = end + (len & 1);
    }
    if let Some((width, height)) = dimensions {
        lines.push(row("size", format!("{width}x{height} px")));
    }
    lines.push(row("chunks", kinds.join(", ")));
}

fn wav_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() < 12 || !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        lines.push(Line::from("not a complete WAVE"));
        return;
    }
    lines.push(row(
        "riff",
        human(u64::from(u32le(bytes, 4).unwrap_or(0)) + 8),
    ));

    let mut at = 12;
    let mut data_bytes = 0u64;
    let mut byte_rate = 0u32;
    let mut chunks = 0usize;
    while at + 8 <= bytes.len() && chunks < 64 {
        let id = &bytes[at..at + 4];
        let Some(size) = u32le(bytes, at + 4).map(|size| size as usize) else {
            break;
        };
        let start = at + 8;
        let Some(end) = start.checked_add(size) else {
            break;
        };
        if end > bytes.len() {
            break;
        }
        match id {
            b"fmt " if size >= 16 => {
                let format = u16le(bytes, start).unwrap_or(0);
                let channels = u16le(bytes, start + 2).unwrap_or(0);
                let rate = u32le(bytes, start + 4).unwrap_or(0);
                byte_rate = u32le(bytes, start + 8).unwrap_or(0);
                let bits = u16le(bytes, start + 14).unwrap_or(0);
                lines.push(row("format", format!("{format:#06x}")));
                lines.push(row("channels", channels.to_string()));
                lines.push(row("rate", format!("{rate} Hz")));
                lines.push(row("bits", format!("{bits}-bit")));
            }
            b"data" => data_bytes += size as u64,
            _ => {}
        }
        chunks += 1;
        at = end + (size & 1);
    }
    lines.push(row("data", human(data_bytes)));
    if byte_rate > 0 && data_bytes > 0 {
        lines.push(row(
            "duration",
            format!("{:.2} s", data_bytes as f64 / f64::from(byte_rate)),
        ));
    }
}

fn awc_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if !bytes.starts_with(b"ADAT") {
        lines.push(Line::from("not a complete AWC"));
        return;
    }
    let Some(flags) = u32le(bytes, 4) else {
        lines.push(Line::from("truncated AWC header"));
        return;
    };
    let streams = u32le(bytes, 8).unwrap_or(0) as usize;
    let header_len = u32le(bytes, 12).unwrap_or(0) as usize;
    lines.push(row("flags", format!("{flags:#010x}")));
    lines.push(row("streams", streams.to_string()));
    lines.push(row("header", human(header_len as u64)));
    if header_len > bytes.len() {
        lines.push(Line::from("header extends past carved bytes"));
        return;
    }

    let has = |flag: u32| flags & flag != 0;
    let first_tags = 16 + if has(0x0040_0000) { 4 } else { 0 };
    let table = first_tags + if has(0x0001_0000) { 2 * streams } else { 0 };
    let ids = table
        + if has(0x0020_0000) {
            4 * (streams + 1)
        } else {
            0
        };
    let tags = ids + 4 * streams;

    let mut counts = Vec::with_capacity(streams);
    for stream in 0..streams {
        let Some(word) = u32le(bytes, ids + 4 * stream) else {
            break;
        };
        counts.push((word >> 29) as usize);
    }
    let total: usize = counts.iter().sum();
    lines.push(row("chunks", total.to_string()));
    if !counts.is_empty() {
        let per_stream: Vec<_> = counts
            .iter()
            .enumerate()
            .map(|(index, count)| format!("#{index}:{count}"))
            .collect();
        lines.push(row("per stream", per_stream.join(" ")));
    }

    let mut data_bytes = 0u64;
    let mut kinds = std::collections::BTreeMap::<u8, usize>::new();
    let mut formats = Vec::new();
    for index in 0..total.min(256) {
        let Some(tag) = u64le(bytes, tags + index * 8) else {
            break;
        };
        let kind = (tag >> 56) as u8;
        let size = ((tag >> 28) & 0x0fff_ffff) as usize;
        let offset = (tag & 0x0fff_ffff) as usize;
        *kinds.entry(kind).or_default() += 1;
        if kind == 0x55 {
            data_bytes += size as u64;
        }
        let in_bounds = offset
            .checked_add(size)
            .is_some_and(|end| end <= bytes.len());
        if in_bounds && (matches!(kind, 0x76 | 0xFA) && size >= 20 || kind == 0x48 && size >= 12) {
            formats.push((kind, offset, size));
        }
    }
    if total > 256 {
        lines.push(Line::from(format!("… {} more chunk records", total - 256)));
    }
    lines.push(row("data", human(data_bytes)));
    let table: Vec<_> = kinds
        .iter()
        .map(|(kind, count)| format!("{}×{count}", awc_chunk_name(*kind)))
        .collect();
    if !table.is_empty() {
        lines.push(row("chunk table", table.join(" ")));
    }

    for (kind, offset, size) in formats.iter().take(8) {
        let body = &bytes[*offset..*offset + *size];
        if *kind == 0x48 {
            let blocks = u32le(body, 0).unwrap_or(0);
            let block_size = u32le(body, 4).unwrap_or(0);
            let channels = u32le(body, 8).unwrap_or(0) as usize;
            lines.push(row(
                "stream fmt",
                format!(
                    "{channels} ch, {blocks} blocks × {}",
                    human(u64::from(block_size))
                ),
            ));
            for channel in 0..channels.min(8) {
                let base = 12 + channel * 16;
                if base + 16 > body.len() {
                    break;
                }
                let samples = u32le(body, base + 4).unwrap_or(0);
                let rate = u16le(body, base + 10).unwrap_or(0);
                let codec = body[base + 12];
                lines.push(Line::from(format!(
                    "  ch {channel}: {rate} Hz, codec {codec:#04x}, {samples} samples"
                )));
            }
        } else {
            let samples = u32le(body, 0).unwrap_or(0);
            let rate = u16le(body, 8).unwrap_or(0);
            let codec = body[19];
            let duration = if rate > 0 {
                f64::from(samples) / f64::from(rate)
            } else {
                0.0
            };
            lines.push(row(
                "format",
                format!("{rate} Hz, codec {codec:#04x}, {samples} samples ({duration:.2} s)"),
            ));
        }
    }
}

fn awc_chunk_name(kind: u8) -> &'static str {
    match kind {
        0x55 => "data",
        0x76 | 0xFA => "format",
        0x48 => "stream-format",
        0x36 => "peak",
        0xA3 => "seek",
        0x5C => "animation",
        0xB2 => "metadata",
        0xBD => "markers",
        _ => "chunk",
    }
}

fn ogg_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() < 27 || !bytes.starts_with(b"OggS\0") {
        lines.push(Line::from("not a complete Ogg page"));
        return;
    }
    lines.push(row("version", bytes[4].to_string()));
    lines.push(row("flags", format!("{:#04x}", bytes[5])));
    lines.push(row("granule", u64le(bytes, 6).unwrap_or(0).to_string()));
    lines.push(row(
        "serial",
        format!("{:#010x}", u32le(bytes, 14).unwrap_or(0)),
    ));
    lines.push(row("sequence", u32le(bytes, 18).unwrap_or(0).to_string()));
    let segments = usize::from(bytes[26]);
    let payload: usize = bytes
        .get(27..27 + segments)
        .map(|lacing| lacing.iter().map(|&len| usize::from(len)).sum())
        .unwrap_or(0);
    lines.push(row(
        "page",
        format!("{segments} segments, {payload} payload bytes"),
    ));
}

fn amr_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    let mode = if bytes.starts_with(b"#!AMR-WB\n") {
        "AMR-WB"
    } else if bytes.starts_with(b"#!AMR\n") {
        "AMR-NB"
    } else {
        lines.push(Line::from("not a complete AMR stream"));
        return;
    };
    lines.push(row("mode", mode));
    lines.push(row("header", human(9)));
}

fn bink_metadata(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() < 12 {
        lines.push(Line::from("not a complete Bink header"));
        return;
    }
    lines.push(row(
        "declared",
        human(u64::from(u32le(bytes, 4).unwrap_or(0)) + 8),
    ));
    lines.push(row("frames", u32le(bytes, 8).unwrap_or(0).to_string()));
}

fn kind_color(kind: &FileKind) -> Color {
    match kind {
        FileKind::Resource => Color::Magenta,
        FileKind::Known(_) => Color::Green,
        FileKind::Unknown => Color::Yellow,
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn u16le(data: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(data.get(at..end)?.try_into().ok()?))
}

fn u32le(data: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(data.get(at..end)?.try_into().ok()?))
}

fn u64le(data: &[u8], at: usize) -> Option<u64> {
    let end = at.checked_add(8)?;
    Some(u64::from_le_bytes(data.get(at..end)?.try_into().ok()?))
}

fn u32be(data: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(data.get(at..end)?.try_into().ok()?))
}

fn u24le(data: &[u8], at: usize) -> u32 {
    let end = at.saturating_add(3);
    match data.get(at..end) {
        Some(&[a, b, c]) => u32::from(a) | u32::from(b) << 8 | u32::from(c) << 16,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use ratatui::{Terminal, backend::TestBackend};
    use std::io::Write;

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let first = b"\xef\xbb\xbf<?xml version=\"1.0\"?><a/>".to_vec();
        let second = b"\xef\xbb\xbf<?xml version=\"1.0\"?><b/>".to_vec();
        let first_stream = zlib(&first);
        let second_stream = zlib(&second);
        let at = 0x40;
        let next = (at + first_stream.len()).next_multiple_of(16);
        let mut data = vec![0u8; 0x400];
        data[at..at + first_stream.len()].copy_from_slice(&first_stream);
        data[next..next + second_stream.len()].copy_from_slice(&second_stream);
        (data, first, second)
    }

    #[test]
    fn marks_extract_scrolls_and_renders() {
        let (data, first, second) = fixture();
        let scan = Scan::new(&data);
        assert_eq!(scan.files().len(), 2);

        let out = std::env::temp_dir().join(format!("rpf9-tui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let mut app = App::new("test".to_owned(), &scan, out.clone());

        app.ensure_preview();
        app.detail_height = 3;
        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Details);
        app.scroll = 0;
        assert!(!app.handle_key(KeyCode::Char('j')));
        assert_eq!(app.scroll, 1);
        assert!(!app.handle_key(KeyCode::PageDown));
        assert_eq!(
            app.scroll,
            app.preview.as_ref().unwrap().len().saturating_sub(3) as u16
        );
        assert!(!app.handle_key(KeyCode::Home));
        assert_eq!(app.scroll, 0);
        assert!(!app.handle_key(KeyCode::End));
        let expected_max = app.preview.as_ref().unwrap().len().saturating_sub(3) as u16;
        assert_eq!(app.scroll, expected_max);
        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Files);

        assert!(!app.handle_key(KeyCode::Char(' ')));
        assert!(!app.handle_key(KeyCode::Char(' ')));
        assert_eq!(app.marked, BTreeSet::from([0, 1]));
        assert!(!app.handle_key(KeyCode::Char('a')));
        assert!(app.marked.is_empty());
        assert!(!app.handle_key(KeyCode::Char('a')));
        assert_eq!(app.marked, BTreeSet::from([0, 1]));

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("[x]"), "screen:\n{screen}");
        assert!(screen.contains("focus: files"), "screen:\n{screen}");
        assert!(screen.contains("2 marked"), "screen:\n{screen}");

        assert!(!app.handle_key(KeyCode::Char('e')));
        assert!(app.marked.is_empty());
        assert!(app.status.starts_with("extracted 2"), "{}", app.status);
        assert!(app.handle_key(KeyCode::Char('q')));

        let mut written: Vec<Vec<u8>> = std::fs::read_dir(&out)
            .unwrap()
            .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
            .collect();
        written.sort();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(written, expected);
        std::fs::remove_dir_all(&out).unwrap();
    }
}
