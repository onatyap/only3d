//! A small backtracking regular-expression matcher for `=~ /.../` (full-match semantics like
//! `std::regex_match`). Supports literals, `.`, `[...]`/`[^...]` classes with ranges,
//! `\d \w \s \D \W \S` and escaped literals, groups `(...)`/`(?:...)`, alternation `|`,
//! quantifiers `* + ? {m} {m,} {m,n}` (lazy `?` suffix accepted, treated as greedy) and the
//! anchors `^ $`. Enough for `compatible_printers_condition`-style expressions.

#[derive(Debug, Clone)]
enum Atom {
    Char(char),
    Any,
    Class { ranges: Vec<(char, char)>, negated: bool },
    Group(Alt),
    Start,
    End,
}

#[derive(Debug, Clone)]
struct Piece {
    atom: Atom,
    min: u32,
    max: Option<u32>,
}

#[derive(Debug, Clone)]
struct Alt(Vec<Vec<Piece>>);

#[derive(Debug, Clone)]
pub(crate) struct Regex {
    root: Alt,
}

struct P<'a> {
    c: &'a [char],
    i: usize,
}

const DIGIT: &[(char, char)] = &[('0', '9')];
const WORD: &[(char, char)] = &[('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
const SPACE: &[(char, char)] = &[(' ', ' '), ('\t', '\r')];

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn alt(&mut self) -> Result<Alt, String> {
        let mut seqs = vec![self.seq()?];
        while self.peek() == Some('|') {
            self.i += 1;
            seqs.push(self.seq()?);
        }
        Ok(Alt(seqs))
    }

    fn seq(&mut self) -> Result<Vec<Piece>, String> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let (min, max) = self.quantifier()?;
            out.push(Piece { atom, min, max });
        }
        Ok(out)
    }

    fn escape_class(c: char) -> Option<(Vec<(char, char)>, bool)> {
        match c {
            'd' => Some((DIGIT.to_vec(), false)),
            'D' => Some((DIGIT.to_vec(), true)),
            'w' => Some((WORD.to_vec(), false)),
            'W' => Some((WORD.to_vec(), true)),
            's' => Some((SPACE.to_vec(), false)),
            'S' => Some((SPACE.to_vec(), true)),
            _ => None,
        }
    }

    fn escaped_char(c: char) -> char {
        match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            other => other,
        }
    }

    fn atom(&mut self) -> Result<Atom, String> {
        let c = self.peek().ok_or("unexpected end")?;
        self.i += 1;
        Ok(match c {
            '.' => Atom::Any,
            '^' => Atom::Start,
            '$' => Atom::End,
            '(' => {
                if self.c[self.i..].starts_with(&['?', ':']) {
                    self.i += 2;
                }
                let alt = self.alt()?;
                if self.peek() != Some(')') {
                    return Err("missing `)`".into());
                }
                self.i += 1;
                Atom::Group(alt)
            }
            '[' => self.class()?,
            '\\' => {
                let e = self.peek().ok_or("trailing backslash")?;
                self.i += 1;
                match Self::escape_class(e) {
                    Some((ranges, negated)) => Atom::Class { ranges, negated },
                    None => Atom::Char(Self::escaped_char(e)),
                }
            }
            '*' | '+' | '?' => return Err(format!("nothing to repeat before `{c}`")),
            other => Atom::Char(other),
        })
    }

    fn class(&mut self) -> Result<Atom, String> {
        let mut negated = false;
        if self.peek() == Some('^') {
            negated = true;
            self.i += 1;
        }
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("missing `]`")?;
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.peek().ok_or("trailing backslash")?;
                self.i += 1;
                if let Some((r, false)) = Self::escape_class(e) {
                    ranges.extend(r);
                    continue;
                }
                Self::escaped_char(e)
            } else {
                c
            };
            if self.peek() == Some('-') && self.c.get(self.i + 1).is_some_and(|&n| n != ']') {
                let hi = self.c[self.i + 1];
                self.i += 2;
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Atom::Class { ranges, negated })
    }

    fn number(&mut self) -> Option<u32> {
        let s = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        self.c[s..self.i].iter().collect::<String>().parse().ok()
    }

    fn quantifier(&mut self) -> Result<(u32, Option<u32>), String> {
        let q = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                let save = self.i;
                self.i += 1;
                let Some(m) = self.number() else {
                    self.i = save;
                    return Ok((1, Some(1)));
                };
                let q = if self.peek() == Some(',') {
                    self.i += 1;
                    (m, self.number())
                } else {
                    (m, Some(m))
                };
                if self.peek() != Some('}') {
                    return Err("malformed `{m,n}` quantifier".into());
                }
                self.i += 1;
                if self.peek() == Some('?') {
                    self.i += 1;
                }
                return Ok(q);
            }
            _ => return Ok((1, Some(1))),
        };
        self.i += 1;
        if self.peek() == Some('?') {
            self.i += 1;
        }
        Ok(q)
    }
}

