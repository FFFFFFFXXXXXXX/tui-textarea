use crossterm::event::Event;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use tui_textarea::{CursorMove, Fullscreen, Input, Key, TextArea};

use std::borrow::Cow;
use std::fmt::Display;
use std::io::Write;
use std::path::{absolute, PathBuf};
use std::{env, fs, io};

macro_rules! error {
    ($fmt: expr $(, $args:tt)*) => {{
        Err(io::Error::new(io::ErrorKind::Other, format!($fmt $(, $args)*)))
    }};
}

fn main() -> io::Result<()> {
    let filepaths = env::args_os()
        .skip(1)
        .filter_map(|p| p.into_string().ok())
        .filter_map(|filepath| match shellexpand::full(&filepath) {
            Ok(Cow::Borrowed(s)) => Some(s.to_owned()),
            Ok(Cow::Owned(s)) => Some(s),
            Err(_) => None,
        })
        .filter_map(|p| absolute(p).ok());
    Editor::new(filepaths)?.run()
}

struct Editor<'a> {
    current: usize,
    buffers: Vec<Buffer<'a>>,
    term: Terminal<CrosstermBackend<io::Stdout>>,
    message: Option<Cow<'static, str>>,
}

#[derive(PartialEq)]
enum Status {
    Continue,
    Stop,
}

impl<'a> Editor<'a> {
    fn new<I>(paths: I) -> io::Result<Self>
    where
        I: Iterator,
        I::Item: Into<PathBuf>,
    {
        let buffers = paths.map(|p| Buffer::new(p.into())).collect::<io::Result<Vec<_>>>()?;
        if buffers.is_empty() {
            return error!("USAGE: ded FILE1 [FILE2...]");
        }
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        crossterm::execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let term = Terminal::new(backend)?;
        Ok(Self {
            current: 0,
            buffers,
            term,
            message: None,
        })
    }

    fn run(&mut self) -> io::Result<()> {
        // initial render so the content is immediately drawn to the terminal
        self.render()?;

        loop {
            // wait for next userinput (blocking!)
            let event = crossterm::event::read()?;
            // manually re-render on window resize because Event::Resize(_, _) gets ignored by tui_textarea
            if let Event::Resize(_, _) = event {
                self.render()?;
            }

            let event = event.into();
            // ignore Key::Null so we don't rerender unnecessarily
            if let Input { key: Key::Null, .. } = event {
                continue;
            }

            // process input / change state
            if self.process_input(event)? == Status::Stop {
                break;
            }

            // render state to terminal
            self.render()?;
        }

        Ok(())
    }

    fn process_input(&mut self, event: Input) -> io::Result<Status> {
        match event {
            Input { key: Key::F(11), .. } => {
                self.buffers[self.current].textarea.toggle_fullscreen();
            }
            Input { key: Key::F(12), .. } => {
                self.buffers[self.current].textarea.toggle_line_numbers();
            }

            Input {
                key: Key::Char('q'),
                ctrl: true,
                ..
            } => return Ok(Status::Stop),
            Input {
                key: Key::Char(char),
                alt: true,
                ctrl: false,
                shift: false,
            } if char.is_ascii_digit() => {
                let buf_idx = (char as u32 - '1' as u32) as usize;
                if buf_idx < self.buffers.len() && self.current != buf_idx {
                    self.current = buf_idx;
                    self.message = Some(format!("Switched to buffer #{}", self.current + 1).into());
                }
            }
            Input {
                key: Key::Char('s'),
                ctrl: true,
                ..
            } => {
                self.buffers[self.current].save()?;
                self.message = Some("Saved!".into());
            }

            event => {
                let buffer = &mut self.buffers[self.current];
                let textarea = &mut buffer.textarea;
                let search = &mut buffer.search;
                if search.open {
                    match event {
                        Input { key: Key::Down, .. } => {
                            if !textarea.search_forward(false) {
                                search.set_error(Some("Pattern not found"));
                            }
                        }
                        Input { key: Key::Up, .. } => {
                            if !textarea.search_back(false) {
                                search.set_error(Some("Pattern not found"));
                            }
                        }
                        Input { key: Key::Enter, .. } => {
                            if !textarea.search_forward(true) {
                                self.message = Some("Pattern not found".into());
                            }
                            search.close();
                            textarea.set_search_pattern("").unwrap();
                        }
                        Input { key: Key::Esc, .. } => {
                            search.close();
                            textarea.set_search_pattern("").unwrap();
                        }
                        input => {
                            if let Some(query) = search.input(input) {
                                let maybe_err = textarea.set_search_pattern(query).err();
                                search.set_error(maybe_err);
                            }
                        }
                    }
                } else {
                    match event {
                        Input {
                            key: Key::Char('f'),
                            ctrl: true,
                            ..
                        } => {
                            let search_pattern = {
                                let prev_search_pattern = search.open();
                                textarea.take_selection().unwrap_or(prev_search_pattern).to_owned()
                            };

                            search.set_pattern(&search_pattern);
                            let maybe_err = textarea.set_search_pattern(search_pattern).err();
                            search.set_error(maybe_err);
                        }
                        input => {
                            let buffer = &mut self.buffers[self.current];
                            buffer.modified |= buffer.textarea.input(input);
                        }
                    }
                }
            }
        };

        Ok(Status::Continue)
    }

