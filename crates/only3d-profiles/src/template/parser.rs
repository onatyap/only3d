//! Recursive-descent parser for the placeholder language (grammar of Bambu Studio's
//! `PlaceholderParser.cpp` `macro_processor`).

use super::{TemplateError, TemplateErrorKind};

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Text(String),
    /// `{expr}`
    Expr(Expr),
    /// `[name]` or `[name[index_var]]`; positions are byte offsets for error reporting.
    Legacy { name: String, index: Option<(String, usize)>, pos: usize },
    /// `{if}...{elsif}...{else}...{endif}`
    If { branches: Vec<(Expr, Vec<Node>)>, otherwise: Option<Vec<Node>> },
    /// `{local x = 1; y = x * 2; y}`: PrusaSlicer's and OrcaSlicer's statements (Bambu's
    /// grammar has none); expressions among them are written out.
    Stmts(Vec<Stmt>),
}

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    /// `local name = ...` or `global name = ...` (a global lasts for the rest of the job).
    Decl { global: bool, name: String, init: Init, pos: usize },
    /// `name = ...` or `name[index] = ...`
    Assign { name: String, index: Option<Expr>, value: Init, pos: usize },
    /// An expression: its value is written out.
    Out(Expr),
    /// `if c then ... elsif c then ... else ... endif` inside a macro: only the taken branch runs.
    If { branches: Vec<(Expr, Vec<Stmt>)>, otherwise: Option<Vec<Stmt>> },
}

/// The right-hand side of a declaration or assignment.
#[derive(Clone, Debug)]
pub(crate) enum Init {
    Expr(Expr),
    /// `(a, b, c)`
    List(Vec<Expr>),
    /// `repeat(count, value)`
    Repeat(Expr, Expr),
}

#[derive(Clone, Debug)]
pub(crate) struct Expr {
    pub kind: ExprKind,
    /// Byte offset of the expression start.
    pub pos: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Var(String),
    Index(String, Box<Expr>),
    Neg(Box<Expr>),
    Plus(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Regex { lhs: Box<Expr>, pattern: String, negate: bool },
    Call(Func, Vec<Expr>),
    /// `~vector`: a vector's elements as separate arguments (in `one_of`).
    Spread(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Func {
    Min,
    Max,
    Random,
    FilamentChange,
    Digits,
    ZDigits,
    Int,
    Round,
    Ceil,
    Floor,
    /// `interpolate_table(x, (x0, y0), (x1, y1), ...)`: args are x, x0, y0, x1, y1, ...
    InterpolateTable,
    /// `size(vector)`, `empty(vector)`, `is_nil(vector[i])`: args are the variable reference
    Size,
    Empty,
    IsNil,
    /// `one_of(value, a, b, ...)`
    OneOf,
}

/// Reserved words (cannot be variable names), from the Bambu grammar's `keywords` table.
const KEYWORDS: &[&str] = &[
    "and", "digits", "zdigits", "if", "int", "else", "elsif", "endif", "false", "min", "max", "random",
    "filament_change", "round", "floor", "ceil", "not", "or", "true", "local", "global", "repeat",
    "interpolate_table", "size", "empty", "is_nil", "one_of", "then",
];

type PResult<T> = Result<T, TemplateError>;

pub(crate) struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}
fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser { src, b: src.as_bytes(), pos: 0 }
    }

    pub fn parse_template(src: &'a str) -> PResult<Vec<Node>> {
        let mut p = Parser::new(src);
        // Boost.Spirit's leading `eps` pre-skips whitespace: leading blanks/newlines are dropped.
        p.skip_ws();
        let nodes = p.text_block()?;
        if p.pos < p.b.len() {
            let kw = p.block_keyword().unwrap_or("?");
            return Err(p.err_at(p.pos, format!("`{{{kw}}}` without a matching `{{if}}`")));
        }
        Ok(nodes)
    }

    pub fn parse_condition(src: &'a str) -> PResult<Expr> {
        let mut p = Parser::new(src);
        let e = p.conditional()?;
        p.skip_ws();
        if p.pos < p.b.len() {
            return Err(p.err_here("unexpected text after the expression"));
        }
        Ok(e)
    }

    // ---------------------------------------------------------------- helpers

