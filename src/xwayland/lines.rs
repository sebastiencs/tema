//! Split Xwayland's stderr into lines.
//!
//! A line ends at `\n`. A `\r` immediately before that is dropped, so a CRLF
//! terminator does not stay in the text. Invalid UTF-8 is replaced. A partial
//! line longer than 4096 bytes is emitted without waiting for a newline.

const MAX_LINE: usize = 4096;

#[derive(Debug, Default)]
pub(crate) struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    pub(crate) fn push(&mut self, bytes: &[u8], mut emit: impl FnMut(&str)) {
        self.pending.extend_from_slice(bytes);
        self.emit_lines(&mut emit);
    }

    pub(crate) fn finish(&mut self, mut emit: impl FnMut(&str)) {
        self.emit_lines(&mut emit);
        if self.pending.is_empty() {
            return;
        }
        emit_text(&self.pending, &mut emit);
        self.pending.clear();
    }

    fn emit_lines(&mut self, emit: &mut impl FnMut(&str)) {
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            emit_text(&line, emit);
        }
        while self.pending.len() > MAX_LINE {
            let line: Vec<u8> = self.pending.drain(..MAX_LINE).collect();
            emit_text(&line, emit);
        }
    }
}

fn emit_text(bytes: &[u8], emit: &mut impl FnMut(&str)) {
    emit(&String::from_utf8_lossy(bytes));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pushed(chunks: &[&[u8]]) -> Vec<String> {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        for chunk in chunks {
            splitter.push(chunk, |line| lines.push(line.to_string()));
        }
        lines
    }

    #[test]
    fn lines_split_across_chunks_and_several_arrive_together() {
        assert_eq!(pushed(&[b"hel", b"lo\nwo", b"rld\n"]), vec!["hello", "world"]);
        assert_eq!(pushed(&[b"a\nb\nc\n"]), vec!["a", "b", "c"]);
        assert_eq!(pushed(&[b"a\nb"]), vec!["a"]);
    }

    #[test]
    fn crlf_is_one_terminator() {
        assert_eq!(pushed(&[b"a\r\nb\r\n"]), vec!["a", "b"]);
        assert_eq!(pushed(&[b"a\r", b"\n"]), vec!["a"]);
        assert_eq!(pushed(&[b"a\r\r\n"]), vec!["a\r"]);
    }

    #[test]
    fn a_partial_line_past_4096_bytes_is_emitted_early() {
        let mut bytes = vec![b'x'; MAX_LINE];
        assert!(pushed(&[&bytes]).is_empty());

        bytes.push(b'y');
        let lines = pushed(&[&bytes]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "x".repeat(MAX_LINE));

        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        splitter.push(&bytes, |line| lines.push(line.to_string()));
        splitter.finish(|line| lines.push(line.to_string()));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "y");
    }

    #[test]
    fn finish_flushes_the_partial_line_and_replaces_invalid_utf8() {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        splitter.finish(|line| lines.push(line.to_string()));
        assert!(lines.is_empty());

        splitter.push(b"par", |line| lines.push(line.to_string()));
        splitter.finish(|line| lines.push(line.to_string()));
        assert_eq!(lines, vec!["par"]);

        splitter.push(b"\xff\n", |line| lines.push(line.to_string()));
        assert_eq!(lines.last().unwrap(), "\u{FFFD}");
    }
}
