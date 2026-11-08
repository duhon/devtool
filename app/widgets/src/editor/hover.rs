pub struct VariableHover<'a> {
    pub start: usize,
    pub end: usize,
    pub key: &'a str,
    pub value: &'a str,
}
pub fn hover_at<'a>(
    line: &str,
    ch: usize,
    hints: &'a [(String, String)],
) -> Option<VariableHover<'a>> {
    line.match_indices("{{").find_map(|(start, _)| {
        let end = start + 2 + line[start + 2..].find("}}")? + 2;
        let from = line[..start].encode_utf16().count();
        let to = line[..end].encode_utf16().count();
        if !(from..to).contains(&ch) {
            return None;
        }
        hints
            .iter()
            .find(|(key, _)| key == &line[start..end])
            .map(|(key, value)| VariableHover {
                start: from,
                end: to,
                key,
                value,
            })
    })
}
