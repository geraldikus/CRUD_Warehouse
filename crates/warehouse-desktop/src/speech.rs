//! Strip markdown before Piper speaks a reply.
//!
//! The chat bubble keeps the original text. Speech only needs the words.

/// Markdown-free text for TTS. Empty when nothing remains to say.
pub(crate) fn for_speech(text: &str) -> String {
    let without_links = strip_links(text);
    let without_ticks = without_links.replace('`', "");
    let without_strong = without_ticks.replace("**", "").replace("__", "");
    let without_emphasis = without_strong.replace('*', "").replace('_', "");
    collapse(&strip_heading_marks(&without_emphasis))
}

/// `[label](url)` becomes `label`. Unclosed markup is left as-is.
fn strip_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '['
            && let Some(relative_close) = chars[index + 1..].iter().position(|&ch| ch == ']')
        {
            let close = index + 1 + relative_close;
            if close + 1 < chars.len()
                && chars[close + 1] == '('
                && let Some(relative_paren) = chars[close + 2..].iter().position(|&ch| ch == ')')
            {
                out.extend(chars[index + 1..close].iter());
                index = close + 2 + relative_paren + 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn strip_heading_marks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_start_matches('#').trim_start());
    }
    out
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_markers_are_dropped() {
        assert_eq!(for_speech("**bold**"), "bold");
    }

    #[test]
    fn code_ticks_are_dropped() {
        assert_eq!(for_speech("`code`"), "code");
    }

    #[test]
    fn markdown_link_keeps_the_label() {
        assert_eq!(for_speech("[cows](http://x)"), "cows");
    }

    #[test]
    fn markers_only_leave_nothing_to_say() {
        assert!(for_speech("**").is_empty());
    }

    #[test]
    fn heading_hashes_and_extra_spaces_go() {
        assert_eq!(for_speech("##  Added **10** cows"), "Added 10 cows");
    }
}
