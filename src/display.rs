/// Text that came from an agent or a file, made safe to print: control
/// characters (also escape sequences and newlines) and bidirectional overrides
/// become `?`, and the length is bounded.
pub fn sanitize_display(s: &str, max_chars: usize) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control()
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)
            {
                '?'
            } else {
                c
            }
        })
        .take(max_chars)
        .collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}
