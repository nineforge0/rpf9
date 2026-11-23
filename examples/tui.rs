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
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use rpf9::{Entry, Rpf};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const PREVIEW_LIMIT: usize = 1 << 20;
const HEX_LINES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Entries,
    Preview,
}

impl Focus {
    fn label(self) -> &'static str {
        match self {
            Self::Entries => "entries",
            Self::Preview => "preview",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "rpf9-tui",
    bin_name = "rpf9-tui",
    version,
    about = "Browse a RPF9 archive"
)]
struct Args {
    /// Archive to browse.
    archive: PathBuf,

    /// Directory for extracted entries.
    #[arg(short, long, value_name = "DIR", default_value = "rpf9-out")]
    out: PathBuf,
}

struct App<'a> {
    name: String,
    rpf: Rpf<'a>,
    state: ListState,
    focus: Focus,
    selected: BTreeSet<usize>,
    detail_scroll: u16,
    out: String,
    editing_out: bool,
    status: String,
    log: Vec<String>,
    preview_entry: Option<usize>,
    preview_lines: Vec<String>,
    preview_text: bool,
}

impl<'a> App<'a> {
    fn new(name: String, rpf: Rpf<'a>, out: PathBuf) -> Self {
        let mut state = ListState::default();
        if !rpf.entries().is_empty() {
            state.select(Some(0));
        }
        let mut app = Self {
            name,
            rpf,
            state,
            focus: Focus::Entries,
            selected: BTreeSet::new(),
            detail_scroll: 0,
            out: out.display().to_string(),
            editing_out: false,
            status: "ready".into(),
            log: Vec::new(),
            preview_entry: None,
            preview_lines: Vec::new(),
            preview_text: false,
        };
        app.refresh_preview();
        app
    }

    fn current(&self) -> Option<&Entry> {
        self.state
            .selected()
            .and_then(|i| self.rpf.entries().get(i))
    }

    fn refresh_preview(&mut self) {
        let Some(index) = self.state.selected() else {
            self.preview_entry = None;
            self.preview_lines.clear();
            return;
        };
        if self.preview_entry == Some(index) {
            return;
        }
        self.preview_entry = Some(index);
        self.detail_scroll = 0;
        self.preview_lines.clear();
        let Some(entry) = self.rpf.entries().get(index) else {
            return;
        };
        match self.rpf.read(entry) {
            Ok(bytes) => {
                let truncated = bytes.len() > PREVIEW_LIMIT;
                let slice = &bytes[..bytes.len().min(PREVIEW_LIMIT)];
                self.preview_text = is_text(slice);
                if self.preview_text {
                    self.preview_lines = slice
                        .split(|&b| b == b'\n')
                        .take(HEX_LINES * 16)
                        .map(|line| {
                            String::from_utf8_lossy(line)
                                .trim_end_matches('\r')
                                .to_owned()
                        })
                        .collect();
                } else {
                    self.preview_lines = hexdump(&slice[..slice.len().min(HEX_LINES * 16)]);
                }
                if truncated {
                    self.preview_lines
                        .push(format!("… preview capped at {PREVIEW_LIMIT} bytes"));
                }
            }
            Err(error) => self.preview_lines.push(format!("cannot read: {error}")),
        }
    }

    fn toggle_selection(&mut self) {
        if let Some(index) = self.state.selected() {
            if !self.selected.remove(&index) {
                self.selected.insert(index);
            }
        }
    }

    fn extract(&mut self, indices: Vec<usize>) {
        if indices.is_empty() {
            self.status = "nothing selected".into();
            return;
        }
        let out = PathBuf::from(&self.out);
        if let Err(error) = fs::create_dir_all(&out) {
            self.status = format!("cannot create {}: {error}", out.display());
            return;
        }
        let mut written = 0;
        let mut failed = 0;
        for index in indices {
            let Some(entry) = self.rpf.entries().get(index) else {
                continue;
            };
            match self.rpf.read(entry) {
                Ok(bytes) => {
                    let name = format!("{:08x}.{}", entry.offset, rpf9::reader::sniff_ext(&bytes));
                    match fs::write(out.join(name), &bytes) {
                        Ok(()) => written += 1,
                        Err(_) => failed += 1,
                    }
                }
                Err(_) => failed += 1,
            }
        }
        self.status = if failed == 0 {
            format!("extracted {written} → {}", out.display())
        } else {
            format!("extracted {written}, {failed} failed → {}", out.display())
        };
        self.push_log(self.status.clone());
    }