    fn err_at(&self, pos: usize, msg: impl Into<String>) -> TemplateError {
        TemplateError::at(self.src, pos, TemplateErrorKind::Syntax(msg.into()))
    }

    fn err_here(&self, msg: impl Into<String>) -> TemplateError {
        self.err_at(self.pos, msg)
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.pos < self.b.len() && is_space(self.b[self.pos]) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, s: &str) -> bool {
        if self.b[self.pos..].starts_with(s.as_bytes()) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8, what: &str) -> PResult<()> {
        self.skip_ws();
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.err_here(format!("expected `{}` {what}", c as char)))
        }
    }

    /// The identifier-like word at the cursor (no whitespace skipping).
    fn word_at(&self, pos: usize) -> &'a str {
        let b = self.b;
        if pos >= b.len() || !is_ident_start(b[pos]) {
            return "";
        }
        let mut end = pos + 1;
        while end < b.len() && is_ident_char(b[end]) {
            end += 1;
        }
        &self.src[pos..end]
    }

    /// Consumes `kw` if the word at the cursor is exactly `kw` (Spirit's `distinct`).
    fn eat_keyword(&mut self, kw: &str) -> bool {
        if self.word_at(self.pos) == kw {
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    /// If the cursor is at `{` followed by `else`, `elsif` or `endif`, returns that keyword.
    fn block_keyword(&self) -> Option<&'static str> {
        if self.peek() != Some(b'{') {
            return None;
        }
        let mut p = self.pos + 1;
        while p < self.b.len() && is_space(self.b[p]) {
            p += 1;
        }
        let w = self.word_at(p);
        ["else", "elsif", "endif"].into_iter().find(|k| *k == w)
    }

    /// A non-keyword identifier.
    fn identifier(&mut self) -> Option<String> {
        let w = self.word_at(self.pos);
        if w.is_empty() || KEYWORDS.contains(&w) {
            return None;
        }
        self.pos += w.len();
        Some(w.to_string())
    }

    // ---------------------------------------------------------------- text level

    fn text_block(&mut self) -> PResult<Vec<Node>> {
        let mut nodes = Vec::new();
        while self.pos < self.b.len() {
            match self.b[self.pos] {
                b'{' => {
                    if self.block_keyword().is_some() {
                        break;
                    }
                    let open = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    let save = self.pos;
                    let node = if self.eat_keyword("if") {
                        // `{if c then ...}` is PrusaSlicer's statement form; `{if c}` a block
                        let statement = self.conditional().is_ok() && {
                            self.skip_ws();
                            self.word_at(self.pos) == "then"
                        };
                        self.pos = save;
                        if statement {
                            self.statements()?
                        } else {
                            self.eat_keyword("if");
                            self.if_block(open)?
                        }
                    } else {
                        self.statements()?
                    };
                    self.skip_ws();
                    if !self.eat(b'}') {
                        let hint = match node {
                            Node::Expr(_) => {
                                " (a `{...}` macro holds an arithmetic expression; parenthesise comparisons and `?:`)"
                            }
                            _ => "",
                        };
                        return Err(self.err_here(format!("expected `}}`{hint}")));
                    }
                    nodes.push(node);
                }
                b'[' => nodes.push(self.legacy()?),
                _ => {
                    let start = self.pos;
                    while self.pos < self.b.len() && self.b[self.pos] != b'[' && self.b[self.pos] != b'{' {
                        self.pos += 1;
                    }
                    nodes.push(Node::Text(self.src[start..self.pos].to_string()));
                }
            }
        }
        Ok(nodes)
    }

    /// The inside of a `{...}` macro: one arithmetic expression (Bambu's grammar), or
    /// statements separated by `;` (PrusaSlicer's and OrcaSlicer's).
    fn statements(&mut self) -> PResult<Node> {
        let mut stmts = Vec::new();
        let mut separated = false;
        loop {
            self.skip_ws();
            if self.peek() == Some(b'}') && !stmts.is_empty() {
                break;
            }
            stmts.push(self.statement()?);
            self.skip_ws();
            if self.eat(b';') {
                separated = true;
            } else if matches!(stmts.last(), Some(Stmt::If { .. })) && self.peek() != Some(b'}') {
                // `endif` ends a statement by itself
                separated = true;
            } else {
                break;
            }
        }
        Ok(match (stmts.len(), separated) {
            (1, false) if matches!(stmts[0], Stmt::Out(_)) => {
                let Some(Stmt::Out(e)) = stmts.pop() else { unreachable!() };
                Node::Expr(e)
            }
            _ => Node::Stmts(stmts),
        })
    }

    fn statement(&mut self) -> PResult<Stmt> {
        let pos = self.pos;
        if self.eat_keyword("if") {
            let mut branches = Vec::new();
            let mut otherwise = None;
            let mut cond = self.conditional()?;
            loop {
                self.skip_ws();
                if !self.eat_keyword("then") {
                    return Err(self.err_here("expected `then`"));
                }
                branches.push((cond, self.statement_list()?));
                self.skip_ws();
                if self.eat_keyword("elsif") {
                    cond = self.conditional()?;
                } else if self.eat_keyword("else") {
                    otherwise = Some(self.statement_list()?);
                    self.skip_ws();
                    if !self.eat_keyword("endif") {
                        return Err(self.err_here("expected `endif`"));
                    }
                    break;
                } else if self.eat_keyword("endif") {
                    break;
                } else {
                    return Err(self.err_here("expected `elsif`, `else` or `endif`"));
                }
            }
            return Ok(Stmt::If { branches, otherwise });
        }
        for (kw, global) in [("local", false), ("global", true)] {
            if self.eat_keyword(kw) {
                self.skip_ws();
                let name = self.identifier().ok_or_else(|| self.err_here(format!("expected a variable name after `{kw}`")))?;
                self.skip_ws();
                self.expect(b'=', &format!("after `{kw} {name}`"))?;
                let init = self.init()?;
                return Ok(Stmt::Decl { global, name, init, pos });
            }
        }
        // `name = ...` / `name[i] = ...` (but not `name == ...`)
        if let Some(name) = self.identifier() {
            self.skip_ws();
            let index = if self.eat(b'[') {
                let i = self.additive()?;
                self.expect(b']', "to close the vector index")?;
                self.skip_ws();
                Some(i)
            } else {
                None
            };
            if self.peek() == Some(b'=') && self.b.get(self.pos + 1) != Some(&b'=') {
                self.pos += 1;
                let value = self.init()?;
                return Ok(Stmt::Assign { name, index, value, pos });
            }
            self.pos = pos;
        }
        Ok(Stmt::Out(self.additive()?))
    }

    /// Statements up to `elsif`, `else` or `endif` (`;` between them optional).
    fn statement_list(&mut self) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            if matches!(self.word_at(self.pos), "elsif" | "else" | "endif") || matches!(self.peek(), None | Some(b'}')) {
                return Ok(out);
            }
            out.push(self.statement()?);
            self.skip_ws();
            self.eat(b';');
        }
    }

    /// The right-hand side of `=`: an expression, `(a, b, ...)` or `repeat(n, value)`.
    fn init(&mut self) -> PResult<Init> {
        self.skip_ws();
        if self.eat_keyword("repeat") {
            self.expect(b'(', "after `repeat`")?;
            let n = self.conditional()?;
            self.expect(b',', "in `repeat(count, value)`")?;
            let v = self.conditional()?;
            self.expect(b')', "to close `repeat(`")?;
            return Ok(Init::Repeat(n, v));
        }
        if self.peek() == Some(b'(') && self.top_level_comma() {
            self.pos += 1;
            let mut items = vec![self.conditional()?];
            loop {
                self.skip_ws();
                if self.eat(b',') {
                    items.push(self.conditional()?);
                } else {
                    break;
                }
            }
            self.expect(b')', "to close the list")?;
            return Ok(Init::List(items));
        }
        Ok(Init::Expr(self.conditional()?))
    }

    /// Whether the `(` at the cursor holds a comma at its own level (a list, not a group).
    fn top_level_comma(&self) -> bool {
        let mut depth = 0;
        let mut in_str = false;
        for &c in &self.b[self.pos..] {
            match c {
                b'"' => in_str = !in_str,
                _ if in_str => {}
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                b',' if depth == 1 => return true,
                b'}' | b';' if depth <= 1 => return false,
                _ => {}
            }
        }
        false
    }

    /// After `{if`: condition, `}`, bodies, through `endif` (the caller consumes the final `}`).
    fn if_block(&mut self, open: usize) -> PResult<Node> {
        let mut branches = Vec::new();
        let mut otherwise: Option<Vec<Node>> = None;
        let cond = self.conditional()?;
        self.expect(b'}', "after the `{if ...}` condition")?;
        branches.push((cond, self.text_block()?));
        loop {
            if self.pos >= self.b.len() {
                return Err(self.err_at(open, "`{if}` without a matching `{endif}`"));
            }
            // text_block stopped at `{` + else/elsif/endif.
            self.pos += 1;
            self.skip_ws();
            if self.eat_keyword("elsif") {
                if otherwise.is_some() {
                    return Err(self.err_here("`{elsif}` after `{else}`"));
                }
                let c = self.conditional()?;
                self.expect(b'}', "after the `{elsif ...}` condition")?;
                branches.push((c, self.text_block()?));
            } else if self.eat_keyword("else") {
                if otherwise.is_some() {
                    return Err(self.err_here("second `{else}` in one `{if}`"));
                }
                self.expect(b'}', "after `{else`")?;
                otherwise = Some(self.text_block()?);
            } else if self.eat_keyword("endif") {
                return Ok(Node::If { branches, otherwise });
            } else {
                return Err(self.err_here("expected `elsif`, `else` or `endif`"));
            }
        }
    }

    /// `[name]` or `[name[index_var]]`.
    fn legacy(&mut self) -> PResult<Node> {
        self.pos += 1; // '['
        self.skip_ws();
        let pos = self.pos;
        let name = self.identifier().ok_or_else(|| {
            self.err_here("expected a variable name after `[` (legacy `[variable]` expansion)")
        })?;
        self.skip_ws();
        if self.eat(b']') {
            return Ok(Node::Legacy { name, index: None, pos });
        }
        if self.eat(b'[') {
            self.skip_ws();
            let ipos = self.pos;
            let idx = self.identifier().ok_or_else(|| self.err_here("expected an index variable name"))?;
            self.expect(b']', "after the index variable")?;
            self.expect(b']', "to close the legacy `[...]` expansion")?;
            return Ok(Node::Legacy { name, index: Some((idx, ipos)), pos });
        }
        Err(self.err_here("expected `]` to close the legacy `[variable]` expansion"))
    }

    // ---------------------------------------------------------------- expressions

    fn conditional(&mut self) -> PResult<Expr> {
        let c = self.logical_or()?;
        self.skip_ws();
        if self.eat(b'?') {
            let a = self.conditional()?;
            self.expect(b':', "in the `?:` expression")?;
            let b = self.conditional()?;
            let pos = c.pos;
            return Ok(Expr { kind: ExprKind::Ternary(Box::new(c), Box::new(a), Box::new(b)), pos });
        }
        Ok(c)
    }

    fn binary(op: BinOp, l: Expr, r: Expr) -> Expr {
        let pos = l.pos;
        Expr { kind: ExprKind::Binary(op, Box::new(l), Box::new(r)), pos }
    }

    fn logical_or(&mut self) -> PResult<Expr> {
        let mut l = self.logical_and()?;
        loop {
            self.skip_ws();
            if self.eat_str("||") || self.eat_keyword("or") {
                let r = self.logical_and()?;
                l = Self::binary(BinOp::Or, l, r);
            } else {
                return Ok(l);
            }
        }
    }

    fn logical_and(&mut self) -> PResult<Expr> {
        let mut l = self.equality()?;
        loop {
            self.skip_ws();
            if self.eat_str("&&") || self.eat_keyword("and") {
                let r = self.equality()?;
                l = Self::binary(BinOp::And, l, r);
            } else {
                return Ok(l);
            }
        }
    }

    fn equality(&mut self) -> PResult<Expr> {
        let mut l = self.relational()?;
        loop {
            self.skip_ws();
            if self.eat_str("==") {
                let r = self.relational()?;
                l = Self::binary(BinOp::Eq, l, r);
            } else if self.eat_str("!=") || self.eat_str("<>") {
                let r = self.relational()?;
                l = Self::binary(BinOp::Ne, l, r);
            } else if self.eat_str("=~") {
                l = self.regex(l, false)?;
            } else if self.eat_str("!~") {
                l = self.regex(l, true)?;
            } else {
                return Ok(l);
            }
        }
    }

    fn regex(&mut self, lhs: Expr, negate: bool) -> PResult<Expr> {
        self.skip_ws();
        if !self.eat(b'/') {
            return Err(self.err_here("expected a `/regex/` literal"));
        }
        let start = self.pos;
        loop {
            match self.peek() {
                None => return Err(self.err_at(start, "unterminated `/regex/` literal")),
                Some(b'\\') => self.pos = (self.pos + 2).min(self.b.len()),
                Some(b'/') => break,
                Some(_) => self.pos += 1,
            }
        }
        let pattern = self.src[start..self.pos].to_string();
        self.pos += 1;
        let pos = lhs.pos;
        Ok(Expr { kind: ExprKind::Regex { lhs: Box::new(lhs), pattern, negate }, pos })
    }

    fn relational(&mut self) -> PResult<Expr> {
        let mut l = self.additive()?;
        loop {
            self.skip_ws();
            let op = if self.eat_str("<=") {
                BinOp::Le
            } else if self.eat_str(">=") {
                BinOp::Ge
            } else if self.peek() == Some(b'<') && self.b.get(self.pos + 1) != Some(&b'>') {
                self.pos += 1;
                BinOp::Lt
            } else if self.eat(b'>') {
                BinOp::Gt
            } else {
                return Ok(l);
            };
            let r = self.additive()?;
            l = Self::binary(op, l, r);
        }
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut l = self.multiplicative()?;
        loop {
            self.skip_ws();
            let op = if self.eat(b'+') {
                BinOp::Add
            } else if self.eat(b'-') {
                BinOp::Sub
            } else {
                return Ok(l);
            };
            let r = self.multiplicative()?;
            l = Self::binary(op, l, r);
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut l = self.unary()?;
        loop {
            self.skip_ws();
            let op = if self.eat(b'*') {
                BinOp::Mul
            } else if self.eat(b'/') {
                BinOp::Div
            } else if self.eat(b'%') {
                BinOp::Mod
            } else {
                return Ok(l);
            };
            let r = self.unary()?;
            l = Self::binary(op, l, r);
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.skip_ws();
        let pos = self.pos;
        let Some(c) = self.peek() else {
            return Err(self.err_here("unexpected end of template, expected an expression"));
        };
        let boxed = |p: &mut Self| p.unary().map(Box::new);
        let kind = if is_ident_start(c) {
            let word = self.word_at(pos);
            let (f, min, max) = match word {
                "min" => (Some(Func::Min), 2, 2),
                "max" => (Some(Func::Max), 2, 2),
                "random" => (Some(Func::Random), 2, 2),
                "filament_change" => (Some(Func::FilamentChange), 1, 1),
                "digits" => (Some(Func::Digits), 2, 3),
                "zdigits" => (Some(Func::ZDigits), 2, 3),
                "int" => (Some(Func::Int), 1, 1),
                "round" => (Some(Func::Round), 1, 1),
                "ceil" => (Some(Func::Ceil), 1, 1),
                "floor" => (Some(Func::Floor), 1, 1),
                "size" => (Some(Func::Size), 1, 1),
                "empty" => (Some(Func::Empty), 1, 1),
                "is_nil" => (Some(Func::IsNil), 1, 1),
                "one_of" => (Some(Func::OneOf), 2, usize::MAX),
                _ => (None, 0, 0),
            };
            if let Some(f) = f {
                self.pos += word.len();
                return self.call(pos, word, f, min, max);
            }
            if word == "interpolate_table" {
                self.pos += word.len();
                return self.interpolate_table(pos);
            }
            match word {
                "true" | "false" => {
                    self.pos += word.len();
                    ExprKind::Bool(word == "true")
                }
                "not" => {
                    self.pos += word.len();
                    ExprKind::Not(boxed(self)?)
                }
                w if KEYWORDS.contains(&w) => {
                    return Err(self.err_here(format!("unexpected keyword `{w}`, expected an expression")));
                }
                _ => {
                    self.pos += word.len();
                    let name = word.to_string();
                    let save = self.pos;
                    self.skip_ws();
                    if self.eat(b'[') {
                        let idx = self.additive()?;
                        self.expect(b']', "to close the vector index")?;
                        ExprKind::Index(name, Box::new(idx))
                    } else {
                        self.pos = save;
                        ExprKind::Var(name)
                    }
                }
            }
        } else {
            match c {
                b'(' => {
                    self.pos += 1;
                    let e = self.conditional()?;
                    self.expect(b')', "to close `(`")?;
                    return Ok(Expr { kind: e.kind, pos });
                }
                b'-' => {
                    self.pos += 1;
                    ExprKind::Neg(boxed(self)?)
                }
                b'+' => {
                    self.pos += 1;
                    ExprKind::Plus(boxed(self)?)
                }
                b'!' => {
                    self.pos += 1;
                    ExprKind::Not(boxed(self)?)
                }
                b'~' => {
                    self.pos += 1;
                    self.skip_ws();
                    let name = self.identifier().ok_or_else(|| self.err_here("expected a vector variable after `~`"))?;
                    ExprKind::Spread(name)
                }
                b'"' => self.string()?,
                b'0'..=b'9' | b'.' => self.number()?,
                _ => return Err(self.err_here("expected an expression")),
            }
        };
        Ok(Expr { kind, pos })
    }

    /// `interpolate_table(x, (x0, y0), (x1, y1), ...)`.
    fn interpolate_table(&mut self, pos: usize) -> PResult<Expr> {
        self.expect(b'(', "after `interpolate_table`")?;
        let mut args = vec![self.conditional()?];
        loop {
            self.skip_ws();
            if !self.eat(b',') {
                break;
            }
            self.expect(b'(', "before a `(x, y)` pair of interpolate_table")?;
            args.push(self.conditional()?);
            self.expect(b',', "in a `(x, y)` pair")?;
            args.push(self.conditional()?);
            self.expect(b')', "to close a `(x, y)` pair")?;
        }
        self.expect(b')', "to close `interpolate_table(`")?;
        if args.len() < 3 {
            return Err(self.err_at(pos, "`interpolate_table` needs a value and at least one `(x, y)` pair"));
        }
        Ok(Expr { kind: ExprKind::Call(Func::InterpolateTable, args), pos })
    }

    fn call(&mut self, pos: usize, name: &str, f: Func, min: usize, max: usize) -> PResult<Expr> {
        self.expect(b'(', &format!("after `{name}`"))?;
        let mut args = vec![self.conditional()?];
        loop {
            self.skip_ws();
            if self.eat(b',') {
                if args.len() == max {
                    return Err(self.err_here(format!("too many arguments for `{name}` (max {max})")));
                }
                args.push(self.conditional()?);
            } else {
                break;
            }
        }
        self.expect(b')', &format!("to close `{name}(`"))?;
        if args.len() < min {
            return Err(self.err_at(pos, format!("`{name}` expects {min} arguments")));
        }
        Ok(Expr { kind: ExprKind::Call(f, args), pos })
    }

    fn string(&mut self) -> PResult<ExprKind> {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err(self.err_at(start, "unterminated string literal")),
                // Escapes are skipped but kept verbatim, as Bambu does.
                Some(b'\\') => self.pos = (self.pos + 2).min(self.b.len()),
                Some(b'"') => break,
                Some(_) => self.pos += 1,
            }
        }
        let s = self.src[start + 1..self.pos].to_string();
        self.pos += 1;
        Ok(ExprKind::Str(s))
    }

    /// Spirit `strict_double` (requires a `.`) first, then `int_`.
    fn number(&mut self) -> PResult<ExprKind> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let s = p.pos;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.pos += 1;
            }
            p.pos - s
        };
        let int_digits = digits(self);
        if self.peek() == Some(b'.') {
            let dot = self.pos;
            self.pos += 1;
            let frac = digits(self);
            if int_digits == 0 && frac == 0 {
                self.pos = dot;
                return Err(self.err_here("expected a number"));
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                let save = self.pos;
                self.pos += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                if digits(self) == 0 {
                    self.pos = save;
                }
            }
            let text = &self.src[start..self.pos];
            return text
                .parse::<f64>()
                .map(ExprKind::Float)
                .map_err(|_| self.err_at(start, format!("invalid number `{text}`")));
        }
        let text = &self.src[start..self.pos];
        text.parse::<i64>()
            .map(ExprKind::Int)
            .map_err(|_| self.err_at(start, format!("invalid integer `{text}`")))
    }
}
