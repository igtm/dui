use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use textwrap::core::display_width;

#[derive(Clone, Debug)]
pub struct StyledChunk {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AnsiStyle {
    fg: Option<Color>,
    bg: Option<Color>,
    modifiers: Modifier,
}

impl AnsiStyle {
    fn to_style(self) -> Style {
        let mut style = Style::default();
        if let Some(fg) = self.fg {
            style = style.fg(fg);
        }
        if let Some(bg) = self.bg {
            style = style.bg(bg);
        }
        if !self.modifiers.is_empty() {
            style = style.add_modifier(self.modifiers);
        }
        style
    }
}

pub fn strip_ansi(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            skip_escape_sequence(&mut chars);
            continue;
        }
        plain.push(ch);
    }

    plain
}

pub fn parse_ansi(text: &str) -> Vec<StyledChunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut style = AnsiStyle::default();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            push_chunk(&mut chunks, style.to_style(), std::mem::take(&mut current));
            if let Some(sequence) = read_escape_sequence(&mut chars) {
                apply_escape_sequence(&sequence, &mut style);
            }
            continue;
        }

        current.push(ch);
    }

    push_chunk(&mut chunks, style.to_style(), current);
    chunks
}

pub fn wrap_chunks(chunks: &[StyledChunk], width: usize) -> Vec<Vec<StyledChunk>> {
    let width = width.max(1);
    let mut lines = vec![Vec::new()];
    let mut current_width = 0;

    for chunk in chunks {
        let mut buffer = String::new();
        for ch in chunk.text.chars() {
            let mut encoded = [0u8; 4];
            let ch_width = display_width(ch.encode_utf8(&mut encoded));
            if current_width > 0 && current_width + ch_width > width {
                push_chunk(
                    lines.last_mut().expect("line exists"),
                    chunk.style,
                    std::mem::take(&mut buffer),
                );
                lines.push(Vec::new());
                current_width = 0;
            }

            buffer.push(ch);
            current_width += ch_width;
        }

        push_chunk(
            lines.last_mut().expect("line exists"),
            chunk.style,
            buffer,
        );
    }

    lines
}

pub fn plain_text(chunks: &[StyledChunk]) -> String {
    chunks.iter().map(|chunk| chunk.text.as_str()).collect()
}

pub fn chunks_to_line(chunks: Vec<StyledChunk>) -> Line<'static> {
    if chunks.is_empty() {
        return Line::from(String::new());
    }

    Line::from(
        chunks
            .into_iter()
            .map(|chunk| Span::styled(chunk.text, chunk.style))
            .collect::<Vec<_>>(),
    )
}

fn push_chunk(chunks: &mut Vec<StyledChunk>, style: Style, text: String) {
    if text.is_empty() {
        return;
    }

    if let Some(existing) = chunks.last_mut() {
        if existing.style == style {
            existing.text.push_str(&text);
            return;
        }
    }

    chunks.push(StyledChunk { text, style });
}

fn read_escape_sequence<I>(chars: &mut std::iter::Peekable<I>) -> Option<String>
where
    I: Iterator<Item = char>,
{
    match chars.next() {
        Some('[') => {
            let mut sequence = String::new();
            sequence.push('[');
            for ch in chars.by_ref() {
                sequence.push(ch);
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    break;
                }
            }
            Some(sequence)
        }
        Some(']') => {
            let mut last = ']';
            for ch in chars.by_ref() {
                if ch == '\u{7}' || (last == '\u{1b}' && ch == '\\') {
                    break;
                }
                last = ch;
            }
            None
        }
        Some(_) | None => None,
    }
}

fn skip_escape_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    let _ = read_escape_sequence(chars);
}

