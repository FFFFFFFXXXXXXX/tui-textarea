use crate::ratatui::buffer::Buffer;
use crate::ratatui::layout::Rect;
use crate::ratatui::text::{Span, Text};
use crate::ratatui::widgets::{Paragraph, Widget};
use crate::textarea::TextArea;
use crate::util::num_digits;
#[cfg(feature = "ratatui")]
use ratatui::text::Line;
use std::cell::Cell;
use std::cmp;
#[cfg(feature = "tuirs")]
use tui::text::Spans as Line;

#[derive(Default, Debug, Clone)]
pub struct Viewport {
    width: Cell<u16>,
    height: Cell<u16>,
    row: Cell<u64>,
    col: Cell<u64>,
}

impl Viewport {
    fn store(&self, row: u64, col: u64, width: u16, height: u16) {
        self.width.set(width);
        self.height.set(height);
        self.row.set(row);
        self.col.set(col);
    }

    pub fn scroll_top(&self) -> (u64, u64) {
        (self.row.get(), self.col.get())
    }

    pub fn rect(&self) -> (u64, u64, u16, u16) {
        (self.row.get(), self.col.get(), self.width.get(), self.height.get())
    }

    pub fn position(&self) -> (u64, u64, u64, u64) {
        let (row_top, col_top, width, height) = self.rect();
        let row_bottom = row_top.saturating_add(height.into()).saturating_sub(1);
        let col_bottom = col_top.saturating_add(width.into()).saturating_sub(1);

        (
            row_top,
            col_top,
            cmp::max(row_top, row_bottom),
            cmp::max(col_top, col_bottom),
        )
    }

    pub fn scroll(&mut self, rows: i64, cols: i64) {
        self.row.set(self.row.get().saturating_add_signed(rows));
        self.col.set(self.col.get().saturating_add_signed(cols));
    }
}

pub struct Renderer<'a>(&'a TextArea<'a>);

impl<'a> Renderer<'a> {
    pub fn new(textarea: &'a TextArea<'a>) -> Self {
        Self(textarea)
    }

    #[inline]
    fn text(&self, top_row: usize, height: usize) -> Text<'a> {
        let lines_len = self.0.lines().len();
        let lnum_len = num_digits(lines_len);
        let bottom_row = cmp::min(top_row + height, lines_len);

        let (row, _) = self.0.cursor();
        Text::from_iter(
            self.0.lines()[top_row..bottom_row]
                .iter()
                .enumerate()
                .map(|(i, line)| self.0.line_spans(row, line, top_row + i, lnum_len)),
        )
    }

    #[inline]
    fn placeholder_text(&self) -> Text<'a> {
        let cursor = Span::styled(" ", self.0.cursor_style);
        let text = Span::raw(self.0.placeholder.as_str());
        Text::from(Line::from(vec![cursor, text]))
    }
}

impl<'a> Widget for Renderer<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Rect { width, height, .. } = if let Some(b) = self.0.block() {
            b.inner(area)
        } else {
            area
        };

        fn next_scroll_top(prev_top: u64, cursor: u64, length: u64) -> u64 {
            if cursor < prev_top {
                cursor
            } else if prev_top + length <= cursor {
                cursor + 1 - length
            } else {
                prev_top
            }
        }

        let (row, col) = self.0.cursor();
        let (top_row, top_col) = self.0.viewport.scroll_top();
        let top_row = next_scroll_top(top_row, row as u64, height.into());

        let line_number_offset = if self.0.line_number_style().is_some() {
            u64::from(num_digits(row)) + 1
        } else {
            0
        };

        let top_col = next_scroll_top(top_col, col as u64, u64::from(width) - line_number_offset);

        let (text, style) = if !self.0.placeholder.is_empty() && self.0.is_empty() {
            (self.placeholder_text(), self.0.placeholder_style)
        } else {
            (self.text(top_row as usize, height as usize), self.0.style())
        };

        // To get fine control over the text color and the surrrounding block they have to be rendered separately
        // see https://github.com/ratatui-org/ratatui/issues/144
        let mut text_area = area;
        let mut inner = Paragraph::new(text).style(style).alignment(self.0.alignment());
        if let Some(b) = self.0.block() {
            text_area = b.inner(area);
            b.clone().render(area, buf)
        }
        if top_col != 0 {
            inner = inner.scroll((0, top_col.try_into().unwrap()));
        }

        // Store scroll top position for rendering on the next tick
        self.0.viewport.store(top_row, top_col, width, height);

        inner.render(text_area, buf);
    }
}
