//! Evaluator: mirrors the semantic actions of Bambu Studio's `PlaceholderParser.cpp`
//! (`expr<Iterator>` arithmetic, `compare_op`, `legacy_variable_expansion`,
//! `scalar_variable_reference`, `vector_variable_reference`).

use std::collections::HashMap;

use super::parser::{BinOp, Expr, ExprKind, Func, Init, Node, Stmt};
use super::{Context, TemplateError, TemplateErrorKind, Value};
use crate::format::format_float;

/// Runtime value of an expression.
#[derive(Clone, Debug)]
pub(crate) enum Val {
    /// Result of `filament_change()`; prints as nothing.
    Empty,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl Val {
    fn type_name(&self) -> &'static str {
        match self {
            Val::Empty => "empty value",
            Val::Bool(_) => "bool",
            Val::Int(_) => "int",
            Val::Float(_) => "float",
            Val::Str(_) => "string",
        }
    }

    /// `expr::to_string`.
    fn render(&self) -> String {
        match self {
            Val::Empty => String::new(),
            Val::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Val::Int(i) => i.to_string(),
            Val::Float(f) => format_float(*f),
            Val::Str(s) => s.clone(),
        }
    }

    fn is_numeric(&self) -> bool {
        matches!(self, Val::Int(_) | Val::Float(_))
    }

    fn as_f64(&self) -> f64 {
        match self {
            Val::Int(i) => *i as f64,
            Val::Float(f) => *f,
            _ => 0.0,
        }
    }

    /// `expr::as_i` (truncating).
    fn as_i(&self) -> i64 {
        match self {
            Val::Int(i) => *i,
            Val::Float(f) => *f as i64,
            _ => 0,
        }
    }

    /// `expr::as_i_rounded`.
    fn as_i_rounded(&self) -> i64 {
        match self {
            Val::Int(i) => *i,
            Val::Float(f) => f.round() as i64,
            _ => 0,
        }
    }
}

type EResult<T> = Result<T, TemplateError>;

pub(crate) struct Evaluator<'a> {
    src: &'a str,
    layers: &'a [&'a Context],
    current_filament: usize,
    rng: u64,
    /// `local` variables of this template (and assignments to anything else).
    locals: HashMap<String, Value>,
    /// `global` variables: the caller keeps them from one template to the next.
    pub globals: HashMap<String, Value>,
}

