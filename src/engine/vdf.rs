//! Small, dependency-free Valve Data Format (VDF) reader.
//!
//! Steam uses VDF for `libraryfolders.vdf` and every `appmanifest_*.acf`.
//! The parser handles quoted strings, nested tables, `//` comments and the
//! backslash-escaped Windows paths found in those files.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Vdf {
    Str(String),
    Table(BTreeMap<String, Vdf>),
}

impl Vdf {
    /// Parse a complete VDF document. The root is always a table.
    pub fn parse(text: &str) -> Result<Vdf, String> {
        let mut tokenizer = Tokenizer::new(text);
        let table = parse_pairs(&mut tokenizer, false)?;
        Ok(Vdf::Table(table))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(value) => Some(value),
            Vdf::Table(_) => None,
        }
    }

    pub fn as_table(&self) -> Option<&BTreeMap<String, Vdf>> {
        match self {
            Vdf::Table(table) => Some(table),
            Vdf::Str(_) => None,
        }
    }

    /// Case-insensitive lookup, like Valve's own parser.
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        self.as_table()?
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }
}

fn parse_pairs(tokenizer: &mut Tokenizer<'_>, nested: bool) -> Result<BTreeMap<String, Vdf>, String> {
    let mut table = BTreeMap::new();
    loop {
        match tokenizer.next_token()? {
            None => {
                if nested {
                    return Err("unexpected end of file: missing '}'".to_string());
                }
                return Ok(table);
            }
            Some(Token::CloseBrace) => {
                if nested {
                    return Ok(table);
                }
                return Err("unexpected '}' at top level".to_string());
            }
            Some(Token::OpenBrace) => return Err("unexpected '{' where a key was expected".to_string()),
            Some(Token::Str(key)) => {
                let value = match tokenizer.next_token()? {
                    Some(Token::Str(value)) => Vdf::Str(value),
                    Some(Token::OpenBrace) => Vdf::Table(parse_pairs(tokenizer, true)?),
                    Some(Token::CloseBrace) => {
                        return Err(format!("missing value for key '{key}'"))
                    }
                    None => return Err(format!("missing value for key '{key}'")),
                };
                table.insert(key, value);
            }
        }
    }
}

enum Token {
    Str(String),
    OpenBrace,
    CloseBrace,
}

struct Tokenizer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Tokenizer<'a> {
    fn new(text: &'a str) -> Self {
        // Steam sometimes writes a UTF-8 BOM.
        Self {
            chars: text.trim_start_matches('\u{feff}').chars().peekable(),
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, String> {
        loop {
            while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
                self.chars.next();
            }

            // `//` line comments. A lone '/' is left for the unquoted reader.
            if self.chars.peek() == Some(&'/') {
                let mut clone = self.chars.clone();
                clone.next();
                if clone.peek() == Some(&'/') {
                    while let Some(c) = self.chars.next() {
                        if c == '\n' {
                            break;
                        }
                    }
                    continue;
                }
            }

            return match self.chars.peek().copied() {
                None => Ok(None),
                Some('{') => {
                    self.chars.next();
                    Ok(Some(Token::OpenBrace))
                }
                Some('}') => {
                    self.chars.next();
                    Ok(Some(Token::CloseBrace))
                }
                Some('"') => {
                    self.chars.next();
                    Ok(Some(Token::Str(self.read_quoted()?)))
                }
                Some(_) => {
                    let word = self.read_bare();
                    if word.is_empty() {
                        // Defensive: never spin on an unexpected character.
                        self.chars.next();
                        continue;
                    }
                    Ok(Some(Token::Str(word)))
                }
            };
        }
    }

    fn read_quoted(&mut self) -> Result<String, String> {
        let mut value = String::new();
        loop {
            match self.chars.next() {
                None => return Err("unterminated quoted string".to_string()),
                Some('"') => return Ok(value),
                Some('\\') => match self.chars.next() {
                    Some('\\') => value.push('\\'),
                    Some('"') => value.push('"'),
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some(other) => {
                        value.push('\\');
                        value.push(other);
                    }
                    None => return Err("unterminated escape sequence".to_string()),
                },
                Some(c) => value.push(c),
            }
        }
    }

    fn read_bare(&mut self) -> String {
        let mut value = String::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                break;
            }
            value.push(c);
            self.chars.next();
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_folders() {
        let text = r#"
            "libraryfolders"
            {
                "0"
                {
                    "path"      "C:\\Program Files (x86)\\Steam"
                    "apps" { "440" "0" }
                }
                "1"
                {
                    "path"      "D:\\SteamLibrary"
                    "apps"
                    {
                        "730" "74265067316"
                    }
                }
            }
        "#;
        let vdf = Vdf::parse(text).expect("valid vdf");
        let folders = vdf.get("libraryfolders").expect("root table");
        let first = folders.get("0").expect("library 0");
        assert_eq!(
            first.get_str("path"),
            Some(r"C:\Program Files (x86)\Steam")
        );
        assert_eq!(folders.get("1").unwrap().get_str("path"), Some(r"D:\SteamLibrary"));
    }

    #[test]
    fn parses_app_manifest() {
        let text = r#"
            "AppState"
            {
                "appid"     "440"
                "name"      "Team Fortress 2"
                "StateFlags" "4"
                "installdir" "Team Fortress 2"
                "SizeOnDisk" "33292355826"
                "LastPlayed" "1794696648"
                "InstalledDepots"
                {
                    "440"
                    {
                        "manifest" "1118032470228587934"
                    }
                }
            }
        "#;
        let vdf = Vdf::parse(text).expect("valid vdf");
        let state = vdf.get("AppState").expect("AppState");
        assert_eq!(state.get_str("appid"), Some("440"));
        assert_eq!(state.get_str("name"), Some("Team Fortress 2"));
        assert_eq!(
            state.get_str("SizeOnDisk"),
            Some("33292355826")
        );
        assert!(state.get("InstalledDepots").is_some());
    }

    #[test]
    fn handles_comments_and_bare_tokens() {
        let text = r#"
            // leading comment
            "key" 123 // trailing comment
            "nested" { "a" "b" }
        "#;
        let vdf = Vdf::parse(text).expect("valid vdf");
        assert_eq!(vdf.get_str("key"), Some("123"));
        assert_eq!(vdf.get("NESTED").unwrap().get_str("A"), Some("b"));
    }

    #[test]
    fn rejects_unbalanced_braces() {
        assert!(Vdf::parse(r#""a" { "b" "c""#).is_err());
        assert!(Vdf::parse(r#""a""#).is_err());
    }
}