type K<'k> = &'k mut dyn FnMut(usize) -> bool;

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = P { c: &chars, i: 0 };
        let root = p.alt()?;
        if p.i != chars.len() {
            return Err("unbalanced `)`".into());
        }
        Ok(Regex { root })
    }

    pub fn is_full_match(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len();
        m_alt(&self.root, &chars, 0, &mut |j| j == n)
    }
}

fn m_alt(alt: &Alt, s: &[char], i: usize, k: K<'_>) -> bool {
    alt.0.iter().any(|seq| m_seq(seq, s, i, k))
}

fn m_seq(items: &[Piece], s: &[char], i: usize, k: K<'_>) -> bool {
    match items.split_first() {
        None => k(i),
        Some((piece, rest)) => m_rep(piece, 0, s, i, &mut |j| m_seq(rest, s, j, k)),
    }
}

fn m_rep(piece: &Piece, count: u32, s: &[char], i: usize, k: K<'_>) -> bool {
    if piece.max.is_none_or(|m| count < m) {
        let matched = m_atom(&piece.atom, s, i, &mut |j| {
            // An empty iteration beyond the minimum cannot make progress.
            if j == i && count >= piece.min {
                return false;
            }
            m_rep(piece, count + 1, s, j, k)
        });
        if matched {
            return true;
        }
    }
    count >= piece.min && k(i)
}

fn m_atom(atom: &Atom, s: &[char], i: usize, k: K<'_>) -> bool {
    match atom {
        Atom::Char(c) => i < s.len() && s[i] == *c && k(i + 1),
        Atom::Any => i < s.len() && s[i] != '\n' && s[i] != '\r' && k(i + 1),
        Atom::Class { ranges, negated } => {
            i < s.len() && (ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&s[i])) != *negated) && k(i + 1)
        }
        Atom::Group(alt) => m_alt(alt, s, i, k),
        Atom::Start => i == 0 && k(i),
        Atom::End => i == s.len() && k(i),
    }
}

#[cfg(test)]
mod tests {
    use super::Regex;

    #[test]
    fn full_match() {
        let m = |p: &str, s: &str| Regex::new(p).unwrap().is_full_match(s);
        assert!(m(".*PRINTER_VENDOR_BAMBU.*", "foo\nPRINTER_VENDOR_BAMBU bar".replace('\n', " ").as_str()));
        assert!(!m("abc", "abcd"));
        assert!(m("ab*c", "ac"));
        assert!(m("a(b|c)+d", "abcbd"));
        assert!(m("[0-9]{2,3}", "123"));
        assert!(!m("[0-9]{2,3}", "1234"));
        assert!(m("\\d+\\.\\d", "12.5"));
        assert!(m("[^x]*", "abc"));
        assert!(!m("[^x]*", "axc"));
        assert!(m("(?:a|b)?c", "c"));
        assert!(m("(a*)*b", "aaab"));
    }
}
