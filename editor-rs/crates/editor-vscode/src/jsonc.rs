//! Strip JSON-with-comments down to plain JSON.
//!
//! VS Code's theme, snippet, keybinding and language-configuration files are
//! all "JSONC": they allow `//` and `/* */` comments and trailing commas. This
//! turns such a document into strict JSON that `serde_json` accepts, while
//! leaving the contents of string literals untouched.

/// Remove comments and trailing commas, preserving string contents.
pub fn strip_jsonc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    // Line comment: skip to end of line.
                    for n in chars.by_ref() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    // Block comment: skip to "*/".
                    chars.next(); // consume '*'
                    let mut prev = '\0';
                    for n in chars.by_ref() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                }
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }

    remove_trailing_commas(&out)
}

/// Remove commas that directly precede a `}` or `]` (ignoring whitespace),
/// which strict JSON forbids but JSONC allows.
fn remove_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = input.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            // Look ahead past whitespace for a closing bracket.
            let next = chars[i + 1..]
                .iter()
                .find(|ch| !ch.is_whitespace())
                .copied();
            if matches!(next, Some('}') | Some(']')) {
                continue; // drop the trailing comma
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_and_block_comments() {
        let input = r#"{
            // a line comment
            "a": 1, /* inline */ "b": 2
            /* block
               comment */
        }"#;
        let cleaned = strip_jsonc(input);
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn keeps_comment_like_text_inside_strings() {
        let input = r#"{ "url": "http://example.com", "re": "a//b" }"#;
        let cleaned = strip_jsonc(input);
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["url"], "http://example.com");
        assert_eq!(v["re"], "a//b");
    }

    #[test]
    fn removes_trailing_commas() {
        let input = r#"{ "a": [1, 2, 3,], "b": 4, }"#;
        let cleaned = strip_jsonc(input);
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["a"][2], 3);
        assert_eq!(v["b"], 4);
    }
}
