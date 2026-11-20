use std::io::{self, BufRead, IsTerminal, Read, Write};

/// The longest line accepted at an interactive prompt, with room for a pasted list of field names.
const MAX_PROMPT_BYTES: u64 = 1_048_576;

/// Reads one line typed at a prompt, refusing a line longer than the limit.
pub(crate) fn read_line(input: &mut impl BufRead, line: &mut String) -> io::Result<usize> {
    #[expect(clippy::disallowed_methods, reason = "`take` limits this read")]
    let read = input.by_ref().take(MAX_PROMPT_BYTES + 1).read_line(line)?;
    if read as u64 > MAX_PROMPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The line exceeds the prompt limit.",
        ));
    }
    Ok(read)
}

pub(crate) fn uses_color(terminal: bool, term: Option<&str>, no_color: bool) -> bool {
    // IsTerminal and TERM do not establish Windows console ANSI processing.
    cfg!(not(windows))
        && terminal
        && !no_color
        && term.is_some_and(|term| {
            matches!(
                term,
                "ansi" | "linux" | "xterm" | "screen" | "tmux" | "rxvt"
            ) || ["xterm-", "screen-", "tmux-", "rxvt-"]
                .iter()
                .any(|prefix| term.starts_with(prefix))
        })
}

fn enabled(terminal: bool) -> bool {
    uses_color(
        terminal,
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("NO_COLOR").is_some(),
    )
}

fn write_warning(out: &mut impl Write, message: &str, color: bool) -> io::Result<()> {
    let message = crate::human(message);
    if color {
        writeln!(out, "\x1b[33mWarning: {message}\x1b[0m")
    } else {
        writeln!(out, "Warning: {message}")
    }
}

fn write_secondary(out: &mut impl Write, message: &str, color: bool) -> io::Result<()> {
    let message = crate::human(message);
    if color {
        writeln!(out, "\x1b[2m{message}\x1b[0m")
    } else {
        writeln!(out, "{message}")
    }
}

pub(crate) fn secondary(out: &mut impl Write, message: &str, terminal: bool) -> io::Result<()> {
    write_secondary(out, message, enabled(terminal))
}

pub(crate) fn warning_stdout(message: &str) -> io::Result<()> {
    let out = io::stdout();
    write_warning(&mut out.lock(), message, enabled(out.is_terminal()))
}

pub(crate) fn warning_stderr(message: &str) -> io::Result<()> {
    let out = io::stderr();
    write_warning(&mut out.lock(), message, enabled(out.is_terminal()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_line_is_read_whole_or_refused_at_the_limit() {
        let mut line = String::new();
        let mut input = "yes\nnext\n".as_bytes();
        assert_eq!(read_line(&mut input, &mut line).unwrap(), 4);
        assert_eq!(line, "yes\n");
        assert_eq!(input, b"next\n");
        assert_eq!(read_line(&mut "".as_bytes(), &mut line).unwrap(), 0);

        let limit = MAX_PROMPT_BYTES as usize;
        for longest in [format!("{}\n", "a".repeat(limit - 1)), "a".repeat(limit)] {
            line.clear();
            assert_eq!(
                read_line(&mut longest.as_bytes(), &mut line).unwrap(),
                limit
            );
        }
        assert_eq!(limit, 1_048_576);
        let too_long = "a".repeat(limit + 10);
        let mut input = too_long.as_bytes();
        line.clear();
        assert_eq!(
            read_line(&mut input, &mut line).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // The read stops one byte past the limit and leaves the rest unread.
        assert_eq!(input.len(), 9);
    }

    #[test]
    fn warning_color_follows_its_own_stream_and_opt_out() {
        let expected = cfg!(not(windows));
        for (stdout_tty, stderr_tty) in [(false, true), (true, false)] {
            assert_eq!(
                uses_color(stdout_tty, Some("xterm"), false),
                expected && stdout_tty
            );
            assert_eq!(
                uses_color(stderr_tty, Some("xterm"), false),
                expected && stderr_tty
            );
        }
        assert!(!uses_color(true, Some("xterm"), true));
        assert!(!uses_color(true, Some("dumb"), false));
        assert!(!uses_color(true, None, false));
    }

    #[test]
    fn warnings_remain_labelled_escape_controls_and_reset_color() {
        let mut out = Vec::new();
        write_warning(&mut out, "new\nsource\x1b[0m", true).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[33mWarning: new\\nsource\\u{1b}[0m\x1b[0m\n"
        );
        let mut out = Vec::new();
        write_warning(&mut out, "refresh disabled", false).unwrap();
        assert_eq!(out, b"Warning: refresh disabled\n");
    }

    #[test]
    fn secondary_text_preserves_controls_and_resets_muted_style() {
        let mut out = Vec::new();
        write_secondary(&mut out, "phase\n\x1b[0m", true).unwrap();
        assert_eq!(out, b"\x1b[2mphase\\n\\u{1b}[0m\x1b[0m\n");
        let mut out = Vec::new();
        write_secondary(
            &mut out,
            "Recorded inventory refresh phase: prepared",
            false,
        )
        .unwrap();
        assert_eq!(out, b"Recorded inventory refresh phase: prepared\n");
    }
}
