//! Numbers in a model are either literals or expressions over named project parameters:
//! `12`, `"wall * 2 + 0.4"`, `"max(d, 5) / 2"`. Trig functions take degrees, like OpenSCAD.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ModelError;

pub type Params = BTreeMap<String, f64>;

/// A number or an expression string over project parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Num {
    Value(f64),
    /// Expression over project parameters, e.g. "wall*2 + 0.4". Functions: min max abs sqrt
    /// sin cos tan (degrees) floor ceil round; constant pi.
    Expr(String),
}

impl Default for Num {
    fn default() -> Self {
        Num::Value(0.0)
    }
}

impl From<f64> for Num {
    fn from(v: f64) -> Self {
        Num::Value(v)
    }
}

impl Num {
    pub fn eval(&self, p: &Params) -> Result<f64, ModelError> {
        match self {
            Num::Value(v) => Ok(*v),
            Num::Expr(s) => eval(s, p),
        }
    }
}

pub fn eval(src: &str, params: &Params) -> Result<f64, ModelError> {
    let toks = lex(src).map_err(|m| ModelError::Expr { expr: src.into(), message: m })?;
    let mut p = Parser { t: &toks, i: 0, params };
    let v = p.expr().map_err(|m| ModelError::Expr { expr: src.into(), message: m })?;
    if p.i != toks.len() {
        return Err(ModelError::Expr { expr: src.into(), message: format!("unexpected '{:?}'", toks[p.i]) });
    }
    if !v.is_finite() {
        return Err(ModelError::Expr { expr: src.into(), message: "result is not finite".into() });
    }
    Ok(v)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Op(char),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() || ch == '.' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || c[i] == 'e' || ((c[i] == '-' || c[i] == '+') && i > st && c[i - 1] == 'e')) {
                i += 1;
            }
            let t: String = c[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("bad number '{t}'"))?));
        } else if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                i += 1;
            }
            out.push(Tok::Id(c[st..i].iter().collect::<String>().trim_start_matches('$').to_string()));
        } else if "+-*/%^(),".contains(ch) {
            out.push(Tok::Op(ch));
            i += 1;
        } else {
            return Err(format!("unexpected character '{ch}'"));
        }
    }
    Ok(out)
}

