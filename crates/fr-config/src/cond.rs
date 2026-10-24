//! Restricted declarative condition subset for patch expressions.
//!
//! Replaces dsh's `!!js` tag per PRD 5 / CONFIG-4 (2026-10-22 Q3A ruling):
//! env chains with `??` defaults, equality comparisons, platform ternaries,
//! bare env reads, literals, and the profile-context boolean. Arbitrary JS
//! is never evaluated.

use serde_json::Value;

/// Boxed environment lookup shared by all evaluation contexts.
pub type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Evaluation environment for one patch parse.
///
/// `env` resolves an environment variable name; `has_profile_context` is the
/// fr equivalent of dsh's `ctx.get('profileContext')` (an entry/profile
/// context exists); `platform` is one of `linux` / `macos` / `windows`.
pub struct EvalContext {
    /// Environment lookup (name to value).
    pub env: EnvLookup,
    /// Whether an entry/profile context exists.
    pub has_profile_context: bool,
    /// Platform identifier used by `platform` operands.
    pub platform: &'static str,
}

impl std::fmt::Debug for EvalContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalContext")
            .field("has_profile_context", &self.has_profile_context)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl EvalContext {
    /// A context over the live process environment and host platform.
    #[must_use]
    pub fn process(has_profile_context: bool) -> Self {
        Self {
            env: Box::new(process_env),
            has_profile_context,
            platform: host_platform(),
        }
    }

    /// A neutral context for tests and offline tooling: empty environment,
    /// profile context present, linux platform.
    #[must_use]
    pub fn neutral() -> Self {
        Self {
            env: Box::new(|_name| None),
            has_profile_context: true,
            platform: "linux",
        }
    }
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Maps the host OS to the subset's platform identifier.
#[must_use]
pub const fn host_platform() -> &'static str {
    std::env::consts::OS
}

/// Evaluates one expression of the restricted subset.
///
/// # Errors
///
/// Returns a human-readable reason for expressions outside the subset
/// (unsupported syntax, malformed literals, unbalanced parentheses); the
/// caller rejects the whole patch document (fail-closed).
pub fn eval(text: &str, ctx: &EvalContext) -> Result<Value, String> {
    let mut parser = Parser::new(text, ctx);
    let value = parser.parse_ternary()?;
    parser.finish()?;
    Ok(value)
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    ctx: &'a EvalContext,
}

impl<'a> Parser<'a> {
    fn new(text: &str, ctx: &'a EvalContext) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
            ctx,
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        self.skip_ws();
        if self.pos < self.chars.len() {
            Err(format!(
                "unexpected trailing input at column {}: {}",
                self.pos,
                self.rest_head()
            ))
        } else {
            Ok(())
        }
    }

    fn rest_head(&self) -> String {
        self.chars[self.pos..(self.pos + 24).min(self.chars.len())]
            .iter()
            .collect()
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        self.skip_ws();
        let token_chars: Vec<char> = token.chars().collect();
        if self.chars[self.pos..].starts_with(token_chars.as_slice()) {
            self.pos += token_chars.len();
            true
        } else {
            false
        }
    }

    fn lookahead_is(&mut self, token: &str) -> bool {
        self.skip_ws();
        let token_chars: Vec<char> = token.chars().collect();
        self.chars[self.pos..].starts_with(token_chars.as_slice())
    }

    fn parse_ternary(&mut self) -> Result<Value, String> {
        let cond = self.parse_equality()?;
        // A single `?` opens a ternary; `??` is the nullish-coalescing
        // default operator and must not be consumed here.
        if self.lookahead_is("?") && !self.lookahead_is("??") {
            self.eat("?");
            let when_true = self.parse_ternary()?;
            if !self.eat(":") {
                return Err("ternary requires ':' before the false branch".to_owned());
            }
            let when_false = self.parse_ternary()?;
            return match cond {
                Value::Bool(flag) => Ok(if flag { when_true } else { when_false }),
                other => Err(format!("ternary condition must be boolean, got {other}")),
            };
        }
        Ok(cond)
    }

    fn parse_equality(&mut self) -> Result<Value, String> {
        let lhs = self.parse_operand()?;
        self.skip_ws();
        if self.eat("===") {
            let rhs = self.parse_operand()?;
            return Ok(Value::Bool(strict_eq(&lhs, &rhs)));
        }
        if self.eat("==") {
            let rhs = self.parse_operand()?;
            return Ok(Value::Bool(strict_eq(&lhs, &rhs)));
        }
        Ok(lhs)
    }

