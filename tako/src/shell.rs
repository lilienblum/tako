/// POSIX-safe single-quoting: wraps value in single quotes, escaping any
/// embedded single quotes with the `'\''` idiom.
pub fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_string() {
        assert_eq!(shell_single_quote("hello"), "'hello'");
    }

    #[test]
    fn escapes_single_quotes() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn empty_string() {
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn preserves_spaces_and_special_chars() {
        assert_eq!(shell_single_quote("a b$c"), "'a b$c'");
    }
}
