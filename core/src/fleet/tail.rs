//! Incremental, tolerant reading of the append-only files the adapters follow (spec §4,
//! "Leitura incremental"): complete lines only, a restart after truncation, a jump to the tail on
//! first sight, and backwards paging for the full transcript.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::limits::TAIL_BYTES;
use super::source::Offset;

/// A single line longer than this ends backwards paging (the page would never fit it).
const LONGEST_LINE: u64 = 16 * 1024 * 1024;

pub fn read_complete_lines(path: &Path, from: Offset) -> io::Result<(Vec<String>, Offset)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    // `Offset(0)` reads from the start, however big the file: only the sentinel and a file that
    // shrank below the offset (truncated or rotated) restart at the tail.
    let as_tail = from == Offset::TAIL || from.0 > len;
    if !as_tail {
        if from.0 == len {
            return Ok((Vec::new(), from));
        }
        return read_from(&mut file, from.0, len);
    }
    let window = len.saturating_sub(TAIL_BYTES);
    if window == 0 {
        return read_from(&mut file, 0, len);
    }
    // Start one byte early: if that byte is a '\n' the window already begins on a line.
    file.seek(SeekFrom::Start(window - 1))?;
    let mut buf = Vec::with_capacity(TAIL_BYTES as usize + 1);
    file.by_ref().take(len - (window - 1)).read_to_end(&mut buf)?;
    let Some(newline) = buf.iter().position(|b| *b == b'\n') else {
        // One line longer than the window: no safe boundary yet. Try the window again next time.
        return Ok((Vec::new(), Offset::TAIL));
    };
    split_complete(&buf[newline + 1..], window - 1 + newline as u64 + 1)
}

fn read_from(file: &mut File, start: u64, len: u64) -> io::Result<(Vec<String>, Offset)> {
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.by_ref().take(len - start).read_to_end(&mut buf)?;
    split_complete(&buf, start)
}

/// The complete lines of `buf`, which begins at absolute byte `start`, and the offset just after
/// the last of them. A trailing fragment without '\n' is left for the next read.
fn split_complete(buf: &[u8], start: u64) -> io::Result<(Vec<String>, Offset)> {
    let Some(last) = buf.iter().rposition(|b| *b == b'\n') else {
        return Ok((Vec::new(), Offset(start)));
    };
    Ok((lines_of(&buf[..last]), Offset(start + last as u64 + 1)))
}

fn lines_of(buf: &[u8]) -> Vec<String> {
    buf.split(|b| *b == b'\n')
        .map(|raw| {
            String::from_utf8_lossy(raw)
                .trim_end_matches('\r')
                .to_owned()
        })
        .filter(|line| !line.trim().is_empty())
        .collect()
}

pub fn read_lines_before(
    path: &Path,
    before: Option<u64>,
    max_bytes: u64,
) -> io::Result<(Vec<String>, Option<u64>)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let end = match before {
        Some(at) => at.min(len),
        None => complete_end(&mut file, len)?,
    };
    let mut span = max_bytes.max(1);
    loop {
        let window = end.saturating_sub(span);
        // One byte of look-behind tells whether the window begins on a line.
        let from = window.saturating_sub(1);
        file.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        file.by_ref().take(end - from).read_to_end(&mut buf)?;
        let first = if window == 0 {
            Some(0)
        } else {
            buf.iter().position(|b| *b == b'\n').map(|i| i + 1)
        };
        match first {
            Some(first) if first < buf.len() || window == 0 => {
                let body = &buf[first..];
                let body = body.strip_suffix(b"\n").unwrap_or(body);
                let begin = from + first as u64;
                return Ok((lines_of(body), (begin > 0).then_some(begin)));
            }
            // No line starts inside the window (a line longer than the page): widen it.
            _ if span >= LONGEST_LINE => return Ok((Vec::new(), None)),
            _ => span *= 2,
        }
    }
}