    fn render(&mut self) -> io::Result<()> {
        let num_buffers = self.buffers.len();

        let buffer = &mut self.buffers[self.current];
        let textarea = &mut buffer.textarea;
        let search = &mut buffer.search;

        let search_height = search.height();
        let layout = Layout::default().direction(Direction::Vertical).constraints([
            Constraint::Length(search_height),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]);

        let half_fullscreen_layout = Layout::default().direction(Direction::Vertical).constraints([
            Constraint::Length(search_height),
            Constraint::Min(1),
            Constraint::Length(1),
        ]);

        let fullscreen_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(search_height), Constraint::Min(1)]);

        self.term.draw(|f| {
            match textarea.fullscreen() {
                Fullscreen::Off => {
                    let chunks = layout.split(f.size());

                    if search_height > 0 {
                        f.render_widget(search.textarea.widget(), chunks[0]);
                    }

                    f.render_widget(textarea.widget(), chunks[1]);

                    // Render status line
                    let modified = if buffer.modified { " [modified]" } else { "" };
                    let slot = format!("[{}/{}]", self.current + 1, num_buffers);
                    let path = format!(" {}{} ", buffer.path.display(), modified);
                    let (row, col) = textarea.cursor();
                    let cursor = format!("({},{})", row + 1, col + 1);
                    let status_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints(
                            [
                                Constraint::Length(slot.len() as u16),
                                Constraint::Min(1),
                                Constraint::Length(cursor.len() as u16),
                            ]
                            .as_ref(),
                        )
                        .split(chunks[2]);
                    let status_style = Style::default().add_modifier(Modifier::REVERSED);
                    f.render_widget(Paragraph::new(slot).style(status_style), status_chunks[0]);
                    f.render_widget(Paragraph::new(path).style(status_style), status_chunks[1]);
                    f.render_widget(Paragraph::new(cursor).style(status_style), status_chunks[2]);

                    // Render message at bottom
                    let message = if let Some(message) = self.message.take() {
                        Line::from(Span::raw(message))
                    } else if search_height > 0 {
                        Line::from(vec![
                            Span::raw("Press "),
                            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to jump to first match and close, "),
                            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to close, "),
                            Span::styled("↓", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to search next, "),
                            Span::styled("↑", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to search previous"),
                        ])
                    } else {
                        Line::from(vec![
                            Span::raw("Press "),
                            Span::styled("^Q", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to quit, "),
                            Span::styled("^S", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to save, "),
                            Span::styled("^F", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to search, "),
                            Span::styled("alt + BUF_ID", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(" to switch buffer"),
                        ])
                    };
                    f.render_widget(Paragraph::new(message), chunks[3]);
                }
                Fullscreen::Half => {
                    let chunks = half_fullscreen_layout.split(f.size());

                    if search_height > 0 {
                        f.render_widget(search.textarea.widget(), chunks[0]);
                    }

                    f.render_widget(textarea.widget(), chunks[1]);

                    // Render status line
                    let modified = if buffer.modified { " [modified]" } else { "" };
                    let slot = format!("[{}/{}]", self.current + 1, num_buffers);
                    let path = format!(" {}{} ", buffer.path.display(), modified);
                    let (row, col) = textarea.cursor();
                    let cursor = format!("({},{})", row + 1, col + 1);
                    let status_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints(
                            [
                                Constraint::Length(slot.len() as u16),
                                Constraint::Min(1),
                                Constraint::Length(cursor.len() as u16),
                            ]
                            .as_ref(),
                        )
                        .split(chunks[2]);
                    let status_style = Style::default().add_modifier(Modifier::REVERSED);
                    f.render_widget(Paragraph::new(slot).style(status_style), status_chunks[0]);
                    f.render_widget(Paragraph::new(path).style(status_style), status_chunks[1]);
                    f.render_widget(Paragraph::new(cursor).style(status_style), status_chunks[2]);
                }
                Fullscreen::Full => {
                    let chunks = fullscreen_layout.split(f.size());

                    if search_height > 0 {
                        f.render_widget(search.textarea.widget(), chunks[0]);
                    }

                    f.render_widget(textarea.widget(), chunks[1]);
                }
            }
        })?;

        Ok(())
    }
}

impl<'a> Drop for Editor<'a> {
    fn drop(&mut self) {
        self.term.show_cursor().unwrap();
        disable_raw_mode().unwrap();
        crossterm::execute!(self.term.backend_mut(), LeaveAlternateScreen).unwrap();
    }
}

struct Buffer<'a> {
    textarea: TextArea<'a>,
    path: PathBuf,
    modified: bool,
    search: SearchBox<'a>,
}

impl<'a> Buffer<'a> {
    fn new(path: PathBuf) -> io::Result<Self> {
        let mut textarea = if let Ok(md) = path.metadata() {
            if md.is_file() {
                let mut textarea = TextArea::new_from_file(&fs::File::open(&path)?)?;
                if textarea.lines().iter().any(|l| l.starts_with('\t')) {
                    textarea.set_hard_tab_indent(true);
                }
                textarea
            } else {
                return error!("{:?} is not a file", path);
            }
        } else {
            TextArea::default() // File does not exist
        };

        textarea.set_cursor_line_style(Style::default());
        textarea.set_line_number_style(Style::default().fg(Color::DarkGray));
        textarea.set_max_histories(100);

        Ok(Self {
            textarea,
            path,
            modified: false,
            search: SearchBox::default(),
        })
    }

    fn save(&mut self) -> io::Result<()> {
        if !self.modified {
            return Ok(());
        }

        let mut f = io::BufWriter::new(fs::File::create(&self.path)?);
        let lines = self.textarea.lines();
        for line in lines.iter().take(lines.len() - 1) {
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
        }

        if let Some(last_line) = lines.last() {
            f.write_all(last_line.as_bytes())?;
            if !last_line.is_empty() {
                f.write_all(b"\n")?;
            }
        }

        self.modified = false;
        Ok(())
    }
}

struct SearchBox<'a> {
    textarea: TextArea<'a>,
    open: bool,
}

impl<'a> Default for SearchBox<'a> {
    fn default() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().borders(Borders::ALL).title("Search"));
        textarea.set_cursor_line_style(Style::default());
        textarea.set_max_histories(100);

        Self { textarea, open: false }
    }
}

impl<'a> SearchBox<'a> {
    fn open(&mut self) -> &'_ str {
        self.open = true;
        self.textarea.lines()[0].as_str()
    }

    fn close(&mut self) {
        self.open = false;
        // Remove input for next search. Do not recreate `self.textarea` instance to keep undo history so that users can
        // restore previous input easily.
        self.textarea.move_cursor(CursorMove::End);
    }

    fn height(&self) -> u16 {
        if self.open {
            3
        } else {
            0
        }
    }

    fn set_pattern(&mut self, pattern: &str) {
        self.textarea.delete_line(false);
        self.textarea.insert_str(pattern);
    }

    fn input(&mut self, input: Input) -> Option<&'_ str> {
        match input {
            Input { key: Key::Enter, .. } => None, // disable inputs which inserts a newline
            input => {
                let modified = self.textarea.single_line_input(input);
                modified.then(|| self.textarea.lines()[0].as_str())
            }
        }
    }

    fn set_error(&mut self, err: Option<impl Display>) {
        let b = if let Some(err) = err {
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Search: {}", err))
                .style(Style::default().fg(Color::Red))
        } else {
            Block::default().borders(Borders::ALL).title("Search")
        };
        self.textarea.set_block(b);
    }
}
