use super::super::*;

// ==================== Error Handling Tests ====================

#[test]
fn test_invalid_toml_syntax() {
    let toml = r#"
[tako
name = "broken"
"#;
    assert!(TakoToml::parse(toml).is_err());
}

#[test]
fn test_wrong_type() {
    let toml = r#"
name = 123
"#;
    assert!(TakoToml::parse(toml).is_err());
}