    fn push_log(&mut self, message: String) {
        self.log.push(message);
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.rpf.entries().len();
        if count == 0 {
            return;
        }
        let current = self.state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, count as isize - 1) as usize;
        self.state.select(Some(next));
    }

    fn scroll_preview(&mut self, delta: isize) {
        let max = self.preview_lines.len().saturating_sub(1) as isize;
        self.detail_scroll = (self.detail_scroll as isize + delta).clamp(0, max) as u16;
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        loop {
            self.refresh_preview();
            terminal.draw(|frame| self.draw(frame))?;
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if self.editing_out {
                match key.code {
                    KeyCode::Enter => {
                        self.editing_out = false;
                        self.status = format!("destination: {}", self.out);
                    }
                    KeyCode::Esc => {
                        self.editing_out = false;
                        self.status = "destination unchanged".into();
                    }
                    KeyCode::Backspace => {
                        self.out.pop();
                    }
                    KeyCode::Char(c) => self.out.push(c),
                    _ => {}
                }
                continue;
            }

            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Esc => {
                    if self.selected.is_empty() {
                        return Ok(());
                    }
                    self.selected.clear();
                    self.status = "selection cleared".into();
                }
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                    self.focus = match self.focus {
                        Focus::Entries => Focus::Preview,
                        Focus::Preview => Focus::Entries,
                    };
                }
                KeyCode::Char('j') | KeyCode::Down => match self.focus {
                    Focus::Entries => self.move_selection(1),
                    Focus::Preview => self.scroll_preview(1),
                },
                KeyCode::Char('k') | KeyCode::Up => match self.focus {
                    Focus::Entries => self.move_selection(-1),
                    Focus::Preview => self.scroll_preview(-1),
                },
                KeyCode::PageDown => match self.focus {
                    Focus::Entries => self.move_selection(20),
                    Focus::Preview => self.scroll_preview(20),
                },
                KeyCode::PageUp => match self.focus {
                    Focus::Entries => self.move_selection(-20),
                    Focus::Preview => self.scroll_preview(-20),
                },
                KeyCode::Home | KeyCode::Char('g') => match self.focus {
                    Focus::Entries => self.state.select(Some(0)),
                    Focus::Preview => self.detail_scroll = 0,
                },
                KeyCode::End | KeyCode::Char('G') => match self.focus {
                    Focus::Entries => self
                        .state
                        .select(Some(self.rpf.entries().len().saturating_sub(1))),
                    Focus::Preview => {
                        self.detail_scroll = self.preview_lines.len().saturating_sub(1) as u16
                    }
                },
                KeyCode::Char(' ') => {
                    self.toggle_selection();
                    self.move_selection(1);
                }
                KeyCode::Char('a') => {
                    self.selected.extend(0..self.rpf.entries().len());
                    self.status = format!("selected {}", self.selected.len());
                }
                KeyCode::Char('u') => {
                    self.selected.clear();
                    self.status = "selection cleared".into();
                }
                KeyCode::Char('d') => {
                    self.editing_out = true;
                    self.status = "edit destination · Enter apply · Esc cancel".into();
                }
                KeyCode::Char('e') => {
                    let targets: Vec<usize> = if self.selected.is_empty() {
                        self.state.selected().into_iter().collect()
                    } else {
                        self.selected.iter().copied().collect()
                    };
                    self.extract(targets);
                }
                KeyCode::Char('x') => {
                    let all: Vec<usize> = (0..self.rpf.entries().len()).collect();
                    self.extract(all);
                }
                _ => {}
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [header_area, body, bottom] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .areas(frame.area());
        let [list_area, detail_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(body);

        self.draw_header(frame, header_area);
        self.draw_list(frame, list_area);
        self.draw_detail(frame, detail_area);
        self.draw_bottom(frame, bottom);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let h = self.rpf.header();
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    " 9FPR ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    self.name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    "   {} entries   key #{}   graph {}",
                    self.rpf.entries().len(),
                    self.rpf.key_index(),
                    if self.rpf.has_graph() { "yes" } else { "no" }
                )),
            ]),
            Line::from(vec![
                Span::styled("version ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}.{}", h.version_high, h.version_low),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("   gen ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:#x}", h.generation()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("   flags ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:#x}", h.flags),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("   payload ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:#x}", h.payload_start),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("   split ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:#x}", h.split),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("   trailer ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:#x}", h.trailer_size),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            area,
        );
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .rpf
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let chosen = self.selected.contains(&index);
                let zlib = entry.is_zlib(self.rpf.data());
                let style = if zlib {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Gray)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if chosen { "● " } else { "  " },
                        Style::default().fg(Color::Magenta),
                    ),
                    Span::styled(
                        format!("{:08x} ", entry.offset),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{:>8} ", entry.size),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("{:04x} ", entry.flags),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(if zlib { "zlib" } else { "raw " }, style),
                ]))
            })
            .collect();
        let border = if self.focus == Focus::Entries {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border)
                    .title(format!(" entries · {} selected ", self.selected.len())),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(40, 60, 90))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn draw_detail(&self, frame: &mut Frame, area: Rect) {
        let border = if self.focus == Focus::Preview {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let title = match self.current() {
            Some(entry) => format!(
                " entry #{:04}   {:#x}..{:#x}   {} bytes   {}{} ",
                entry.index,
                entry.offset,
                entry.offset + entry.size,
                entry.size,
                if self.preview_text { "text" } else { "hex" },
                if entry.is_zlib(self.rpf.data()) {
                    " · zlib"
                } else {
                    ""
                }
            ),
            None => " preview ".into(),
        };
        let lines: Vec<Line> = self
            .preview_lines
            .iter()
            .map(|line| style_preview_line(line, self.preview_text))
            .collect();
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border)
                    .title(title),
            )
            .scroll((self.detail_scroll, 0));
        frame.render_widget(paragraph, area);
    }

    fn draw_bottom(&self, frame: &mut Frame, area: Rect) {
        let selected = self.selected.len();
        let destination_style = if self.editing_out {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let lines = vec![
            Line::from(vec![
                Span::styled("dest ", Style::default().fg(Color::DarkGray)),
                Span::styled(self.out.clone(), destination_style),
                if self.editing_out {
                    Span::styled("▌", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(vec![
                Span::styled("status ", Style::default().fg(Color::DarkGray)),
                Span::styled(self.status.clone(), Style::default().fg(Color::Green)),
                Span::styled(
                    format!("   focus {}   {selected} selected", self.focus.label()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(vec![
                Span::styled("↑↓/jk ", Style::default().fg(Color::Cyan)),
                Span::raw("move "),
                Span::styled("Tab/←→ ", Style::default().fg(Color::Cyan)),
                Span::raw("pane "),
                Span::styled("Space ", Style::default().fg(Color::Cyan)),
                Span::raw("select "),
                Span::styled("a/u ", Style::default().fg(Color::Cyan)),
                Span::raw("all/none "),
                Span::styled("e ", Style::default().fg(Color::Cyan)),
                Span::raw("extract "),
                Span::styled("x ", Style::default().fg(Color::Cyan)),
                Span::raw("extract all "),
                Span::styled("d ", Style::default().fg(Color::Cyan)),
                Span::raw("dest "),
                Span::styled("q ", Style::default().fg(Color::Cyan)),
                Span::raw("quit"),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(if self.editing_out {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    })
                    .title(" console "),
            ),
            area,
        );
    }
}

fn style_preview_line(line: &str, text: bool) -> Line<'static> {
    if !text {
        let (address, rest) = line.split_at(line.len().min(8));
        return Line::from(vec![
            Span::styled(address.to_owned(), Style::default().fg(Color::DarkGray)),
            Span::styled(rest.to_owned(), Style::default().fg(Color::Gray)),
        ]);
    }
    let trimmed = line.trim_start();
    let style = if trimmed.starts_with("<!--") {
        Style::default().fg(Color::DarkGray)
    } else if trimmed.starts_with("<?") {
        Style::default().fg(Color::Magenta)
    } else if trimmed.starts_with('<') {
        Style::default().fg(Color::Cyan)
    } else if trimmed.starts_with('}') || trimmed.starts_with(')') {
        Style::default().fg(Color::DarkGray)
    } else if trimmed.contains('=') {
        Style::default().fg(Color::Rgb(200, 190, 150))
    } else {
        Style::default().fg(Color::Gray)
    };
    Line::from(Span::styled(line.to_owned(), style))
}

fn is_text(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    let printable = sample
        .iter()
        .filter(|&&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7f).contains(&b))
        .count();
    !sample.is_empty() && printable * 100 / sample.len() >= 90
}

fn hexdump(bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            format!("{:08x}  {:<47}  |{ascii}|", row * 16, hex.join(" "))
        })
        .collect()
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let file = rpf9::MappedFile::open(&args.archive)?;
    let rpf = match file.parse() {
        Ok(rpf) => rpf,
        Err(error) => {
            eprintln!("{}: {error}", args.archive.display());
            std::process::exit(2);
        }
    };
    let name = args.archive.display().to_string();
    let mut app = App::new(name, rpf, args.out);
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}
