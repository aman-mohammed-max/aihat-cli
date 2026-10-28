//! The message box: a small multi-line editor with grapheme-aware cursor
//! movement and word wrapping.

use crate::ui::text::width;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default, Clone)]
pub struct Composer {
    text: String,
    /// Byte offset of the cursor, always on a grapheme boundary.
    cursor: usize,
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.text[..from]
            .grapheme_indices(true)
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..]
            .graphemes(true)
            .next()
            .map(|g| from + g.len())
            .unwrap_or(self.text.len())
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.prev_boundary(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let end = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len())
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    pub fn word_left(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        self.cursor = trimmed
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + trimmed[i..].chars().next().map(char::len_utf8).unwrap_or(1))
            .unwrap_or(0);
    }

    pub fn word_right(&mut self) {
        let after = &self.text[self.cursor..];
        let skip_space = after.len() - after.trim_start().len();
        let rest = &after[skip_space..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.cursor += skip_space + word;
    }

    /// ctrl+w: delete the word before the cursor.
    pub fn delete_word(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.text.replace_range(self.cursor..end, "");
    }

    /// ctrl+u: delete from the start of the line to the cursor.
    pub fn delete_to_start(&mut self) {
        let start = self.line_start();
        let start = if start == self.cursor && start > 0 { start - 1 } else { start };
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// ctrl+k: delete from the cursor to the end of the line.
    pub fn delete_to_end(&mut self) {
        let end = self.line_end();
        let end = if end == self.cursor && end < self.text.len() { end + 1 } else { end };
        self.text.replace_range(self.cursor..end, "");
    }

    /// Move up one visual row. Returns false when already on the first row,
    /// so the caller can recall history instead.
    pub fn up(&mut self, cols: usize) -> bool {
        let rows = self.rows(cols);
        let (row, col) = self.position(&rows);
        if row == 0 {
            return false;
        }
        self.cursor = offset_at(&self.text, &rows[row - 1], col);
        true
    }

    pub fn down(&mut self, cols: usize) -> bool {
        let rows = self.rows(cols);
        let (row, col) = self.position(&rows);
        if row + 1 >= rows.len() {
            return false;
        }
        self.cursor = offset_at(&self.text, &rows[row + 1], col);
        true
    }

    /// The `@token` or `/command` the cursor is in, with its byte range.
    pub fn token_at_cursor(&self) -> Option<(Range<usize>, &str)> {
        let start = self.text[..self.cursor]
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.text[self.cursor..]
            .find(char::is_whitespace)
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
        (start < end).then(|| (start..end, &self.text[start..end]))
    }

    pub fn replace_range(&mut self, range: Range<usize>, with: &str) {
        self.text.replace_range(range.clone(), with);
        self.cursor = range.start + with.len();
    }

    /// Visual rows (byte ranges) when wrapped to `cols` columns. Words wrap
    /// whole when they fit; hard newlines always start a new row.
    pub fn rows(&self, cols: usize) -> Vec<Range<usize>> {
        let cols = cols.max(2);
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in self.text.split('\n') {
            let mut row_start = line_start;
            let mut used = 0;
            let mut last_space: Option<usize> = None;
            for (offset, grapheme) in line.grapheme_indices(true) {
                let at = line_start + offset;
                let w = width(grapheme);
                if used + w > cols - 1 {
                    let cut = match last_space {
                        Some(space) if space > row_start => space,
                        _ => at,
                    };
                    rows.push(row_start..cut);
                    row_start = cut;
                    used = width(&self.text[row_start..at]);
                    last_space = None;
                }
                if grapheme == " " {
                    last_space = Some(at + 1);
                }
                used += w;
            }
            rows.push(row_start..line_start + line.len());
            line_start += line.len() + 1;
        }
        rows
    }

    /// (row, column) of the cursor in `rows`.
    pub fn position(&self, rows: &[Range<usize>]) -> (usize, usize) {
        for (index, row) in rows.iter().enumerate() {
            let last = index + 1 == rows.len();
            let next_starts_here = rows.get(index + 1).is_some_and(|next| next.start == self.cursor);
            if self.cursor >= row.start && self.cursor <= row.end && (last || !next_starts_here || self.cursor < row.end) {
                return (index, width(&self.text[row.start..self.cursor]));
            }
        }
        (rows.len().saturating_sub(1), 0)
    }
}

fn offset_at(text: &str, row: &Range<usize>, col: usize) -> usize {
    let mut used = 0;
    for (offset, grapheme) in text[row.clone()].grapheme_indices(true) {
        let w = width(grapheme);
        if used + w > col {
            return row.start + offset;
        }
        used += w;
    }
    row.end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_respects_graphemes() {
        let mut composer = Composer::default();
        composer.insert("héllo 👋🏽");
        composer.backspace();
        assert_eq!(composer.text(), "héllo ");
        composer.left();
        composer.left();
        composer.insert("X");
        assert_eq!(composer.text(), "héllXo ");
    }

    #[test]
    fn rows_wrap_at_word_boundaries() {
        let mut composer = Composer::default();
        composer.insert("one two three four");
        let rows = composer.rows(10);
        let text: Vec<&str> = rows.iter().map(|r| &composer.text()[r.clone()]).collect();
        assert_eq!(text, vec!["one two ", "three ", "four"]);
    }

    #[test]
    fn up_and_down_move_between_rows() {
        let mut composer = Composer::default();
        composer.insert("first\nsecond");
        assert!(composer.up(40));
        assert_eq!(composer.cursor(), 5);
        assert!(!composer.up(40));
        assert!(composer.down(40));
    }

    #[test]
    fn token_at_cursor_finds_mentions() {
        let mut composer = Composer::default();
        composer.insert("does @vm");
        let (range, token) = composer.token_at_cursor().unwrap();
        assert_eq!(token, "@vm");
        composer.replace_range(range, "@src/vm.rs ");
        assert_eq!(composer.text(), "does @src/vm.rs ");
    }
}
