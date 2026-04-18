use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    Param(String),
}

#[derive(Debug, Clone)]
pub struct CompiledPattern {
    source: String,
    segments: Vec<Segment>,
    param_names: Vec<String>,
    has_wildcard: bool,
}

impl CompiledPattern {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn param_names(&self) -> &[String] {
        &self.param_names
    }

    pub fn has_wildcard(&self) -> bool {
        self.has_wildcard
    }

    pub fn match_channel(&self, channel: &str) -> Option<HashMap<String, String>> {
        let parts: Vec<&str> = channel.split('/').collect();
        if self.has_wildcard {
            if parts.len() < self.segments.len() + 1 {
                return None;
            }
        } else if parts.len() != self.segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (i, seg) in self.segments.iter().enumerate() {
            let part = parts[i];
            if part.is_empty() {
                return None;
            }
            match seg {
                Segment::Literal(v) if v == part => continue,
                Segment::Literal(_) => return None,
                Segment::Param(name) => {
                    params.insert(name.clone(), part.to_string());
                }
            }
        }
        Some(params)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    #[error("pattern must not be empty")]
    Empty,
    #[error("pattern must not start or end with '/'")]
    BadSlash,
    #[error("pattern contains an empty segment")]
    EmptySegment,
    #[error("wildcard must be the final segment")]
    MidWildcard,
    #[error("invalid param name ':{0}'")]
    BadParamName(String),
    #[error("duplicate param name ':{0}'")]
    DuplicateParam(String),
}

pub fn compile(pattern: &str) -> Result<CompiledPattern, PatternError> {
    if pattern.is_empty() {
        return Err(PatternError::Empty);
    }
    if pattern.starts_with('/') || pattern.ends_with('/') {
        return Err(PatternError::BadSlash);
    }
    let raw: Vec<&str> = pattern.split('/').collect();
    let mut segments = Vec::new();
    let mut param_names = Vec::new();
    let mut has_wildcard = false;
    for (i, seg) in raw.iter().enumerate() {
        if seg.is_empty() {
            return Err(PatternError::EmptySegment);
        }
        if *seg == "*" {
            if i != raw.len() - 1 {
                return Err(PatternError::MidWildcard);
            }
            has_wildcard = true;
            continue;
        }
        if let Some(name) = seg.strip_prefix(':') {
            if !is_valid_ident(name) {
                return Err(PatternError::BadParamName(name.to_string()));
            }
            if param_names.iter().any(|n| n == name) {
                return Err(PatternError::DuplicateParam(name.to_string()));
            }
            param_names.push(name.to_string());
            segments.push(Segment::Param(name.to_string()));
        } else {
            segments.push(Segment::Literal((*seg).to_string()));
        }
    }
    Ok(CompiledPattern {
        source: pattern.to_string(),
        segments,
        param_names,
        has_wildcard,
    })
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn specificity_cmp(a: &CompiledPattern, b: &CompiledPattern) -> Ordering {
    let len = a.segments.len().max(b.segments.len());
    for i in 0..len {
        let ra = rank(a.segments.get(i));
        let rb = rank(b.segments.get(i));
        if ra != rb {
            return ra.cmp(&rb);
        }
    }
    match (a.has_wildcard, b.has_wildcard) {
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        _ => b.segments.len().cmp(&a.segments.len()),
    }
}

fn rank(seg: Option<&Segment>) -> u8 {
    match seg {
        Some(Segment::Literal(_)) => 0,
        Some(Segment::Param(_)) => 1,
        None => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_rejects_empty() {
        assert!(compile("").is_err());
    }

    #[test]
    fn compile_rejects_leading_slash() {
        assert!(compile("/chat").is_err());
    }

    #[test]
    fn compile_collects_params_in_order() {
        let p = compile("chat/:roomId/users/:userId").unwrap();
        assert_eq!(p.param_names(), &["roomId", "userId"]);
    }

    #[test]
    fn compile_rejects_duplicate_params() {
        assert!(compile("chat/:id/reply/:id").is_err());
    }

    #[test]
    fn match_literal() {
        let p = compile("status").unwrap();
        assert!(p.match_channel("status").is_some());
        assert!(p.match_channel("status/x").is_none());
    }

    #[test]
    fn match_param_capture() {
        let p = compile("chat/:roomId").unwrap();
        let m = p.match_channel("chat/abc").unwrap();
        assert_eq!(m.get("roomId"), Some(&"abc".to_string()));
    }

    #[test]
    fn match_tail_wildcard() {
        let p = compile("chat/:roomId/*").unwrap();
        let m = p.match_channel("chat/r1/a/b").unwrap();
        assert_eq!(m.get("roomId"), Some(&"r1".to_string()));
    }

    #[test]
    fn compare_literal_beats_param() {
        let a = compile("chat/lobby").unwrap();
        let b = compile("chat/:id").unwrap();
        assert!(specificity_cmp(&a, &b).is_lt());
    }
}
