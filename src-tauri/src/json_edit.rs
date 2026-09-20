//! Removing one entry from a JSON file the user maintains, without rewriting
//! the rest of it.
//!
//! Parsing a config file into [`serde_json::Value`] and serializing it back
//! reorders every object key — this crate's `Map` is sorted — and reflows the
//! whole document. For a file Mework edits on the user's behalf, and that a
//! project may well have committed, that turns "delete one server" into a diff
//! touching every line. So the edit is made on the text: locate the member's
//! span with a scanner that understands JSON's string escaping, and cut exactly
//! that span plus the separator that joined it to its neighbours.
//!
//! The caller is expected to have parsed the document already, so these
//! functions assume well-formed JSON and report a plain error when the text
//! does not match what the caller found.

/// Cuts `member` from the object at `path` (a sequence of object keys from the
/// root), returning the remaining text.
///
/// The document keeps its original key order, indentation and line endings;
/// only the removed member and one separating comma disappear.
pub fn remove_object_member(text: &str, path: &[&str], member: &str) -> Result<String, String> {
    let object = object_span_at(text, path)?;
    let members = object_members(text, object)?;
    let index = members
        .iter()
        .position(|item| item.key.as_deref() == Some(member))
        .ok_or_else(|| format!("没有找到条目 {member}"))?;
    Ok(cut(text, removal_span(&members, index, object)))
}

/// Cuts element `index` from the array at `path`, where the last step of
/// `path` may be an array index itself (hooks address a handler as
/// `hooks / <event> / <group> / hooks / <handler>`).
pub fn remove_array_element(text: &str, path: &[Step], index: usize) -> Result<String, String> {
    let array = value_span_at(text, path)?;
    let elements = array_elements(text, array)?;
    if index >= elements.len() {
        return Err(format!("数组里没有第 {index} 项"));
    }
    Ok(cut(text, removal_span(&elements, index, array)))
}

/// One step of a path into a JSON document.
#[derive(Clone, Copy, Debug)]
pub enum Step<'a> {
    Key(&'a str),
    Index(usize),
}

/// A half-open byte range of the source text.
type Span = (usize, usize);

/// One member of an object or element of an array.
///
/// `whole` is what removing it must cut — for an object member that starts at
/// the key's opening quote, not at the value — while `value` is what descending
/// into it must read. Conflating the two is the obvious bug here, so they are
/// separate fields rather than one span used for both.
struct Item {
    key: Option<String>,
    whole: Span,
    value: Span,
}

fn cut(text: &str, span: Span) -> String {
    let mut output = String::with_capacity(text.len());
    output.push_str(&text[..span.0]);
    output.push_str(&text[span.1..]);
    output
}

/// The bytes to remove for one member or element: the item itself, plus the
/// comma that attached it to a neighbour and the whitespace that came with it.
///
/// Removing the last remaining item leaves an empty container rather than a
/// syntax error, and removing a middle one leaves its predecessor's line ending
/// intact — the next item keeps the indentation it was written with.
fn removal_span(items: &[Item], index: usize, container: Span) -> Span {
    let item = &items[index];
    if items.len() == 1 {
        // Nothing else is left: take everything between the braces so the
        // container collapses to `{}` / `[]` rather than keeping stray space.
        return (container.0 + 1, container.1 - 1);
    }
    if index + 1 < items.len() {
        // Cut forward to where the next item starts, which consumes the comma
        // and the newline and indentation in front of that item.
        return (item.whole.0, items[index + 1].whole.0);
    }
    // The last of several: cut backwards from the previous item's end, which
    // consumes the comma that used to follow it.
    (items[index - 1].whole.1, item.whole.1)
}

fn object_span_at(text: &str, path: &[&str]) -> Result<Span, String> {
    let steps = path.iter().map(|key| Step::Key(key)).collect::<Vec<_>>();
    value_span_at(text, &steps)
}

fn value_span_at(text: &str, path: &[Step]) -> Result<Span, String> {
    let bytes = text.as_bytes();
    let mut span = value_span(bytes, skip_whitespace(bytes, 0))?;
    for step in path {
        span = match step {
            Step::Key(key) => object_members(text, span)?
                .into_iter()
                .find(|item| item.key.as_deref() == Some(*key))
                .map(|item| item.value)
                .ok_or_else(|| format!("文件里没有 {key}"))?,
            Step::Index(index) => array_elements(text, span)?
                .get(*index)
                .map(|item| item.value)
                .ok_or_else(|| format!("数组里没有第 {index} 项"))?,
        };
    }
    Ok(span)
}

