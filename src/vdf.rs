//! Minimal Valve KeyValues ("VDF") parser — the text format Steam stores
//! controller configurations in.
//!
//! Supported: nested `"key" { … }` blocks, `"key" "value"` pairs, quoted and
//! bare tokens, `//` comments, `\"`/`\\`/`\n`/`\t` escapes in quoted strings,
//! and `[$CONDITION]` platform tags (skipped). Duplicate keys are preserved
//! ([`Block::get_all`]), as Steam's own files rely on repeated keys like
//! `"group"`. Not supported (not needed for our configs): `#base` includes.

use anyhow::bail;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Block(Block),
}

/// An ordered list of key/value pairs; duplicate keys allowed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block(pub Vec<(String, Value)>);

impl Block {
    /// First value with this key (Valve treats keys case-insensitively).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn get_block(&self, key: &str) -> Option<&Block> {
        match self.get(key) {
            Some(Value::Block(b)) => Some(b),
            _ => None,
        }
    }

    /// All values with this key, in file order.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Value> {
        self.0
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }
}

/// Parse a whole document; the top level is an implicit block.
pub fn parse(text: &str) -> anyhow::Result<Block> {
    let mut lexer = Lexer {
        chars: text.chars().peekable(),
        line: 1,
    };
    parse_block(&mut lexer, 0)
}

enum Token {
    Str(String),
    Open,
    Close,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
}

impl Lexer<'_> {
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next();
        if c == Some('\n') {
            self.line += 1;
        }
        c
    }

    fn next_token(&mut self) -> anyhow::Result<Option<Token>> {
        loop {
            let Some(c) = self.bump() else {
                return Ok(None);
            };
            match c {
                c if c.is_whitespace() => {}
                '/' if self.chars.peek() == Some(&'/') => {
                    while let Some(c) = self.bump() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                // Platform conditional like [$WIN32]: skip.
                '[' => {
                    while let Some(c) = self.bump() {
                        if c == ']' {
                            break;
                        }
                    }
                }
                '{' => return Ok(Some(Token::Open)),
                '}' => return Ok(Some(Token::Close)),
                '"' => {
                    let start = self.line;
                    let mut s = String::new();
                    loop {
                        match self.bump() {
                            None => bail!("line {start}: unterminated string"),
                            Some('"') => break,
                            Some('\\') => match self.bump() {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some(c @ ('\\' | '"')) => s.push(c),
                                other => {
                                    bail!("line {}: bad escape {other:?}", self.line)
                                }
                            },
                            Some(c) => s.push(c),
                        }
                    }
                    return Ok(Some(Token::Str(s)));
                }
                c => {
                    let mut s = String::from(c);
                    while let Some(&next) = self.chars.peek() {
                        if next.is_whitespace() || "{}\"[".contains(next) {
                            break;
                        }
                        s.push(self.bump().unwrap());
                    }
                    return Ok(Some(Token::Str(s)));
                }
            }
        }
    }
}

/// Recursion guard: no sane config nests this deep, and unbounded depth
/// would let a pathological file overflow the stack.
const MAX_DEPTH: usize = 64;

fn parse_block(lexer: &mut Lexer, depth: usize) -> anyhow::Result<Block> {
    if depth > MAX_DEPTH {
        bail!("line {}: more than {MAX_DEPTH} nested blocks", lexer.line);
    }
    let mut items = Vec::new();
    loop {
        let key = match lexer.next_token()? {
            None if depth == 0 => return Ok(Block(items)),
            None => bail!("unexpected end of file: {depth} unclosed {{"),
            Some(Token::Close) if depth > 0 => return Ok(Block(items)),
            Some(Token::Close) => bail!("line {}: unmatched }}", lexer.line),
            Some(Token::Open) => bail!("line {}: {{ without a key", lexer.line),
            Some(Token::Str(key)) => key,
        };
        let value = match lexer.next_token()? {
            Some(Token::Str(value)) => Value::String(value),
            Some(Token::Open) => Value::Block(parse_block(lexer, depth + 1)?),
            Some(Token::Close) | None => {
                bail!("line {}: key {key:?} has no value", lexer.line)
            }
        };
        items.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_blocks_and_comments() {
        let doc = parse(concat!(
            "// header comment\n",
            "\"root\"\n",
            "{\n",
            "\t\"name\"\t\"value\" // trailing comment\n",
            "\t\"nested\" { \"a\" \"1\" }\n",
            "}\n",
        ))
        .unwrap();
        let root = doc.get_block("root").unwrap();
        assert_eq!(root.get_str("name"), Some("value"));
        assert_eq!(root.get_block("nested").unwrap().get_str("a"), Some("1"));
    }

    #[test]
    fn keys_are_case_insensitive_and_bare_tokens_work() {
        let doc = parse("Key value").unwrap();
        assert_eq!(doc.get_str("key"), Some("value"));
        assert_eq!(doc.get_str("KEY"), Some("value"));
    }

    #[test]
    fn duplicate_keys_are_preserved_in_order() {
        let doc = parse(r#""group" { "id" "0" } "group" { "id" "1" }"#).unwrap();
        let ids: Vec<_> = doc
            .get_all("group")
            .filter_map(|v| match v {
                Value::Block(b) => b.get_str("id"),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["0", "1"]);
        assert_eq!(doc.get_all("missing").count(), 0);
    }

    #[test]
    fn escapes_and_conditionals() {
        let doc = parse(r#""key" "a\"b\\c\nd" [$WIN32]"#).unwrap();
        assert_eq!(doc.get_str("key"), Some("a\"b\\c\nd"));
    }

    #[test]
    fn errors_carry_line_numbers() {
        let err = parse("\"a\"\n{\n\"b\" \"1\"\n").unwrap_err().to_string();
        assert!(err.contains("unclosed"), "{err}");

        let err = parse("\"a\" \"1\"\n}").unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");

        let err = parse("\"lonely\"").unwrap_err().to_string();
        assert!(err.contains("no value"), "{err}");

        let err = parse("\"s\" \"unterminated").unwrap_err().to_string();
        assert!(err.contains("unterminated"), "{err}");
    }

    #[test]
    fn empty_input_is_an_empty_block() {
        assert_eq!(parse("  // nothing\n").unwrap(), Block::default());
    }

    #[test]
    fn pathological_nesting_is_rejected_not_a_stack_overflow() {
        let doc = "\"k\" {".repeat(1000);
        let err = parse(&doc).unwrap_err().to_string();
        assert!(err.contains("nested"), "{err}");
    }
}