    fn parse_operand(&mut self) -> Result<Value, String> {
        if self.eat("(") {
            let inner = self.parse_ternary()?;
            if !self.eat(")") {
                return Err("unbalanced '('".to_owned());
            }
            return Ok(inner);
        }
        if self.eat("!profile-context") {
            return Ok(Value::Bool(!self.ctx.has_profile_context));
        }
        if self.eat("profile-context") {
            return Ok(Value::Bool(self.ctx.has_profile_context));
        }
        if self.eat("platform") {
            return Ok(Value::String(self.ctx.platform.to_owned()));
        }
        if self.eat("env.") {
            return self.parse_env_chain();
        }
        self.parse_literal()
    }

    fn parse_env_chain(&mut self) -> Result<Value, String> {
        let name = self.parse_ident("env variable name")?;
        let mut value = (self.ctx.env)(&name).map_or(Value::Null, Value::String);
        // The `?? <default>` clause is part of the expression grammar: it is
        // consumed whether or not the lookup succeeded, and chains are allowed
        // (left-associative, coalescing only null, matching JS semantics).
        while self.lookahead_is("??") {
            self.eat("??");
            let default = self.parse_literal()?;
            if value.is_null() {
                value = default;
            }
        }
        Ok(value)
    }

    fn parse_ident(&mut self, what: &str) -> Result<String, String> {
        self.skip_ws();
        let mut ident = String::new();
        while let Some(&ch) = self.chars.get(self.pos) {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ident.push(ch);
                self.pos += 1;
            } else {
                break;
            }
        }
        if ident.is_empty()
            || !ident
                .chars()
                .next()
                .is_some_and(|ch: char| ch.is_ascii_alphabetic())
        {
            return Err(format!("expected {what} at column {}", self.pos));
        }
        Ok(ident)
    }

    fn parse_literal(&mut self) -> Result<Value, String> {
        self.skip_ws();
        if self.pos >= self.chars.len() {
            return Err("unexpected end of expression".to_owned());
        }
        let first = self.chars[self.pos];
        // String literals accept single or double quotes; the subset defines
        // no escape sequences, so the content is taken verbatim.
        if matches!(first, '"' | '\'') {
            self.pos += 1;
            let mut content = String::new();
            loop {
                match self.chars.get(self.pos) {
                    None => {
                        return Err(format!("unterminated string literal starting with {first}"));
                    }
                    Some(&ch) => {
                        self.pos += 1;
                        if ch == first {
                            break;
                        }
                        content.push(ch);
                    }
                }
            }
            return Ok(Value::String(content));
        }
        // Bare token: extends until a subset boundary (whitespace, operator,
        // or punctuation).
        let mut token = String::new();
        while let Some(&ch) = self.chars.get(self.pos) {
            if ch.is_whitespace() || matches!(ch, '?' | ':' | '(' | ')' | '=') {
                break;
            }
            token.push(ch);
            self.pos += 1;
        }
        serde_json::from_str::<Value>(&token)
            .map_err(|e| format!("unsupported or malformed literal token `{token}`: {e}"))
    }
}