/// Every member of the object at `span`.
fn object_members(text: &str, span: Span) -> Result<Vec<Item>, String> {
    let bytes = text.as_bytes();
    if bytes.get(span.0) != Some(&b'{') {
        return Err("这里不是一个 JSON 对象".into());
    }
    let mut members = Vec::new();
    let mut cursor = skip_whitespace(bytes, span.0 + 1);
    while cursor < span.1 - 1 {
        let key_start = cursor;
        let key = string_literal(bytes, cursor)?;
        cursor = skip_whitespace(bytes, key.1);
        if bytes.get(cursor) != Some(&b':') {
            return Err("对象成员缺少冒号".into());
        }
        cursor = skip_whitespace(bytes, cursor + 1);
        let value = value_span(bytes, cursor)?;
        members.push(Item {
            key: Some(key.0),
            whole: (key_start, value.1),
            value,
        });
        cursor = skip_whitespace(bytes, value.1);
        if bytes.get(cursor) == Some(&b',') {
            cursor = skip_whitespace(bytes, cursor + 1);
        }
    }
    Ok(members)
}

/// Every element of the array at `span`. An element is its own whole, so the
/// two spans coincide.
fn array_elements(text: &str, span: Span) -> Result<Vec<Item>, String> {
    let bytes = text.as_bytes();
    if bytes.get(span.0) != Some(&b'[') {
        return Err("这里不是一个 JSON 数组".into());
    }
    let mut elements = Vec::new();
    let mut cursor = skip_whitespace(bytes, span.0 + 1);
    while cursor < span.1 - 1 {
        let value = value_span(bytes, cursor)?;
        elements.push(Item {
            key: None,
            whole: value,
            value,
        });
        cursor = skip_whitespace(bytes, value.1);
        if bytes.get(cursor) == Some(&b',') {
            cursor = skip_whitespace(bytes, cursor + 1);
        }
    }
    Ok(elements)
}

/// The span of the value starting at `start`, which must be its first byte.
fn value_span(bytes: &[u8], start: usize) -> Result<Span, String> {
    match bytes.get(start) {
        Some(b'"') => string_literal(bytes, start).map(|(_, end)| (start, end)),
        Some(b'{') => balanced(bytes, start, b'{', b'}'),
        Some(b'[') => balanced(bytes, start, b'[', b']'),
        Some(_) => {
            // A number, `true`, `false` or `null`: it ends where the next
            // separator or closing bracket begins.
            let mut end = start;
            while end < bytes.len()
                && !matches!(bytes[end], b',' | b'}' | b']')
                && !bytes[end].is_ascii_whitespace()
            {
                end += 1;
            }
            Ok((start, end))
        }
        None => Err("JSON 在值开始处就结束了".into()),
    }
}

/// The span of a bracketed value, counting nesting and ignoring brackets that
/// appear inside string literals.
fn balanced(bytes: &[u8], start: usize, open: u8, close: u8) -> Result<Span, String> {
    let mut depth = 0usize;
    let mut cursor = start;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => {
                cursor = string_literal(bytes, cursor)?.1;
                continue;
            }
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return Ok((start, cursor + 1));
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Err("JSON 括号没有闭合".into())
}

/// The decoded key and the index just past the closing quote. Only the escapes
/// a key can contain matter here: the value is compared against a name Mework
/// minted, and `\uXXXX` in a server name is already refused by the name rule.
fn string_literal(bytes: &[u8], start: usize) -> Result<(String, usize), String> {
    if bytes.get(start) != Some(&b'"') {
        return Err("这里不是一个 JSON 字符串".into());
    }
    let mut decoded = Vec::new();
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => {
                let key =
                    String::from_utf8(decoded).map_err(|_| "JSON 字符串不是 UTF-8".to_owned())?;
                return Ok((key, cursor + 1));
            }
            b'\\' => {
                let escaped = bytes.get(cursor + 1).ok_or("JSON 字符串在转义处结束")?;
                match escaped {
                    b'"' => decoded.push(b'"'),
                    b'\\' => decoded.push(b'\\'),
                    b'/' => decoded.push(b'/'),
                    b'n' => decoded.push(b'\n'),
                    b't' => decoded.push(b'\t'),
                    b'r' => decoded.push(b'\r'),
                    b'b' => decoded.push(0x08),
                    b'f' => decoded.push(0x0c),
                    // `\uXXXX` is skipped rather than decoded: no name this is
                    // compared against can contain one.
                    b'u' => {
                        cursor += 6;
                        continue;
                    }
                    other => decoded.push(*other),
                }
                cursor += 2;
                continue;
            }
            byte => decoded.push(byte),
        }
        cursor += 1;
    }
    Err("JSON 字符串没有闭合".into())
}