struct Parser<'a> {
    t: &'a [Tok],
    i: usize,
    params: &'a Params,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Op(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop {
            if self.eat('+') {
                v += self.term()?;
            } else if self.eat('-') {
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.power()?;
        loop {
            if self.eat('*') {
                v *= self.power()?;
            } else if self.eat('/') {
                v /= self.power()?;
            } else if self.eat('%') {
                v %= self.power()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn power(&mut self) -> Result<f64, String> {
        let b = self.unary()?;
        if self.eat('^') {
            Ok(b.powf(self.power()?))
        } else {
            Ok(b)
        }
    }
    fn unary(&mut self) -> Result<f64, String> {
        if self.eat('-') {
            return Ok(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        self.atom()
    }
    fn atom(&mut self) -> Result<f64, String> {
        match self.peek().cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok(v)
            }
            Some(Tok::Op('(')) => {
                self.i += 1;
                let v = self.expr()?;
                if !self.eat(')') {
                    return Err("missing ')'".into());
                }
                Ok(v)
            }
            Some(Tok::Id(name)) => {
                self.i += 1;
                if self.eat('(') {
                    let mut args = Vec::new();
                    if !self.eat(')') {
                        loop {
                            args.push(self.expr()?);
                            if self.eat(')') {
                                break;
                            }
                            if !self.eat(',') {
                                return Err("expected ',' or ')'".into());
                            }
                        }
                    }
                    let a = |k: usize| args.get(k).copied().ok_or_else(|| format!("{name}() needs {} argument(s)", k + 1));
                    return match name.as_str() {
                        "min" => args.iter().copied().reduce(f64::min).ok_or_else(|| "min() needs arguments".into()),
                        "max" => args.iter().copied().reduce(f64::max).ok_or_else(|| "max() needs arguments".into()),
                        "abs" => Ok(a(0)?.abs()),
                        "sqrt" => Ok(a(0)?.sqrt()),
                        "sin" => Ok(a(0)?.to_radians().sin()),
                        "cos" => Ok(a(0)?.to_radians().cos()),
                        "tan" => Ok(a(0)?.to_radians().tan()),
                        "floor" => Ok(a(0)?.floor()),
                        "ceil" => Ok(a(0)?.ceil()),
                        "round" => Ok(a(0)?.round()),
                        _ => Err(format!("unknown function '{name}'")),
                    };
                }
                if name == "pi" {
                    return Ok(std::f64::consts::PI);
                }
                self.params.get(&name).copied().ok_or_else(|| {
                    let known: Vec<_> = self.params.keys().cloned().collect();
                    format!("unknown parameter '{name}' (defined: {})", if known.is_empty() { "none".into() } else { known.join(", ") })
                })
            }
            other => Err(format!("unexpected {:?}", other)),
        }
    }
}

/// The same expression in OpenSCAD syntax: `^` becomes `pow()`, `pi` becomes `PI`; parameter
/// names stay as they are (they become top-level variables).
pub fn to_scad(src: &str) -> Result<String, String> {
    let toks = lex(src)?;
    let mut p = ScadWriter { t: &toks, i: 0 };
    let (v, _) = p.expr()?;
    if p.i != toks.len() {
        return Err(format!("unexpected '{:?}'", toks[p.i]));
    }
    Ok(v)
}

/// Writes OpenSCAD source with the precedence of [`Parser`]; each result carries its binding
/// level (1 sum, 2 product, 4 unary, 5 atom) so parentheses are added only where needed.
struct ScadWriter<'a> {
    t: &'a [Tok],
    i: usize,
}

impl ScadWriter<'_> {
    fn eat(&mut self, c: char) -> bool {
        if self.t.get(self.i) == Some(&Tok::Op(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Result<(String, u8), String> {
        let (mut v, mut l) = self.term()?;
        loop {
            let op = if self.eat('+') { '+' } else if self.eat('-') { '-' } else { return Ok((v, l)) };
            let (r, _) = self.term()?;
            v = format!("{v} {op} {r}");
            l = 1;
        }
    }
    fn term(&mut self) -> Result<(String, u8), String> {
        let (mut v, mut l) = self.power()?;
        loop {
            let op = if self.eat('*') { '*' } else if self.eat('/') { '/' } else if self.eat('%') { '%' } else { return Ok((v, l)) };
            let (r, rl) = self.power()?;
            // a product on the right keeps its own grouping: a / (b * c)
            let r = if rl <= 2 { format!("({r})") } else { r };
            v = format!("{v} {op} {r}");
            l = 2;
        }
    }
    fn power(&mut self) -> Result<(String, u8), String> {
        let (b, l) = self.unary()?;
        if self.eat('^') {
            let (e, _) = self.power()?;
            return Ok((format!("pow({b}, {e})"), 5));
        }
        Ok((b, l))
    }
    fn unary(&mut self) -> Result<(String, u8), String> {
        if self.eat('-') {
            let (v, l) = self.unary()?;
            return Ok((if l < 4 { format!("-({v})") } else { format!("-{v}") }, 4));
        }
        if self.eat('+') {
            return self.unary();
        }
        self.atom()
    }
    fn atom(&mut self) -> Result<(String, u8), String> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok((format!("{v}"), 5))
            }
            Some(Tok::Op('(')) => {
                self.i += 1;
                let (v, _) = self.expr()?;
                if !self.eat(')') {
                    return Err("missing ')'".into());
                }
                Ok((format!("({v})"), 5))
            }
            Some(Tok::Id(name)) => {
                self.i += 1;
                if self.eat('(') {
                    let mut args = Vec::new();
                    if !self.eat(')') {
                        loop {
                            args.push(self.expr()?.0);
                            if self.eat(')') {
                                break;
                            }
                            if !self.eat(',') {
                                return Err("expected ',' or ')'".into());
                            }
                        }
                    }
                    return Ok((format!("{name}({})", args.join(", ")), 5));
                }
                Ok((if name == "pi" { "PI".into() } else { name }, 5))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions() {
        let mut p = Params::new();
        p.insert("wall".into(), 2.0);
        assert_eq!(eval("wall*2 + 0.4", &p).unwrap(), 4.4);
        assert_eq!(eval("max(1, wall, -3) - -1", &p).unwrap(), 3.0);
        assert!((eval("sin(30)", &p).unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(eval("2^3^2", &p).unwrap(), 512.0);
        assert!(eval("nope + 1", &p).unwrap_err().to_string().contains("wall"));
    }

    #[test]
    fn expressions_in_openscad_syntax() {
        assert_eq!(to_scad("wall*2 + 0.4").unwrap(), "wall * 2 + 0.4");
        assert_eq!(to_scad("2^3^2").unwrap(), "pow(2, pow(3, 2))");
        assert_eq!(to_scad("-(a+b)*2").unwrap(), "-(a + b) * 2");
        assert_eq!(to_scad("a / (b*c)").unwrap(), "a / (b * c)");
        assert_eq!(to_scad("max(d, 5) / 2 * pi").unwrap(), "max(d, 5) / 2 * PI");
        assert_eq!(to_scad("$fn_like - -1").unwrap(), "fn_like - -1");
    }
}
