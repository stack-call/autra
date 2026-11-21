//! Minimal `.env` loading for the CLI.
//!
//! The provider stays configured through environment variables (ADR-0013);
//! this module only supplies values the process environment does not define.

use std::collections::HashMap;
use std::path::PathBuf;

/// Explicit path to a `.env` file. When set, the file must be readable.
pub const ENV_FILE_OVERRIDE: &str = "AUTRA_ENV_FILE";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DotEnv {
    values: HashMap<String, String>,
}

impl DotEnv {
    /// Parse `KEY=VALUE` lines. Blank lines, `#` comments, an optional
    /// `export ` prefix and single/double quoted values are accepted.
    /// Malformed lines are ignored.
    pub fn parse(text: &str) -> Self {
        let mut values = HashMap::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !is_valid_key(key) {
                continue;
            }
            values.insert(key.to_owned(), unquote(value.trim()));
        }
        Self { values }
    }

    /// Load the first available `.env`:
    /// 1. `AUTRA_ENV_FILE`, which must be readable;
    /// 2. `.env` in the current directory;
    /// 3. `$USERPROFILE/.autra/.env` (or `$HOME/.autra/.env`).
    ///
    /// No file yields an empty set.
    pub fn load() -> Result<Self, String> {
        let Some(path) = resolve_path() else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read env file {}: {error}", path.display()))?;
        Ok(Self::parse(&text))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

/// Prefer the process environment, then the file, ignoring empty values.
pub fn pick(env: Option<String>, file: Option<&str>) -> Option<String> {
    env.filter(|value| !value.is_empty())
        .or_else(|| file.map(str::to_owned))
}

fn resolve_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(ENV_FILE_OVERRIDE) {
        return Some(PathBuf::from(path));
    }
    let local = PathBuf::from(".env");
    if local.is_file() {
        return Some(local);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let user = PathBuf::from(home).join(".autra").join(".env");
    user.is_file().then_some(user)
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        unescape(&value[1..value.len() - 1])
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys_comments_and_quotes() {
        let env = DotEnv::parse(
            r#"
# comment
OPENAI_API_KEY=sk-test
export OPENAI_BASE_URL="https://api.example.test/v1"
QUOTED='literal \n value'
IGNORED LINE
EMPTY=
"#,
        );

        assert_eq!(env.get("OPENAI_API_KEY"), Some("sk-test"));
        assert_eq!(
            env.get("OPENAI_BASE_URL"),
            Some("https://api.example.test/v1")
        );
        assert_eq!(env.get("QUOTED"), Some(r"literal \n value"));
        assert_eq!(env.get("EMPTY"), Some(""));
    }

    #[test]
    fn unescapes_double_quoted_values() {
        let env = DotEnv::parse("KEY=\"line\\nnext\\\\end\"");

        assert_eq!(env.get("KEY"), Some("line\nnext\\end"));
    }

    #[test]
    fn environment_wins_over_file() {
        assert_eq!(
            pick(Some("from-env".to_owned()), Some("from-file")),
            Some("from-env".to_owned())
        );
        assert_eq!(pick(None, Some("from-file")), Some("from-file".to_owned()));
        assert_eq!(
            pick(Some(String::new()), Some("from-file")),
            Some("from-file".to_owned())
        );
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn empty_text_yields_empty_config() {
        assert_eq!(DotEnv::parse("").get("OPENAI_API_KEY"), None);
    }
}