fn skip_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deleting one server leaves every other line byte-for-byte as the user
    /// wrote it — the property a parse-and-reserialize round trip loses.
    #[test]
    fn removing_a_member_keeps_the_rest_of_the_file_verbatim() {
        let text = r#"{
  "note": "mine",
  "mcpServers": {
    "zebra": { "command": "z", "args": ["--last"] },
    "docs":  {
      "type": "http",
      "url": "https://example.test/mcp"
    },
    "alpha": { "command": "a" }
  }
}
"#;

        let without_docs = remove_object_member(text, &["mcpServers"], "docs").unwrap();

        // Order is preserved and the survivors are untouched.
        assert!(without_docs.contains("\"zebra\": { \"command\": \"z\", \"args\": [\"--last\"] },"));
        assert!(without_docs.contains("\"alpha\": { \"command\": \"a\" }"));
        assert!(!without_docs.contains("docs"));
        assert!(without_docs.contains("\"note\": \"mine\""));
        let value: serde_json::Value = serde_json::from_str(&without_docs).unwrap();
        let servers = value["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2);
        assert!(value["mcpServers"]["zebra"]["args"][0] == "--last");
        // Removing the first and the last work the same way.
        let without_zebra = remove_object_member(text, &["mcpServers"], "zebra").unwrap();
        let value: serde_json::Value = serde_json::from_str(&without_zebra).unwrap();
        assert_eq!(value["mcpServers"].as_object().unwrap().len(), 2);
        let without_alpha = remove_object_member(text, &["mcpServers"], "alpha").unwrap();
        let value: serde_json::Value = serde_json::from_str(&without_alpha).unwrap();
        assert_eq!(value["mcpServers"].as_object().unwrap().len(), 2);
        assert!(value["mcpServers"]["docs"]["type"] == "http");
    }

    /// Braces, brackets and commas inside strings are text, not structure.
    #[test]
    fn punctuation_inside_strings_does_not_confuse_the_scanner() {
        let text = r#"{"mcpServers":{"a":{"command":"c:\\x\",{}[],","args":["},",""]},"b":{"command":"b"}}}"#;

        let without_a = remove_object_member(text, &["mcpServers"], "a").unwrap();

        let value: serde_json::Value = serde_json::from_str(&without_a).unwrap();
        assert_eq!(value["mcpServers"].as_object().unwrap().len(), 1);
        assert!(value["mcpServers"]["b"]["command"] == "b");
        // And the entry whose value held the punctuation survives its sibling's removal.
        let without_b = remove_object_member(text, &["mcpServers"], "b").unwrap();
        let value: serde_json::Value = serde_json::from_str(&without_b).unwrap();
        assert!(value["mcpServers"]["a"]["command"] == "c:\\x\",{}[],");
        assert!(value["mcpServers"]["a"]["args"][0] == "},");
    }

    /// Removing the only member leaves a valid empty object, not a dangling comma.
    #[test]
    fn removing_the_only_member_leaves_an_empty_object() {
        let text = "{\n  \"mcpServers\": {\n    \"solo\": { \"command\": \"s\" }\n  }\n}\n";

        let emptied = remove_object_member(text, &["mcpServers"], "solo").unwrap();

        let value: serde_json::Value = serde_json::from_str(&emptied).unwrap();
        assert!(value["mcpServers"].as_object().unwrap().is_empty());
    }

    #[test]
    fn an_unknown_member_or_path_is_an_error_rather_than_a_rewrite() {
        let text = r#"{"mcpServers":{"a":{"command":"c"}}}"#;

        assert!(remove_object_member(text, &["mcpServers"], "gone").is_err());
        assert!(remove_object_member(text, &["servers"], "a").is_err());
    }

    /// Hook handlers are array elements, addressed through a mixed path.
    #[test]
    fn removing_an_array_element_keeps_its_neighbours_verbatim() {
        let text = r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "^(bash)$",
        "hooks": [
          { "type": "command", "name": "first", "command": "one" },
          { "type": "command", "name": "second", "command": "two" }
        ]
      }
    ]
  }
}
"#;
        let path = [
            Step::Key("hooks"),
            Step::Key("PreToolUse"),
            Step::Index(0),
            Step::Key("hooks"),
        ];

        let without_first = remove_array_element(text, &path, 0).unwrap();

        assert!(
            without_first.contains(r#"{ "type": "command", "name": "second", "command": "two" }"#)
        );
        assert!(!without_first.contains("first"));
        // The group and its matcher are untouched, in their original order.
        assert!(without_first.contains(r#""matcher": "^(bash)$","#));
        let value: serde_json::Value = serde_json::from_str(&without_first).unwrap();
        assert_eq!(
            value["hooks"]["PreToolUse"][0]["hooks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let without_second = remove_array_element(text, &path, 1).unwrap();
        let value: serde_json::Value = serde_json::from_str(&without_second).unwrap();
        assert_eq!(value["hooks"]["PreToolUse"][0]["hooks"][0]["name"], "first");
        // An emptied group stays a group, as the hook address contract requires.
        let emptied = remove_array_element(&without_second, &path, 0).unwrap();
        let value: serde_json::Value = serde_json::from_str(&emptied).unwrap();
        assert!(value["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(remove_array_element(text, &path, 9).is_err());
    }
}
