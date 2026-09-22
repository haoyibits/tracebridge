//! JSON with comments and trailing commas, as VS Code writes it (vscode/jsonc.py).

use std::path::Path;

use serde_json::Value;

use crate::bridge_error;
use crate::errors::Result;

/// `strip_comments`: remove `//` and `/* */` comments outside strings; newlines
/// inside comments are kept so error positions stay meaningful.
pub fn strip_comments(source: &str) -> Result<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut result = String::with_capacity(source.len());
    let (mut in_string, mut escaped, mut line_comment, mut block_comment) =
        (false, false, false, false);
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        let following = chars.get(index + 1).copied();
        if line_comment {
            if character == '\n' {
                line_comment = false;
                result.push(character);
            }
        } else if block_comment {
            if character == '*' && following == Some('/') {
                block_comment = false;
                index += 1;
            } else if character == '\n' {
                result.push(character);
            }
        } else if in_string {
            result.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
            result.push(character);
        } else if character == '/' && following == Some('/') {
            line_comment = true;
            index += 1;
        } else if character == '/' && following == Some('*') {
            block_comment = true;
            index += 1;
        } else {
            result.push(character);
        }
        index += 1;
    }
    if block_comment {
        return Err(bridge_error!("unterminated block comment in JSONC"));
    }
    Ok(result)
}

/// Python's `str.isspace`, which also counts the ASCII separators 0x1C..0x1F.
fn is_python_space(character: char) -> bool {
    character.is_whitespace() || ('\x1c'..='\x1f').contains(&character)
}

/// `strip_trailing_commas`: drop a comma that is followed (after whitespace)
/// by `}` or `]`. A comma at the very end of the text is kept, so it still
/// fails to parse.
pub fn strip_trailing_commas(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut result = String::with_capacity(source.len());
    let (mut in_string, mut escaped) = (false, false);
    for (index, &character) in chars.iter().enumerate() {
        if in_string {
            result.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
            result.push(character);
        } else if character == ',' {
            let next = chars[index + 1..]
                .iter()
                .copied()
                .find(|&c| !is_python_space(c));
            if !matches!(next, Some('}' | ']')) {
                result.push(character);
            }
        } else {
            result.push(character);
        }
    }
    result
}

/// `loads`.
pub fn loads(source: &str, source_name: &str) -> Result<Value> {
    let cleaned = strip_trailing_commas(&strip_comments(source)?);
    serde_json::from_str(&cleaned)
        .map_err(|error| bridge_error!("cannot parse {source_name}: {error}"))
}

/// `load`: read UTF-8 (a BOM is ignored) and parse.
pub fn load(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path)
        .map_err(|error| bridge_error!("cannot read {}: {error}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| bridge_error!("cannot read {}: {error}", path.display()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    loads(text, &path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // test_jsonc.py: test_comments_and_trailing_commas
    #[test]
    fn comments_and_trailing_commas() {
        let document = loads(
            r#"
            {
                // line comment
                "url": "https://example.com/a//b",
                "items": [1, 2,],
                /* block comment */
            }
            "#,
            "<string>",
        )
        .unwrap();
        assert_eq!(document["url"], "https://example.com/a//b");
        assert_eq!(document["items"], serde_json::json!([1, 2]));
    }

    // test_jsonc.py: test_unterminated_block_comment_is_rejected
    #[test]
    fn unterminated_block_comment_is_rejected() {
        let error = loads(r#"{"value": 1 /*"#, "<string>").unwrap_err();
        assert!(error.0.contains("unterminated"));
    }

    // test_jsonc.py: test_comma_at_end_of_file_is_not_silently_removed
    #[test]
    fn comma_at_end_of_file_is_not_silently_removed() {
        assert!(loads(r#"{"value": 1},"#, "<string>").is_err());
    }

    #[test]
    fn strings_keep_comment_markers_and_escaped_quotes() {
        let document = loads(r#"{"a": "x\" // not a comment ,}", "b": "/*"}"#, "s").unwrap();
        assert_eq!(document["a"], "x\" // not a comment ,}");
        assert_eq!(document["b"], "/*");
    }

    #[test]
    fn block_comment_keeps_line_breaks() {
        assert_eq!(strip_comments("a/* x\ny */b").unwrap(), "a\nb");
        assert_eq!(strip_comments("a // c\nb").unwrap(), "a \nb");
    }

    #[test]
    fn bom_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, "\u{feff}{\"a\": 1}").unwrap();
        assert_eq!(load(&path).unwrap()["a"], 1);
    }
}