/// The byte just after the last '\n' of the file, 0 when it has none: everything before it is
/// complete lines.
fn complete_end(file: &mut File, len: u64) -> io::Result<u64> {
    let mut end = len;
    while end > 0 {
        let start = end.saturating_sub(8192);
        file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0; (end - start) as usize];
        file.read_exact(&mut buf)?;
        if let Some(i) = buf.iter().rposition(|b| *b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// The last component of a working directory, with `/` or `\` as separator.
pub fn project_of(cwd: &str) -> Option<String> {
    cwd.split(['/', '\\'])
        .rfind(|part| !part.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_with(dir: &tempfile::TempDir, content: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, content).unwrap();
        path
    }

    fn numbered(count: usize) -> String {
        (0..count).map(|i| format!("line-{i:05}\n")).collect()
    }

    #[test]
    fn a_partial_last_line_is_not_returned_and_the_offset_stays_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"a\nb\nc");

        let (lines, offset) = read_complete_lines(&path, Offset(0)).unwrap();
        assert_eq!(lines, ["a", "b"]);
        assert_eq!(offset, Offset(4));

        // The writer finishes the line: it is now read whole, from where we stayed.
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"d\n").unwrap();
        let (lines, offset) = read_complete_lines(&path, offset).unwrap();
        assert_eq!(lines, ["cd"]);
        // "a\nb\nc" is 5 bytes and "d\n" adds 2: the file is 7 bytes and the line ended at its end.
        assert_eq!(offset, Offset(7));
    }

    #[test]
    fn reading_from_the_end_returns_nothing_and_keeps_the_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"a\n");
        let (lines, offset) = read_complete_lines(&path, Offset(2)).unwrap();
        assert!(lines.is_empty());
        assert_eq!(offset, Offset(2));
    }

    #[test]
    fn a_file_shorter_than_the_offset_restarts_from_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"one\ntwo\n");
        let (_, offset) = read_complete_lines(&path, Offset(0)).unwrap();
        assert_eq!(offset, Offset(8));

        std::fs::write(&path, b"x\n").unwrap(); // rotated or truncated
        let (lines, offset) = read_complete_lines(&path, offset).unwrap();
        assert_eq!(lines, ["x"]);
        assert_eq!(offset, Offset(2));
    }

    #[test]
    fn the_first_read_skips_to_the_line_after_the_64k_window() {
        let dir = tempfile::tempdir().unwrap();
        let body = numbered(10_000); // 110 000 bytes, well past the window
        let path = file_with(&dir, body.as_bytes());

        let (lines, offset) = read_complete_lines(&path, Offset::TAIL).unwrap();
        assert_eq!(offset, Offset(body.len() as u64));
        assert!(lines.len() < 10_000 && lines.len() > 5_000, "{}", lines.len());
        // Every line is whole: the window started mid-line and that fragment was dropped.
        assert!(lines.iter().all(|l| l.len() == 10 && l.starts_with("line-")));
        assert_eq!(lines.last().unwrap(), "line-09999");
        assert!(lines.first().unwrap().as_str() > "line-00000");
    }

    #[test]
    fn a_window_that_starts_exactly_on_a_line_keeps_that_line() {
        let dir = tempfile::tempdir().unwrap();
        // 5 bytes of header, then 1024 lines of exactly 64 bytes: the window starts at byte 5,
        // which is the first byte of a line, so nothing may be skipped.
        let mut body = String::from("head\n");
        for _ in 0..(TAIL_BYTES / 64) {
            body.push_str(&"x".repeat(63));
            body.push('\n');
        }
        let path = file_with(&dir, body.as_bytes());

        let (lines, _) = read_complete_lines(&path, Offset::TAIL).unwrap();
        assert_eq!(lines.len() as u64, TAIL_BYTES / 64);
        assert_eq!(lines[0].len(), 63);
    }

    #[test]
    fn a_tail_window_with_no_newline_returns_nothing_and_asks_to_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, &vec![b'x'; 70_000]);
        let (lines, offset) = read_complete_lines(&path, Offset::TAIL).unwrap();
        assert!(lines.is_empty());
        assert_eq!(offset, Offset::TAIL);
    }

    #[test]
    fn the_tail_of_a_file_within_the_window_starts_at_byte_zero_and_skips_nothing() {
        // Contract amendment 8: the skip to the next line only happens when the read started
        // mid-file, so a short file's first line is not lost.
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"a\nb\n");
        let (lines, offset) = read_complete_lines(&path, Offset::TAIL).unwrap();
        assert_eq!(lines, ["a", "b"]);
        assert_eq!(offset, Offset(4));
    }

    #[test]
    fn blank_lines_and_carriage_returns_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"a\r\n\r\n\nb\r\n");
        let (lines, _) = read_complete_lines(&path, Offset(0)).unwrap();
        assert_eq!(lines, ["a", "b"]);
    }

    #[test]
    fn a_missing_file_is_an_error_the_adapter_can_report() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_complete_lines(&dir.path().join("nope"), Offset(0)).is_err());
    }

    #[test]
    fn backwards_pages_walk_the_file_oldest_page_last() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..10).map(|i| format!("l{i}\n")).collect(); // 3 bytes each
        let path = file_with(&dir, body.as_bytes());

        let (page, cursor) = read_lines_before(&path, None, 9).unwrap();
        assert_eq!(page, ["l7", "l8", "l9"]);
        assert_eq!(cursor, Some(21));
        let (page, cursor) = read_lines_before(&path, cursor, 9).unwrap();
        assert_eq!(page, ["l4", "l5", "l6"]);
        assert_eq!(cursor, Some(12));
        let (page, cursor) = read_lines_before(&path, cursor, 9).unwrap();
        assert_eq!(page, ["l1", "l2", "l3"]);
        assert_eq!(cursor, Some(3));
        let (page, cursor) = read_lines_before(&path, cursor, 9).unwrap();
        assert_eq!(page, ["l0"]);
        assert_eq!(cursor, None);
    }

    #[test]
    fn a_backwards_page_ignores_an_unterminated_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"a\nb\nc");
        let (page, cursor) = read_lines_before(&path, None, 1024).unwrap();
        assert_eq!(page, ["a", "b"]);
        assert_eq!(cursor, None);
    }

    #[test]
    fn a_line_longer_than_the_page_still_comes_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let long = "y".repeat(100);
        let path = file_with(&dir, format!("w\nx\n{long}\n").as_bytes());

        // A 10-byte page cannot hold the long line, so the window widens until it does; walking
        // the cursors must still return every line exactly once, in order.
        let mut all: Vec<String> = Vec::new();
        let mut cursor = None;
        loop {
            let (mut page, next) = read_lines_before(&path, cursor, 10).unwrap();
            page.extend(all);
            all = page;
            match next {
                Some(at) => cursor = Some(at),
                None => break,
            }
        }
        assert_eq!(all, ["w", "x", long.as_str()]);
    }

    #[test]
    fn an_empty_file_has_no_earlier_page() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(&dir, b"");
        assert_eq!(read_lines_before(&path, None, 64).unwrap(), (vec![], None));
    }

    #[test]
    fn the_project_is_the_last_path_component_with_either_separator() {
        assert_eq!(project_of(r"C:\Projects\nucleos").as_deref(), Some("nucleos"));
        assert_eq!(project_of("/home/u/proj/").as_deref(), Some("proj"));
        assert_eq!(project_of(r"C:\Projects\nucleos\").as_deref(), Some("nucleos"));
        assert_eq!(project_of("/").as_deref(), None);
        assert_eq!(project_of("").as_deref(), None);
        assert_eq!(project_of("solo").as_deref(), Some("solo"));
    }
}