fn apply_escape_sequence(sequence: &str, style: &mut AnsiStyle) {
    if !sequence.starts_with('[') || !sequence.ends_with('m') {
        return;
    }

    let params = &sequence[1..sequence.len().saturating_sub(1)];
    let params = if params.is_empty() {
        vec![0]
    } else {
        params
            .split(';')
            .map(|part| part.parse::<u16>().unwrap_or(0))
            .collect::<Vec<_>>()
    };

    let mut index = 0;
    while index < params.len() {
        match params[index] {
            0 => *style = AnsiStyle::default(),
            1 => style.modifiers.insert(Modifier::BOLD),
            2 => style.modifiers.insert(Modifier::DIM),
            3 => style.modifiers.insert(Modifier::ITALIC),
            4 => style.modifiers.insert(Modifier::UNDERLINED),
            5 => style.modifiers.insert(Modifier::SLOW_BLINK),
            6 => style.modifiers.insert(Modifier::RAPID_BLINK),
            7 => style.modifiers.insert(Modifier::REVERSED),
            8 => style.modifiers.insert(Modifier::HIDDEN),
            9 => style.modifiers.insert(Modifier::CROSSED_OUT),
            21 | 22 => style
                .modifiers
                .remove(Modifier::BOLD | Modifier::DIM),
            23 => style.modifiers.remove(Modifier::ITALIC),
            24 => style.modifiers.remove(Modifier::UNDERLINED),
            25 => style
                .modifiers
                .remove(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            27 => style.modifiers.remove(Modifier::REVERSED),
            28 => style.modifiers.remove(Modifier::HIDDEN),
            29 => style.modifiers.remove(Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(indexed_color(params[index] - 30, false)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(indexed_color(params[index] - 40, false)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(indexed_color(params[index] - 90, true)),
            100..=107 => style.bg = Some(indexed_color(params[index] - 100, true)),
            38 | 48 => {
                let is_foreground = params[index] == 38;
                if let Some((color, consumed)) = extended_color(&params[index + 1..]) {
                    if is_foreground {
                        style.fg = Some(color);
                    } else {
                        style.bg = Some(color);
                    }
                    index += consumed;
                }
            }
            _ => {}
        }
        index += 1;
    }
}

fn indexed_color(code: u16, bright: bool) -> Color {
    match (bright, code) {
        (false, 0) => Color::Black,
        (false, 1) => Color::Red,
        (false, 2) => Color::Green,
        (false, 3) => Color::Yellow,
        (false, 4) => Color::Blue,
        (false, 5) => Color::Magenta,
        (false, 6) => Color::Cyan,
        (false, 7) => Color::Gray,
        (true, 0) => Color::DarkGray,
        (true, 1) => Color::LightRed,
        (true, 2) => Color::LightGreen,
        (true, 3) => Color::LightYellow,
        (true, 4) => Color::LightBlue,
        (true, 5) => Color::LightMagenta,
        (true, 6) => Color::LightCyan,
        (true, 7) => Color::White,
        _ => Color::Reset,
    }
}

fn extended_color(params: &[u16]) -> Option<(Color, usize)> {
    match params {
        [5, value, ..] => Some((Color::Indexed(*value as u8), 2)),
        [2, red, green, blue, ..] => {
            Some((Color::Rgb(*red as u8, *green as u8, *blue as u8), 4))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_sgr_sequences() {
        assert_eq!(strip_ansi("\u{1b}[31merror\u{1b}[0m"), "error");
    }

    #[test]
    fn parse_ansi_maps_standard_colors() {
        let chunks = parse_ansi("\u{1b}[31merror\u{1b}[0m");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "error");
        assert_eq!(chunks[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn wrap_chunks_preserves_style_across_lines() {
        let chunks = parse_ansi("\u{1b}[31mabcdef\u{1b}[0m");
        let wrapped = wrap_chunks(&chunks, 3);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(plain_text(&wrapped[0]), "abc");
        assert_eq!(plain_text(&wrapped[1]), "def");
        assert_eq!(wrapped[0][0].style.fg, Some(Color::Red));
        assert_eq!(wrapped[1][0].style.fg, Some(Color::Red));
    }
}