fn strict_eq(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        // JS `===` compares Numbers by f64 value: `1 === 1.0` is true, and
        // precision loss above 2**53 is part of the semantics (dsh `!!js`
        // expressions run under real JS). serde_json Numbers are finite,
        // so the f64 projection has no NaN/inf case.
        (Value::Number(lhs), Value::Number(rhs)) => lhs.as_f64() == rhs.as_f64(),
        _ => lhs == rhs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(env: Option<Vec<(&'static str, &'static str)>>) -> EvalContext {
        let map: std::collections::HashMap<String, String> = env
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        EvalContext {
            env: Box::new(move |name: &str| map.get(name).cloned()),
            has_profile_context: true,
            platform: "linux",
        }
    }

    #[test]
    fn strict_eq_compares_numbers_by_js_value() {
        let ctx = ctx(None);
        for text in [
            "60000 === 60000.0",
            "1 === 1",
            // Above 2**53 JS Numbers lose precision: both literals project
            // to the same f64, so JS (and this subset) compares them equal.
            "9007199254740993 === 9007199254740992",
        ] {
            assert_eq!(
                eval(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e}")),
                Value::Bool(true),
                "{text}"
            );
        }
        for text in ["1 === 1.5", "1 === \"1\"", "1 === true"] {
            assert_eq!(
                eval(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e}")),
                Value::Bool(false),
                "{text}"
            );
        }
    }

    #[test]
    fn env_default_chain_matches_dsh_semantics() {
        // dsh: (process.env.DSH_PERMISSION_MODE ?? 'workspace-write') === 'danger-full-access'
        let set = ctx(Some(vec![("FR_PERMISSION_MODE", "danger-full-access")]));
        let absent = ctx(None);
        let other = ctx(Some(vec![("FR_PERMISSION_MODE", "bogus")]));
        let expr = "(env.FR_PERMISSION_MODE ?? 'workspace-write') === 'danger-full-access'";
        assert_eq!(
            eval(expr, &set).unwrap_or_else(|e| panic!("{e}")),
            json!(true)
        );
        assert_eq!(
            eval(expr, &absent).unwrap_or_else(|e| panic!("{e}")),
            json!(false)
        );
        assert_eq!(
            eval(expr, &other).unwrap_or_else(|e| panic!("{e}")),
            json!(false)
        );
    }

    #[test]
    fn ternary_selects_by_condition() {
        let set = ctx(Some(vec![("FR_PERMISSION_MODE", "danger-full-access")]));
        let expr = "(env.FR_PERMISSION_MODE ?? 'workspace-write') === 'danger-full-access' ? 'never' : 'ask'";
        assert_eq!(
            eval(expr, &set).unwrap_or_else(|e| panic!("{e}")),
            json!("never")
        );
        assert_eq!(
            eval(expr, &ctx(None)).unwrap_or_else(|e| panic!("{e}")),
            json!("ask")
        );
    }

    #[test]
    fn env_default_chain_supports_multiple_defaults() {
        // env.X absent: falls through both defaults; env.Y set: value kept.
        let absent = ctx(None);
        assert_eq!(
            eval("env.FR_X ?? 'a' ?? 'b'", &absent).unwrap_or_else(|e| panic!("{e}")),
            json!("a")
        );
        let set = ctx(Some(vec![("FR_Y", "")]));
        assert_eq!(
            eval("env.FR_Y ?? 'a' ?? 'b'", &set).unwrap_or_else(|e| panic!("{e}")),
            json!(""),
            "empty string is not nullish: default must not apply"
        );
    }

    #[test]
    fn bare_env_read_is_null_when_absent() {
        assert_eq!(
            eval("env.FR_TOOLS_MODE", &ctx(None)).unwrap_or_else(|e| panic!("{e}")),
            Value::Null
        );
        assert_eq!(
            eval(
                "env.FR_TOOLS_MODE",
                &ctx(Some(vec![("FR_TOOLS_MODE", "ptc")]))
            )
            .unwrap_or_else(|e| panic!("{e}")),
            json!("ptc")
        );
    }

    #[test]
    fn platform_ternary_and_equality() {
        let linux = EvalContext {
            env: Box::new(|_n| None),
            has_profile_context: true,
            platform: "linux",
        };
        let macos = EvalContext {
            env: Box::new(|_n| None),
            has_profile_context: true,
            platform: "macos",
        };
        let expr = "platform === 'linux' ? '/usr/bin' : null";
        assert_eq!(
            eval(expr, &linux).unwrap_or_else(|e| panic!("{e}")),
            json!("/usr/bin")
        );
        assert_eq!(
            eval(expr, &macos).unwrap_or_else(|e| panic!("{e}")),
            Value::Null
        );
    }

    #[test]
    fn profile_context_boolean_with_negation() {
        let yes = EvalContext {
            env: Box::new(|_n| None),
            has_profile_context: true,
            platform: "linux",
        };
        let no = EvalContext {
            env: Box::new(|_n| None),
            has_profile_context: false,
            platform: "linux",
        };
        assert_eq!(
            eval("profile-context", &yes).unwrap_or_else(|e| panic!("{e}")),
            json!(true)
        );
        assert_eq!(
            eval("!profile-context", &yes).unwrap_or_else(|e| panic!("{e}")),
            json!(false)
        );
        assert_eq!(
            eval("!profile-context", &no).unwrap_or_else(|e| panic!("{e}")),
            json!(true)
        );
    }

    #[test]
    fn literals_and_equality() {
        let neutral = ctx(None);
        assert_eq!(
            eval("null", &neutral).unwrap_or_else(|e| panic!("{e}")),
            Value::Null
        );
        assert_eq!(
            eval("true", &neutral).unwrap_or_else(|e| panic!("{e}")),
            json!(true)
        );
        assert_eq!(
            eval("'a' == \"a\"", &neutral).unwrap_or_else(|e| panic!("{e}")),
            json!(true)
        );
    }

    #[test]
    fn subset_rejects_arbitrary_js() {
        let neutral = ctx(None);
        let expressions = [
            "process.getBuiltinModule('node:path').resolve('')",
            "!ctx.get('profileContext')",
            "JSON.parse(env.X)",
            "['darwin'].includes('linux')",
            "env.X ?? {a: 1}",
        ];
        for expr in expressions {
            eval(expr, &neutral)
                .err()
                .unwrap_or_else(|| panic!("expression must be rejected: {expr}"));
        }
    }

    #[test]
    fn malformed_expressions_are_rejected() {
        let neutral = ctx(None);
        let _ = eval("(env.X ?? 'a'", &neutral).expect_err("unclosed paren");
        let _ = eval("env.X === ", &neutral).expect_err("dangling equality");
        let _ = eval("true ? 'a'", &neutral).expect_err("ternary missing colon");
        let _ = eval("1 ? 'a' : 'b' ?? ", &neutral).expect_err("dangling default");
    }
}
