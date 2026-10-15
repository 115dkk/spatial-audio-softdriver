//! Pure toast payload construction, tested on every platform.

pub(crate) fn document(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape(title),
        escape(body)
    )
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_xml_escapes_both_text_nodes() {
        assert_eq!(
            document("제목 & < > \" '", "</text>&amp;\"'"),
            "<toast><visual><binding template=\"ToastGeneric\"><text>제목 &amp; &lt; &gt; &quot; &apos;</text><text>&lt;/text&gt;&amp;amp;&quot;&apos;</text></binding></visual></toast>"
        );
    }

    #[test]
    fn toast_xml_preserves_korean_and_empty_text() {
        assert_eq!(
            escape("공간 소프트리시버\n기본 장치"),
            "공간 소프트리시버\n기본 장치"
        );
        assert_eq!(
            document("", ""),
            "<toast><visual><binding template=\"ToastGeneric\"><text></text><text></text></binding></visual></toast>"
        );
    }
}
