//! Tiny shell-quoting helper shared between the watch popup command
//! and the dashboard's spawn-agent dispatch. Both pass user-controlled
//! or tmux-controlled strings (session names, agent prompts) to `sh -c`
//! via `tmux display-popup` / `tmux new-window`, and both need to be
//! defended against shell metacharacters and whitespace.

/// POSIX-safe single-quote wrapping: `it's a "test"` →
/// `'it'\''s a "test"'`. Single quotes in sh are literal — no `$VAR`
/// or `` `cmd` `` expansion, no backslash escapes — so the result is
/// guaranteed to round-trip unchanged through `sh -c`.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_in_single_quotes() {
        assert_eq!(quote("hello"), "'hello'");
        assert_eq!(quote("with spaces"), "'with spaces'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn escapes_embedded_single_quotes() {
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote("'"), "''\\'''");
    }

    #[test]
    fn passes_dollar_and_backtick_through_unchanged() {
        assert_eq!(quote("$PATH"), "'$PATH'");
        assert_eq!(quote("`whoami`"), "'`whoami`'");
        assert_eq!(quote("a\\nb"), "'a\\nb'");
    }
}