/// `escape_string_cstyle` used by `ConfigOptionString::serialize`.
fn escape_cstyle(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

impl<'a> Evaluator<'a> {
    pub fn new(src: &'a str, layers: &'a [&'a Context], current_filament: usize) -> Self {
        Evaluator { src, layers, current_filament, rng: 0x9E37_79B9_7F4A_7C15, locals: HashMap::new(), globals: HashMap::new() }
    }

    fn err(&self, pos: usize, msg: impl Into<String>) -> TemplateError {
        TemplateError::at(self.src, pos, TemplateErrorKind::Eval(msg.into()))
    }

    fn unknown(&self, pos: usize, name: &str) -> TemplateError {
        TemplateError::at(self.src, pos, TemplateErrorKind::UnknownVariable(name.to_string()))
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.locals.get(name).or_else(|| self.globals.get(name)).or_else(|| self.layers.iter().find_map(|c| c.get(name)))
    }

    /// A value to store: a scalar, or a list from `(a, b, ...)` / `repeat(n, x)` (ints become
    /// floats when the list mixes them, as PrusaSlicer's vectors do).
    fn init_value(&mut self, init: &Init, pos: usize) -> EResult<Value> {
        let items: Vec<Val> = match init {
            Init::Expr(e) => return Ok(Self::to_value(self.eval(e)?)),
            Init::List(es) => {
                let mut v = Vec::with_capacity(es.len());
                for e in es {
                    v.push(self.eval(e)?);
                }
                v
            }
            Init::Repeat(n, x) => {
                let n = self.eval(n)?.as_i().clamp(0, 100_000) as usize;
                vec![self.eval(x)?; n]
            }
        };
        Ok(if items.iter().all(|v| matches!(v, Val::Bool(_))) && !items.is_empty() {
            Value::Bools(items.iter().map(|v| matches!(v, Val::Bool(true))).collect())
        } else if items.iter().all(|v| matches!(v, Val::Int(_))) {
            Value::Ints(items.iter().map(Val::as_i).collect())
        } else if items.iter().all(Val::is_numeric) {
            Value::Floats(items.iter().map(Val::as_f64).collect())
        } else if items.iter().all(|v| matches!(v, Val::Str(_))) {
            Value::Strs(items.iter().map(Val::render).collect())
        } else {
            return Err(self.err(pos, "a list must hold values of one kind"));
        })
    }

    fn to_value(v: Val) -> Value {
        match v {
            Val::Bool(b) => Value::Bool(b),
            Val::Int(i) => Value::Int(i),
            Val::Float(f) => Value::Float(f),
            Val::Str(s) => Value::Str(s),
            Val::Empty => Value::Str(String::new()),
        }
    }

    /// Store `v` as `name` where it lives (a global stays global; anything else is local).
    fn store(&mut self, name: &str, v: Value) {
        if self.globals.contains_key(name) && !self.locals.contains_key(name) {
            self.globals.insert(name.to_string(), v);
        } else {
            self.locals.insert(name.to_string(), v);
        }
    }

    fn run(&mut self, st: &Stmt, out: &mut String) -> EResult<()> {
        match st {
            Stmt::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.eval_bool(cond)? {
                        for s in body {
                            self.run(s, out)?;
                        }
                        return Ok(());
                    }
                }
                for s in otherwise.iter().flatten() {
                    self.run(s, out)?;
                }
            }
            Stmt::Out(e) => out.push_str(&self.eval(e)?.render()),
            Stmt::Decl { global, name, init, pos } => {
                let v = self.init_value(init, *pos)?;
                if *global {
                    self.globals.insert(name.clone(), v);
                } else {
                    self.locals.insert(name.clone(), v);
                }
            }
            Stmt::Assign { name, index: None, value, pos } => {
                let v = self.init_value(value, *pos)?;
                self.store(name, v);
            }
            Stmt::Assign { name, index: Some(idx), value, pos } => {
                let i = self.eval(idx)?.as_i().max(0) as usize;
                let new = self.init_value(value, *pos)?;
                // set one element (PrusaSlicer also lets a template change the writer's state,
                // `e_retracted[0] = ...`; only3d keeps it as a variable)
                let mut cur = self.lookup(name).cloned().unwrap_or(Value::Floats(Vec::new()));
                match (&mut cur, new) {
                    (Value::Floats(v), n @ (Value::Float(_) | Value::Int(_))) => {
                        let f = match n {
                            Value::Float(f) => f,
                            Value::Int(i) => i as f64,
                            _ => unreachable!(),
                        };
                        if v.len() <= i {
                            v.resize(i + 1, 0.0);
                        }
                        v[i] = f;
                    }
                    (Value::Ints(v), Value::Int(n)) => {
                        if v.len() <= i {
                            v.resize(i + 1, 0);
                        }
                        v[i] = n;
                    }
                    (Value::Bools(v), Value::Bool(n)) => {
                        if v.len() <= i {
                            v.resize(i + 1, false);
                        }
                        v[i] = n;
                    }
                    (Value::Strs(v), Value::Str(n)) => {
                        if v.len() <= i {
                            v.resize(i + 1, String::new());
                        }
                        v[i] = n;
                    }
                    _ => return Err(self.err(*pos, format!("cannot assign that to an element of `{name}`"))),
                }
                self.store(name, cur);
            }
        }
        Ok(())
    }

    pub fn render_nodes(&mut self, nodes: &[Node], out: &mut String) -> EResult<()> {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Expr(e) => {
                    let v = self.eval(e)?;
                    out.push_str(&v.render());
                }
                Node::Stmts(stmts) => {
                    for st in stmts {
                        self.run(st, out)?;
                    }
                }
                Node::Legacy { name, index, pos } => {
                    let s = self.legacy(name, index.as_ref(), *pos)?;
                    out.push_str(&s);
                }
                Node::If { branches, otherwise } => {
                    // Bambu evaluates every condition and every branch (macros are evaluated
                    // while parsing), then keeps the first branch whose condition is true.
                    let mut chosen: Option<String> = None;
                    for (cond, body) in branches {
                        let c = self.eval_bool(cond)?;
                        let mut buf = String::new();
                        self.render_nodes(body, &mut buf)?;
                        if c && chosen.is_none() {
                            chosen = Some(buf);
                        }
                    }
                    if let Some(body) = otherwise {
                        let mut buf = String::new();
                        self.render_nodes(body, &mut buf)?;
                        if chosen.is_none() {
                            chosen = Some(buf);
                        }
                    }
                    if let Some(s) = chosen {
                        out.push_str(&s);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn eval_bool(&mut self, e: &Expr) -> EResult<bool> {
        match self.eval(e)? {
            Val::Bool(b) => Ok(b),
            other => Err(self.err(e.pos, format!("not a boolean expression (got {})", other.type_name()))),
        }
    }

    fn eval(&mut self, e: &Expr) -> EResult<Val> {
        Ok(match &e.kind {
            ExprKind::Int(i) => Val::Int(*i),
            ExprKind::Float(f) => Val::Float(*f),
            ExprKind::Bool(b) => Val::Bool(*b),
            ExprKind::Str(s) => Val::Str(s.clone()),
            ExprKind::Var(name) => self.scalar_ref(name, e.pos)?,
            ExprKind::Index(name, idx) => self.vector_ref(name, idx, e.pos)?,
            ExprKind::Neg(x) => match self.eval(x)? {
                Val::Int(i) => Val::Int(i.wrapping_neg()),
                Val::Float(f) => Val::Float(-f),
                other => {
                    return Err(self.err(e.pos, format!("cannot apply unary minus to a {}", other.type_name())))
                }
            },
            ExprKind::Plus(x) => self.eval(x)?,
            ExprKind::Not(x) => match self.eval(x)? {
                Val::Bool(b) => Val::Bool(!b),
                other => {
                    return Err(self.err(e.pos, format!("cannot apply `!`/`not` to a {}", other.type_name())))
                }
            },
            ExprKind::Binary(op, l, r) => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                self.binary(*op, a, b, l.pos, r.pos)?
            }
            ExprKind::Ternary(c, a, b) => {
                let cv = self.eval(c)?;
                let av = self.eval(a)?;
                let bv = self.eval(b)?;
                match cv {
                    Val::Bool(true) => av,
                    Val::Bool(false) => bv,
                    other => {
                        return Err(self.err(
                            c.pos,
                            format!("`?:` condition is not a boolean expression (got {})", other.type_name()),
                        ))
                    }
                }
            }
            ExprKind::Regex { lhs, pattern, negate } => {
                let v = self.eval(lhs)?;
                let Val::Str(s) = v else {
                    return Err(self.err(lhs.pos, "left hand side of a regex match must be a string"));
                };
                let re = super::regex::Regex::new(pattern)
                    .map_err(|m| self.err(e.pos, format!("regular expression compilation failed: {m}")))?;
                Val::Bool(re.is_full_match(&s) != *negate)
            }
            ExprKind::Call(f, args) => self.call(*f, args, e.pos)?,
            ExprKind::Spread(_) => return Err(self.err(e.pos, "`~vector` only works among one_of's choices")),
        })
    }

    fn numeric_pair(&self, a: &Val, b: &Val, pos: usize, msg: &str) -> EResult<()> {
        if a.is_numeric() && b.is_numeric() {
            Ok(())
        } else {
            Err(self.err(pos, format!("{msg} ({} and {})", a.type_name(), b.type_name())))
        }
    }

    fn binary(&self, op: BinOp, a: Val, b: Val, lpos: usize, rpos: usize) -> EResult<Val> {
        let float = matches!(a, Val::Float(_)) || matches!(b, Val::Float(_));
        Ok(match op {
            BinOp::Add => match (a, b) {
                (Val::Str(s), b) => Val::Str(s + &b.render()),
                (a, Val::Str(s)) => Val::Str(a.render() + &s),
                (a, b) => {
                    self.numeric_pair(&a, &b, lpos, "cannot add non-numeric types")?;
                    if float {
                        Val::Float(a.as_f64() + b.as_f64())
                    } else {
                        Val::Int(a.as_i().wrapping_add(b.as_i()))
                    }
                }
            },
            BinOp::Sub | BinOp::Mul => {
                let what = if op == BinOp::Sub { "cannot subtract non-numeric types" } else { "cannot multiply non-numeric types" };
                self.numeric_pair(&a, &b, lpos, what)?;
                match (op, float) {
                    (BinOp::Sub, true) => Val::Float(a.as_f64() - b.as_f64()),
                    (BinOp::Sub, false) => Val::Int(a.as_i().wrapping_sub(b.as_i())),
                    (_, true) => Val::Float(a.as_f64() * b.as_f64()),
                    (_, false) => Val::Int(a.as_i().wrapping_mul(b.as_i())),
                }
            }
            BinOp::Div | BinOp::Mod => {
                self.numeric_pair(&a, &b, lpos, "cannot divide non-numeric types")?;
                let zero = match b {
                    Val::Int(i) => i == 0,
                    _ => b.as_f64() == 0.0,
                };
                if zero {
                    return Err(self.err(rpos, "division by zero"));
                }
                match (op, float) {
                    (BinOp::Div, true) => Val::Float(a.as_f64() / b.as_f64()),
                    (BinOp::Div, false) => Val::Int(a.as_i().wrapping_div(b.as_i())),
                    (_, true) => Val::Float(a.as_f64() % b.as_f64()),
                    (_, false) => Val::Int(a.as_i().wrapping_rem(b.as_i())),
                }
            }
            BinOp::Eq => Val::Bool(self.compare(&a, &b, '=', lpos)?),
            BinOp::Ne => Val::Bool(!self.compare(&a, &b, '=', lpos)?),
            BinOp::Lt => Val::Bool(self.compare(&a, &b, '<', lpos)?),
            BinOp::Gt => Val::Bool(self.compare(&a, &b, '>', lpos)?),
            // Bambu implements `<=` as `!(a > b)` and `>=` as `!(a < b)`.
            BinOp::Le => Val::Bool(!self.compare(&a, &b, '>', lpos)?),
            BinOp::Ge => Val::Bool(!self.compare(&a, &b, '<', lpos)?),
            BinOp::And | BinOp::Or => match (&a, &b) {
                (Val::Bool(x), Val::Bool(y)) => Val::Bool(if op == BinOp::And { *x && *y } else { *x || *y }),
                _ => {
                    return Err(self.err(
                        lpos,
                        format!(
                            "cannot apply a logical operator to non-boolean operands ({} and {})",
                            a.type_name(),
                            b.type_name()
                        ),
                    ))
                }
            },
        })
    }

    /// `expr::compare_op`.
    fn compare(&self, a: &Val, b: &Val, op: char, pos: usize) -> EResult<bool> {
        if a.is_numeric() && b.is_numeric() {
            let float = matches!(a, Val::Float(_)) || matches!(b, Val::Float(_));
            return Ok(match (op, float) {
                ('=', true) => (a.as_f64() - b.as_f64()).abs() < 1e-8,
                ('=', false) => a.as_i() == b.as_i(),
                ('<', true) => a.as_f64() < b.as_f64(),
                ('<', false) => a.as_i() < b.as_i(),
                (_, true) => a.as_f64() > b.as_f64(),
                (_, false) => a.as_i() > b.as_i(),
            });
        }
        if let (Val::Bool(x), Val::Bool(y)) = (a, b) {
            if op != '=' {
                return Err(self.err(pos, "cannot compare booleans with `<`/`>`"));
            }
            return Ok(x == y);
        }
        if matches!(a, Val::Str(_)) || matches!(b, Val::Str(_)) {
            let (x, y) = (a.render(), b.render());
            return Ok(match op {
                '=' => x == y,
                '<' => x < y,
                _ => x > y,
            });
        }
        Err(self.err(pos, format!("cannot compare a {} with a {}", a.type_name(), b.type_name())))
    }

    fn call(&mut self, f: Func, args: &[Expr], pos: usize) -> EResult<Val> {
        // these look at a variable itself, not its value
        if matches!(f, Func::Size | Func::Empty | Func::IsNil) {
            let (name, idx) = match &args[0].kind {
                ExprKind::Var(n) => (n.clone(), None),
                ExprKind::Index(n, i) => (n.clone(), Some(i.as_ref().clone())),
                _ => return Err(self.err(args[0].pos, "expected a variable")),
            };
            let i = match idx {
                Some(e) => Some(self.eval(&e)?.as_i().max(0) as usize),
                None => None,
            };
            let v = self.lookup(&name).ok_or_else(|| self.unknown(args[0].pos, &name))?;
            return Ok(match f {
                Func::Size => Val::Int(if v.is_vector() { v.len() as i64 } else { 1 }),
                Func::Empty => Val::Bool(v.is_vector() && v.is_empty()),
                _ => Val::Bool(match (v, i) {
                    (Value::Floats(x), Some(i)) => x.get(i).is_some_and(|f| f.is_nan()),
                    (Value::Float(f), None) => f.is_nan(),
                    _ => false,
                }),
            });
        }
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            match &a.kind {
                // one_of(x, ~list): every element of the list is a choice
                ExprKind::Spread(name) if f == Func::OneOf => {
                    let v = self.lookup(name).ok_or_else(|| self.unknown(a.pos, name))?.clone();
                    vals.extend((0..v.len()).map(|i| Val::Str(Self::serialize_element(&v, i))));
                }
                _ => vals.push(self.eval(a)?),
            }
        }
        let name = format!("{f:?}").to_lowercase();
        let need_numeric = |ev: &Self, v: &Val, i: usize| -> EResult<()> {
            if v.is_numeric() {
                Ok(())
            } else {
                Err(ev.err(args[i].pos, format!("`{name}`: not a numeric type ({})", v.type_name())))
            }
        };
        Ok(match f {
            Func::Min | Func::Max | Func::Random => {
                need_numeric(self, &vals[0], 0)?;
                need_numeric(self, &vals[1], 1)?;
                let float = matches!(vals[0], Val::Float(_)) || matches!(vals[1], Val::Float(_));
                let (a, b) = (&vals[0], &vals[1]);
                match (f, float) {
                    (Func::Min, true) => Val::Float(a.as_f64().min(b.as_f64())),
                    (Func::Min, false) => Val::Int(a.as_i().min(b.as_i())),
                    (Func::Max, true) => Val::Float(a.as_f64().max(b.as_f64())),
                    (Func::Max, false) => Val::Int(a.as_i().max(b.as_i())),
                    (_, true) => {
                        let r = self.next_random() as f64 / u64::MAX as f64;
                        Val::Float(a.as_f64() + r * (b.as_f64() - a.as_f64()))
                    }
                    (_, false) => {
                        let (lo, hi) = (a.as_i().min(b.as_i()), a.as_i().max(b.as_i()));
                        let span = (hi - lo) as u64 + 1;
                        Val::Int(lo + (self.next_random() % span) as i64)
                    }
                }
            }
            Func::FilamentChange => {
                need_numeric(self, &vals[0], 0)?;
                self.current_filament = vals[0].as_i().max(0) as usize;
                Val::Empty
            }
            Func::Digits | Func::ZDigits => {
                need_numeric(self, &vals[0], 0)?;
                let Val::Int(width) = vals[1] else {
                    return Err(self.err(args[1].pos, "digits: second parameter must be integer"));
                };
                let width = width.clamp(0, 64) as usize;
                let zero = f == Func::ZDigits;
                match vals.get(2) {
                    Some(Val::Int(dec)) => {
                        let dec = (*dec).clamp(0, 64) as usize;
                        let x = vals[0].as_f64();
                        Val::Str(if zero { format!("{x:0width$.dec$}") } else { format!("{x:width$.dec$}") })
                    }
                    Some(_) => return Err(self.err(args[2].pos, "digits: third parameter must be integer")),
                    None => {
                        let i = vals[0].as_i_rounded();
                        Val::Str(if zero { format!("{i:0width$}") } else { format!("{i:width$}") })
                    }
                }
            }
            Func::InterpolateTable => {
                // piecewise linear through (x0, y0), (x1, y1), ...; flat beyond the ends
                let x = vals[0].as_f64();
                let pts: Vec<(f64, f64)> = vals[1..].chunks(2).map(|c| (c[0].as_f64(), c[1].as_f64())).collect();
                let y = if x <= pts[0].0 {
                    pts[0].1
                } else if x >= pts[pts.len() - 1].0 {
                    pts[pts.len() - 1].1
                } else {
                    let k = pts.windows(2).position(|w| x >= w[0].0 && x <= w[1].0).unwrap_or(0);
                    let ((x0, y0), (x1, y1)) = (pts[k], pts[k + 1]);
                    if (x1 - x0).abs() < 1e-12 {
                        y0
                    } else {
                        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
                    }
                };
                Val::Float(y)
            }
            Func::OneOf => {
                let first = vals[0].render();
                Val::Bool(vals[1..].iter().any(|v| v.render() == first))
            }
            Func::Size | Func::Empty | Func::IsNil => unreachable!("handled above"),
            Func::Int | Func::Round | Func::Ceil | Func::Floor => match &vals[0] {
                Val::Int(i) => Val::Int(*i),
                Val::Float(x) => Val::Int(match f {
                    Func::Int => x.trunc(),
                    Func::Round => x.round(),
                    Func::Ceil => x.ceil(),
                    _ => x.floor(),
                } as i64),
                other => return Err(self.err(pos, format!("`{name}` of a non-numeric value ({})", other.type_name()))),
            },
        })
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*: deterministic (Bambu seeds its RNG per G-code export).
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Element index Bambu uses when a float vector is referenced without an index:
    /// `filament_map[current_filament]` (1-based extruder number, used as a 0-based index).
    fn scalar_vector_index(&self) -> usize {
        match self.lookup("filament_map") {
            Some(Value::Ints(map)) => map.get(self.current_filament).map_or(0, |&v| v.max(0) as usize),
            _ => 0,
        }
    }

    /// `MyContext::scalar_variable_reference`.
    fn scalar_ref(&self, name: &str, pos: usize) -> EResult<Val> {
        let v = self.lookup(name).ok_or_else(|| self.unknown(pos, name))?;
        Ok(match v {
            Value::Bool(b) => Val::Bool(*b),
            Value::Int(i) => Val::Int(*i),
            Value::Float(f) => Val::Float(*f),
            Value::Str(s) => Val::Str(s.clone()),
            Value::Floats(f) => {
                if f.is_empty() {
                    return Err(self.err(pos, format!("variable `{name}` is an empty vector")));
                }
                let idx = if f.len() == 1 { 0 } else { self.scalar_vector_index() };
                Val::Float(*f.get(idx).unwrap_or(&f[0]))
            }
            other => {
                return Err(self.err(
                    pos,
                    format!("variable `{name}` is a {}; index it, e.g. `{name}[0]`", other.type_name()),
                ))
            }
        })
    }

    /// `MyContext::vector_variable_reference`.
    fn vector_ref(&mut self, name: &str, idx: &Expr, pos: usize) -> EResult<Val> {
        let i = match self.eval(idx)? {
            Val::Int(i) => i,
            other => {
                return Err(self.err(
                    idx.pos,
                    format!("non-integer index ({}) is not allowed to address a vector variable", other.type_name()),
                ))
            }
        };
        let v = self.lookup(name).ok_or_else(|| self.unknown(pos, name))?;
        if !v.is_vector() {
            return Err(self.err(pos, format!("referencing scalar variable `{name}` as a vector")));
        }
        if v.is_empty() {
            return Err(self.err(pos, format!("indexing empty vector variable `{name}`")));
        }
        let i = if i < 0 || i as usize >= v.len() { 0 } else { i as usize };
        Ok(match v {
            Value::Bools(x) => Val::Bool(x[i]),
            Value::Ints(x) => Val::Int(x[i]),
            Value::Floats(x) => Val::Float(x[i]),
            Value::Strs(x) => Val::Str(x[i].clone()),
            _ => unreachable!("checked is_vector"),
        })
    }

    fn serialize_element(v: &Value, i: usize) -> String {
        match v {
            Value::Bools(x) => if x[i] { "1" } else { "0" }.to_string(),
            Value::Ints(x) => x[i].to_string(),
            Value::Floats(x) => {
                if x[i].is_nan() {
                    "nil".to_string()
                } else {
                    format_float(x[i])
                }
            }
            Value::Strs(x) => x[i].clone(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => format_float(*f),
            Value::Str(s) => escape_cstyle(s),
        }
    }

    /// `legacy_variable_expansion` / `legacy_variable_expansion2`.
    fn legacy(&self, name: &str, index: Option<&(String, usize)>, pos: usize) -> EResult<String> {
        match index {
            None => {
                let (value, idx) = if let Some(v) = self.lookup(name) {
                    (v, self.current_filament)
                } else {
                    let split = name.rfind('_').and_then(|us| self.lookup(&name[..us]).map(|v| (us, v)));
                    let Some((us, v)) = split else {
                        return Err(self.unknown(pos, name));
                    };
                    if !v.is_vector() {
                        return Err(self.err(pos, format!("trying to index scalar variable `{}`", &name[..us])));
                    }
                    let idx = name[us + 1..]
                        .parse::<usize>()
                        .map_err(|_| self.err(pos + us + 1, format!("invalid vector index in `{name}`")))?;
                    (v, idx)
                };
                if !value.is_vector() {
                    return Ok(Self::serialize_element(value, 0));
                }
                if value.is_empty() {
                    return Err(self.err(pos, format!("indexing empty vector variable `{name}`")));
                }
                let i = if idx >= value.len() { 0 } else { idx };
                Ok(Self::serialize_element(value, i))
            }
            Some((iname, ipos)) => {
                let value = self
                    .lookup(name)
                    .or_else(|| name.strip_suffix('_').and_then(|n| self.lookup(n)))
                    .ok_or_else(|| self.unknown(pos, name))?;
                if !value.is_vector() {
                    return Err(self.err(pos, format!("trying to index scalar variable `{name}`")));
                }
                if value.is_empty() {
                    return Err(self.err(pos, format!("indexing empty vector variable `{name}`")));
                }
                let i = match self.lookup(iname) {
                    None => return Err(self.unknown(*ipos, iname)),
                    Some(Value::Int(i)) => *i,
                    Some(other) => {
                        return Err(self.err(
                            *ipos,
                            format!("indexing variable `{iname}` has to be an int (is {})", other.type_name()),
                        ))
                    }
                };
                if i < 0 {
                    return Err(self.err(*ipos, format!("negative vector index `{iname}` = {i}")));
                }
                let i = if i as usize >= value.len() { 0 } else { i as usize };
                Ok(Self::serialize_element(value, i))
            }
        }
    }
}
