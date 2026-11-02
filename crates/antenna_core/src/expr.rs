//! Design-variable expressions, as in the HFSS and CST variable tables.
//!
//! Operators `+ - * / ^ %`, unary minus, comparisons (giving 1 or 0), lazy
//! `if(cond, a, b)`, the usual functions, the constants `pi e c0 eta0`, and number
//! literals with unit suffixes converted to SI (`5mm`, `2.4GHz`, `30deg`).
//!
//! A variable shadows a constant of the same name. `ln` and `log` are both the
//! natural logarithm; `log10` is base 10. `%` keeps the sign of the dividend.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::f64::consts::{E, PI};

use anyhow::{Result, anyhow, bail};

use crate::{C0, ETA0};

pub type Vars = BTreeMap<String, f64>;

const MAX_DEPTH: usize = 200;
const UNITS: &str = "mm, cm, m, um, nm, mil, in, Hz, kHz, MHz, GHz, THz, deg, rad";
const CONSTANTS: &str = "pi, e, c0, eta0";
/// What [`arity`] accepts, for offering them while typing.
const FUNCTIONS: [&str; 23] = ["sqrt", "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh", "exp", "ln", "log", "log10", "pow", "abs", "floor", "ceil", "round", "min", "max", "if"];

/// Evaluates an expression against a variable table.
pub fn eval(src: &str, vars: &Vars) -> Result<f64> {
    let node = parse(src)?;
    let value = eval_node(&node, vars).map_err(|e| anyhow!("{e} in \"{src}\""))?;
    if !value.is_finite() {
        bail!("\"{src}\" does not evaluate to a finite number");
    }
    Ok(value)
}

/// A frequency someone typed, in GHz. Units count (`915 MHz`, `2.45GHz`). A bare number
/// is GHz unless it's 100 or more: nothing here is solved at 915 GHz, so that means MHz.
pub fn parse_ghz(src: &str) -> Option<f64> {
    if let Ok(v) = src.trim().parse::<f64>() {
        return v.is_finite().then_some(if v >= 100.0 { v / 1e3 } else { v });
    }
    if !src.to_lowercase().contains("hz") {
        return None;
    }
    eval(src, &Vars::default()).ok().map(|hz| hz / 1e9)
}

/// What a piece of an expression is, for colouring it and for offering what fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Number,
    Unit,
    Variable,
    Constant,
    Function,
    Operator,
    /// Something the parser would refuse: a name nobody defined, a bad character.
    Unknown,
}

/// A piece of an expression, as byte offsets into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// Splits an expression the way [`lex`] reads it, but for text still being typed: this
/// can't fail, and what the parser would refuse comes out as `Unknown`. White space
/// has no token.
pub fn tokens(src: &str, is_var: impl Fn(&str) -> bool) -> Vec<Token> {
    let b: Vec<char> = src.chars().collect();
    let at: Vec<usize> = src.char_indices().map(|(k, _)| k).chain([src.len()]).collect();
    let word_end = |mut i: usize| {
        while i < b.len() && is_ident_char(b[i]) {
            i += 1;
        }
        i
    };
    let skip_space = |mut i: usize| {
        while b.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let end = number_end(&b, i);
            out.push(Token { start: at[i], end: at[end], kind: TokenKind::Number });
            i = end;
            // The suffix rule of `lex_number`: touching, it has to be a unit; after a
            // space, it's a unit only if it is one, and otherwise a word of its own.
            let (from, to) = (skip_space(end), word_end(skip_space(end)));
            let word: String = b[from..to].iter().collect();
            let known = unit(&word).is_some();
            if to > from && (from == end || (known && b.get(to) != Some(&'('))) {
                out.push(Token { start: at[from], end: at[to], kind: if known { TokenKind::Unit } else { TokenKind::Unknown } });
                i = to;
            }
        } else if c.is_alphabetic() || c == '_' {
            let end = word_end(i);
            let word: String = b[i..end].iter().collect();
            let kind = if b.get(skip_space(end)) == Some(&'(') {
                if arity(&word).is_some() { TokenKind::Function } else { TokenKind::Unknown }
            } else if is_var(&word) {
                TokenKind::Variable
            } else if constant(&word).is_some() {
                TokenKind::Constant
            } else if arity(&word).is_some() {
                TokenKind::Function
            } else {
                TokenKind::Unknown
            };
            out.push(Token { start: at[i], end: at[end], kind });
            i = end;
        } else {
            let pair = matches!(c, '<' | '>' | '=' | '!') && b.get(i + 1) == Some(&'=');
            let end = i + if pair { 2 } else { 1 };
            let kind = if pair || "+-*/^%(),<>".contains(c) { TokenKind::Operator } else { TokenKind::Unknown };
            out.push(Token { start: at[i], end: at[end], kind });
            i = end;
        }
    }
    out
}

/// One thing the text at the caret could become.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    /// What goes into the text: a function brings its opening bracket.
    pub insert: String,
    pub kind: TokenKind,
}

/// What can replace the word at the caret, best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completions {
    /// The word being replaced, as byte offsets.
    pub start: usize,
    pub end: usize,
    /// The part of it before the caret, which the items were matched against.
    pub prefix: String,
    pub items: Vec<Completion>,
}

impl Completions {
    /// The text with an item chosen, and the byte offset the caret goes to.
    pub fn apply(&self, src: &str, item: usize) -> (String, usize) {
        let insert = self.items.get(item).map_or("", |c| c.insert.as_str());
        (format!("{}{insert}{}", &src[..self.start], &src[self.end..]), self.start + insert.len())
    }
}

/// What fits at byte offset `cursor` of an expression, given the variables it may use.
/// After a number that's a unit; anywhere else a value can start, a variable first,
/// then a constant or a function. `None` where nothing fits, or nothing is left to add.
pub fn complete(src: &str, cursor: usize, vars: &[&str]) -> Option<Completions> {
    if cursor > src.len() || !src.is_char_boundary(cursor) {
        return None;
    }
    let toks = tokens(src, |_| true);
    let is_word = |t: &Token| src[t.start..].chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
    let here = toks.iter().position(|t| t.start <= cursor && cursor <= t.end && t.kind != TokenKind::Number && is_word(t));
    let (start, end, after_number) = match here {
        Some(k) => (toks[k].start, toks[k].end, k > 0 && toks[k - 1].kind == TokenKind::Number),
        None => {
            // No word at the caret: only where a value can begin, not after one.
            let before = src[..cursor].trim_end();
            if !before.is_empty() && !before.ends_with(['+', '-', '*', '/', '^', '%', '(', ',', '<', '>', '=']) {
                return None;
            }
            (cursor, cursor, false)
        }
    };
    let prefix = &src[start..cursor];
    if after_number && prefix.is_empty() {
        return None;
    }
    let lower = prefix.to_lowercase();
    let mut found: Vec<(u8, Completion)> = Vec::new();
    let mut offer = |label: &str, insert: &str, kind: TokenKind| {
        let name = label.to_lowercase();
        let rank = if label == prefix {
            0
        } else if label.starts_with(prefix) {
            1
        } else if name.starts_with(&lower) {
            2
        } else if !after_number && name.contains(&lower) {
            3
        } else {
            return;
        };
        found.push((rank, Completion { label: label.to_string(), insert: insert.to_string(), kind }));
    };
    if after_number {
        for u in UNITS.split(", ") {
            offer(u, u, TokenKind::Unit);
        }
    } else {
        for v in vars {
            offer(v, v, TokenKind::Variable);
        }
        for c in CONSTANTS.split(", ").filter(|c| !vars.contains(c)) {
            offer(c, c, TokenKind::Constant);
        }
        // Every function on the first keystroke would bury the variables.
        if !prefix.is_empty() {
            let bracket_follows = src[end..].trim_start().starts_with('(');
            for f in FUNCTIONS {
                let insert = if bracket_follows { f.to_string() } else { format!("{f}(") };
                offer(f, &insert, TokenKind::Function);
            }
        }
    }
    found.sort_by_key(|x| x.0);
    let items: Vec<Completion> = found.into_iter().map(|x| x.1).collect();
    if items.is_empty() || (items.len() == 1 && items[0].label == src[start..end]) {
        return None;
    }
    Some(Completions { start, end, prefix: prefix.to_string(), items })
}

/// The rows that use `name`, directly or through other rows. A row can't use any of
/// them without going round in a circle.
pub fn dependents(params: &[(String, String)], name: &str) -> BTreeSet<String> {
    let uses: Vec<(&str, Vec<String>)> = params.iter().map(|(n, e)| (n.as_str(), names(e).unwrap_or_default())).collect();
    let mut out = BTreeSet::new();
    let mut frontier = vec![name.to_string()];
    while let Some(x) = frontier.pop() {
        for (n, used) in &uses {
            if used.contains(&x) && out.insert(n.to_string()) {
                frontier.push(n.to_string());
            }
        }
    }
    out
}

/// Identifiers an expression references, without functions and constants, in
/// order of first appearance.
pub fn names(src: &str) -> Result<Vec<String>> {
    let node = parse(src)?;
    let mut all = Vec::new();
    identifiers(&node, &mut all);
    all.retain(|n| constant(n).is_none());
    Ok(all)
}

/// Evaluates `(name, expression)` pairs in dependency order on top of `base`.
/// A pair overrides a `base` entry of the same name.
pub fn resolve(params: &[(String, String)], base: &Vars) -> Result<Vars> {
    let mut defs: HashMap<&str, (Node, &str)> = HashMap::new();
    for (name, src) in params {
        let node = parse(src).map_err(|e| anyhow!("variable '{name}': {e}"))?;
        if defs.insert(name, (node, src)).is_some() {
            bail!("variable '{name}' is defined twice");
        }
    }
    let mut resolver = Resolver { defs: &defs, base, out: base.clone(), done: HashSet::new(), path: Vec::new() };
    for (name, _) in params {
        resolver.visit(name)?;
    }
    Ok(resolver.out)
}

struct Resolver<'a> {
    defs: &'a HashMap<&'a str, (Node, &'a str)>,
    base: &'a Vars,
    out: Vars,
    done: HashSet<&'a str>,
    path: Vec<&'a str>,
}

impl<'a> Resolver<'a> {
    fn visit(&mut self, name: &str) -> Result<()> {
        let Some((key, (node, src))) = self.defs.get_key_value(name) else {
            return Ok(());
        };
        let key: &'a str = key;
        if self.done.contains(key) {
            return Ok(());
        }
        if let Some(at) = self.path.iter().position(|p| *p == key) {
            let mut chain: Vec<&str> = self.path[at..].to_vec();
            chain.push(key);
            bail!("circular definition of variables: {}", chain.join(" -> "));
        }
        self.path.push(key);
        let mut deps = Vec::new();
        identifiers(node, &mut deps);
        for dep in &deps {
            if self.defs.contains_key(dep.as_str()) {
                self.visit(dep)?;
            } else if !self.base.contains_key(dep) && constant(dep).is_none() {
                bail!("variable '{key}' refers to '{dep}', which is not defined");
            }
        }
        let value = eval_node(node, &self.out).map_err(|e| anyhow!("variable '{key}' = \"{src}\": {e}"))?;
        if !value.is_finite() {
            bail!("variable '{key}' = \"{src}\" does not evaluate to a finite number");
        }
        self.out.insert(key.to_string(), value);
        self.path.pop();
        self.done.insert(key);
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum Node {
    Num(f64),
    Var(String),
    Neg(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Sym(&'static str),
    End,
}

fn constant(name: &str) -> Option<f64> {
    match name {
        "pi" => Some(PI),
        "e" => Some(E),
        "c0" => Some(C0),
        "eta0" => Some(ETA0),
        _ => None,
    }
}

fn unit(name: &str) -> Option<f64> {
    Some(match name {
        "mm" => 1e-3,
        "cm" => 1e-2,
        "m" => 1.0,
        "um" | "µm" => 1e-6,
        "nm" => 1e-9,
        "mil" => 25.4e-6,
        "in" => 25.4e-3,
        "Hz" | "hz" => 1.0,
        "kHz" | "khz" => 1e3,
        "MHz" | "mhz" => 1e6,
        "GHz" | "ghz" => 1e9,
        "THz" | "thz" => 1e12,
        "deg" => PI / 180.0,
        "rad" => 1.0,
        _ => return None,
    })
}

/// Accepted argument counts of a function.
fn arity(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "sqrt" | "exp" | "ln" | "log" | "log10"
        | "abs" | "floor" | "ceil" | "round" => (1, 1),
        "atan2" | "pow" => (2, 2),
        "min" | "max" => (2, usize::MAX),
        "if" => (3, 3),
        _ => return None,
    })
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let (value, next) = lex_number(&b, i)?;
            out.push((Tok::Num(value), i));
            i = next;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && is_ident_char(b[i]) {
                i += 1;
            }
            out.push((Tok::Ident(b[start..i].iter().collect()), start));
        } else {
            let pair = (c, b.get(i + 1).copied());
            let sym = match pair {
                ('<', Some('=')) => "<=",
                ('>', Some('=')) => ">=",
                ('=', Some('=')) => "==",
                ('!', Some('=')) => "!=",
                ('=', _) => bail!("'=' at position {} of \"{src}\": use == to compare", i + 1),
                ('+', _) => "+",
                ('-', _) => "-",
                ('*', _) => "*",
                ('/', _) => "/",
                ('^', _) => "^",
                ('%', _) => "%",
                ('(', _) => "(",
                (')', _) => ")",
                (',', _) => ",",
                ('<', _) => "<",
                ('>', _) => ">",
                _ => bail!("unexpected character '{c}' at position {} of \"{src}\"", i + 1),
            };
            out.push((Tok::Sym(sym), i));
            i += sym.len();
        }
    }
    out.push((Tok::End, b.len()));
    Ok(out)
}

/// Where a number starting at `start` ends, its exponent included.
fn number_end(b: &[char], start: usize) -> usize {
    let digits = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = digits(start);
    if b.get(i) == Some(&'.') {
        i = digits(i + 1);
    }
    if matches!(b.get(i), Some('e' | 'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some('+' | '-')) {
            j += 1;
        }
        if b.get(j).is_some_and(|c| c.is_ascii_digit()) {
            i = digits(j);
        }
    }
    i
}

/// A number with an optional exponent and unit suffix. The suffix may follow a
/// space when it is a known unit, since two adjacent values are otherwise illegal.
fn lex_number(b: &[char], start: usize) -> Result<(f64, usize)> {
    let i = number_end(b, start);
    let text: String = b[start..i].iter().collect();
    let value: f64 = text.parse().map_err(|_| anyhow!("'{text}' is not a valid number"))?;

    let mut j = i;
    while b.get(j).is_some_and(|c| c.is_whitespace()) {
        j += 1;
    }
    let mut end = j;
    while end < b.len() && is_ident_char(b[end]) {
        end += 1;
    }
    let suffix: String = b[j..end].iter().collect();
    if suffix.is_empty() {
        return Ok((value, i));
    }
    let factor = unit(&suffix);
    if j == i {
        let factor = factor.ok_or_else(|| anyhow!("unknown unit '{suffix}' after {text}; known units are {UNITS}"))?;
        return Ok((value * factor, end));
    }
    match factor {
        Some(factor) if b.get(end) != Some(&'(') => Ok((value * factor, end)),
        _ => Ok((value, i)),
    }
}

fn parse(src: &str) -> Result<Node> {
    let toks = lex(src)?;
    let mut parser = Parser { toks: &toks, pos: 0, depth: 0, src };
    let node = parser.comparison()?;
    if *parser.peek() != Tok::End {
        return Err(parser.unexpected("an operator"));
    }
    Ok(node)
}

struct Parser<'a> {
    toks: &'a [(Tok, usize)],
    pos: usize,
    depth: usize,
    src: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].0
    }

    fn take_sym(&mut self, syms: &[&'static str]) -> Option<&'static str> {
        if let Tok::Sym(s) = self.peek() {
            let s = *s;
            if syms.contains(&s) {
                self.pos += 1;
                return Some(s);
            }
        }
        None
    }

    fn unexpected(&self, wanted: &str) -> anyhow::Error {
        let (tok, at) = &self.toks[self.pos];
        let found = match tok {
            Tok::Num(v) => format!("number {v}"),
            Tok::Ident(s) => format!("'{s}'"),
            Tok::Sym(s) => format!("'{s}'"),
            Tok::End => "the end of the expression".to_string(),
        };
        anyhow!("expected {wanted} but found {found} at position {} of \"{}\"", at + 1, self.src)
    }

    fn comparison(&mut self) -> Result<Node> {
        let mut left = self.additive()?;
        while let Some(sym) = self.take_sym(&["==", "!=", "<=", ">=", "<", ">"]) {
            let op = match sym {
                "==" => Op::Eq,
                "!=" => Op::Ne,
                "<=" => Op::Le,
                ">=" => Op::Ge,
                "<" => Op::Lt,
                _ => Op::Gt,
            };
            let right = self.additive()?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Node> {
        let mut left = self.term()?;
        while let Some(sym) = self.take_sym(&["+", "-"]) {
            let op = if sym == "+" { Op::Add } else { Op::Sub };
            let right = self.term()?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Node> {
        let mut left = self.unary()?;
        while let Some(sym) = self.take_sym(&["*", "/", "%"]) {
            let op = match sym {
                "*" => Op::Mul,
                "/" => Op::Div,
                _ => Op::Rem,
            };
            let right = self.unary()?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `-2^2` is −4, and `2^-1` is ½: the exponent re-enters here.
    fn unary(&mut self) -> Result<Node> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            bail!("\"{}\" is nested too deeply", self.src);
        }
        let node = if self.take_sym(&["-"]).is_some() {
            Node::Neg(Box::new(self.unary()?))
        } else if self.take_sym(&["+"]).is_some() {
            self.unary()?
        } else {
            let base = self.primary()?;
            if self.take_sym(&["^"]).is_some() {
                let exponent = self.unary()?;
                Node::Bin(Op::Pow, Box::new(base), Box::new(exponent))
            } else {
                base
            }
        };
        self.depth -= 1;
        Ok(node)
    }

    fn primary(&mut self) -> Result<Node> {
        match self.peek().clone() {
            Tok::Num(v) => {
                self.pos += 1;
                Ok(Node::Num(v))
            }
            Tok::Ident(name) => {
                self.pos += 1;
                if self.take_sym(&["("]).is_some() { self.call(name) } else { Ok(Node::Var(name)) }
            }
            Tok::Sym("(") => {
                self.pos += 1;
                let inner = self.comparison()?;
                if self.take_sym(&[")"]).is_none() {
                    return Err(self.unexpected("')'"));
                }
                Ok(inner)
            }
            _ => Err(self.unexpected("a number, a name or '('")),
        }
    }

    fn call(&mut self, name: String) -> Result<Node> {
        let (min, max) = arity(&name).ok_or_else(|| anyhow!("unknown function '{name}' in \"{}\"", self.src))?;
        let mut args = Vec::new();
        if self.take_sym(&[")"]).is_none() {
            loop {
                args.push(self.comparison()?);
                if self.take_sym(&[")"]).is_some() {
                    break;
                }
                if self.take_sym(&[","]).is_none() {
                    return Err(self.unexpected("',' or ')'"));
                }
            }
        }
        if args.len() < min || args.len() > max {
            let wanted = if min == max { min.to_string() } else { format!("at least {min}") };
            bail!("{name}() takes {wanted} argument(s), got {} in \"{}\"", args.len(), self.src);
        }
        Ok(Node::Call(name, args))
    }
}

fn identifiers(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Num(_) => {}
        Node::Var(name) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        Node::Neg(a) => identifiers(a, out),
        Node::Bin(_, a, b) => {
            identifiers(a, out);
            identifiers(b, out);
        }
        Node::Call(_, args) => args.iter().for_each(|a| identifiers(a, out)),
    }
}

fn eval_node(node: &Node, vars: &Vars) -> Result<f64> {
    match node {
        Node::Num(v) => Ok(*v),
        Node::Var(name) => vars
            .get(name)
            .copied()
            .or_else(|| constant(name))
            .ok_or_else(|| anyhow!("unknown name '{name}': it is neither a variable nor a constant ({CONSTANTS})")),
        Node::Neg(a) => Ok(-eval_node(a, vars)?),
        Node::Bin(op, a, b) => {
            let (x, y) = (eval_node(a, vars)?, eval_node(b, vars)?);
            let truth = |t: bool| if t { 1.0 } else { 0.0 };
            Ok(match op {
                Op::Add => x + y,
                Op::Sub => x - y,
                Op::Mul => x * y,
                Op::Div => {
                    if y == 0.0 {
                        bail!("division by zero");
                    }
                    x / y
                }
                Op::Rem => {
                    if y == 0.0 {
                        bail!("modulo by zero");
                    }
                    x % y
                }
                Op::Pow => {
                    let v = x.powf(y);
                    if !v.is_finite() {
                        bail!("{x}^{y} is not a finite real number");
                    }
                    v
                }
                Op::Eq => truth(x == y),
                Op::Ne => truth(x != y),
                Op::Lt => truth(x < y),
                Op::Le => truth(x <= y),
                Op::Gt => truth(x > y),
                Op::Ge => truth(x >= y),
            })
        }
        Node::Call(name, args) => eval_call(name, args, vars),
    }
}

fn eval_call(name: &str, args: &[Node], vars: &Vars) -> Result<f64> {
    if name == "if" {
        let branch = if eval_node(&args[0], vars)? != 0.0 { &args[1] } else { &args[2] };
        return eval_node(branch, vars);
    }
    let a: Vec<f64> = args.iter().map(|n| eval_node(n, vars)).collect::<Result<_>>()?;
    let x = a[0];
    let v = match name {
        "sin" => x.sin(),
        "cos" => x.cos(),
        "tan" => x.tan(),
        "asin" => x.asin(),
        "acos" => x.acos(),
        "atan" => x.atan(),
        "atan2" => x.atan2(a[1]),
        "sinh" => x.sinh(),
        "cosh" => x.cosh(),
        "tanh" => x.tanh(),
        "sqrt" => x.sqrt(),
        "exp" => x.exp(),
        "ln" | "log" => x.ln(),
        "log10" => x.log10(),
        "abs" => x.abs(),
        "pow" => x.powf(a[1]),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "round" => x.round(),
        "min" => a.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => a.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        _ => bail!("unknown function '{name}'"),
    };
    if !v.is_finite() {
        let shown: Vec<String> = a.iter().map(f64::to_string).collect();
        bail!("{name}({}) is not a finite real number", shown.join(", "));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str, known: &[&str]) -> Vec<(String, TokenKind)> {
        tokens(src, |n| known.contains(&n)).iter().map(|t| (src[t.start..t.end].to_string(), t.kind)).collect()
    }

    fn labels(src: &str, cursor: usize, vars: &[&str]) -> Vec<String> {
        complete(src, cursor, vars).map_or(Vec::new(), |c| c.items.into_iter().map(|i| i.label).collect())
    }

    #[test]
    fn each_part_of_an_expression_gets_its_kind() {
        use TokenKind::*;
        let got = kinds("0.55*lambda0 + 5 mm - sqrt(pi)/2.4GHz", &["lambda0"]);
        let want = [("0.55", Number), ("*", Operator), ("lambda0", Variable), ("+", Operator), ("5", Number), ("mm", Unit), ("-", Operator), ("sqrt", Function), ("(", Operator), ("pi", Constant), (")", Operator), ("/", Operator), ("2.4", Number), ("GHz", Unit)];
        assert_eq!(got, want.map(|(s, k)| (s.to_string(), k)));
        // A name nobody defined, a unit nobody knows, a bad character, an unknown call.
        assert_eq!(kinds("lamda0", &["lambda0"])[0].1, Unknown);
        assert_eq!(kinds("5xy", &[])[1], ("xy".to_string(), Unknown));
        assert_eq!(kinds("$", &[])[0].1, Unknown);
        assert_eq!(kinds("foo(1)", &[])[0].1, Unknown);
        // Comparisons are one operator, and a variable shadows a constant, as in `eval`.
        assert_eq!(kinds("a>=b", &["a", "b"])[1], (">=".to_string(), Operator));
        assert_eq!(kinds("e", &["e"])[0].1, Variable);
        assert_eq!(kinds("e", &[])[0].1, Constant);
        assert_eq!(kinds("1e-3", &[]), [("1e-3".to_string(), Number)]);
    }

    /// What's coloured as fine has to be what `eval` takes, or the colours would lie.
    #[test]
    fn the_colours_agree_with_the_parser() {
        let vars: Vars = [("L".to_string(), 1.0), ("m".to_string(), 2.0), ("lambda0".to_string(), 3.0)].into();
        let known = |n: &str| vars.contains_key(n);
        for good in ["0.55*lambda0", "L/2", "5mm", "5 mm", "m*2", "2.4GHz+1 MHz", "1e-3", ".5", "sqrt(L)", "sqrt (L)", "if(L>0, 1, 2)", "pi*e/c0", "min(1,2,3)", "L==1", "L!=2", "30deg"] {
            assert!(eval(good, &vars).is_ok(), "{good}");
            let bad: Vec<_> = tokens(good, known).into_iter().filter(|t| t.kind == TokenKind::Unknown).collect();
            assert!(bad.is_empty(), "{good}: {bad:?}");
        }
        for broken in ["Lx", "5xy", "foo(1)", "$", "L=1", "L!"] {
            assert!(eval(broken, &vars).is_err(), "{broken}");
            assert!(tokens(broken, known).iter().any(|t| t.kind == TokenKind::Unknown), "{broken}");
        }
    }

    #[test]
    fn half_typed_text_never_breaks_anything() {
        let vars = ["L", "lambda0", "µ"];
        for src in ["0.55*lambda0+sqrt(L)/2", "5µm + µ", "if(L>=1, 2.4GHz, 5 mm)", "  (( 1e-", "1.2.3e+", "é$ç=!", ""] {
            let ends: Vec<usize> = src.char_indices().map(|x| x.0).chain([src.len()]).collect();
            for &n in &ends {
                let part = &src[..n];
                let mut last = 0;
                for t in tokens(part, |x| vars.contains(&x)) {
                    assert!(last <= t.start && t.start < t.end && t.end <= part.len(), "{part}: {t:?}");
                    assert!(part.is_char_boundary(t.start) && part.is_char_boundary(t.end), "{part}: {t:?}");
                    last = t.end;
                }
                // At every caret position, not only the end.
                for caret in (0..=part.len()).filter(|c| part.is_char_boundary(*c)) {
                    if let Some(c) = complete(part, caret, &vars) {
                        for k in 0..c.items.len() {
                            let (text, at) = c.apply(part, k);
                            assert!(text.is_char_boundary(at), "{part} at {caret}");
                        }
                    }
                }
            }
        }
        // A caret in the middle of a character is refused, not a panic.
        assert_eq!(complete("µ", 1, &vars), None);
        assert_eq!(complete("L", 9, &vars), None);
    }

    #[test]
    fn variables_are_offered_as_you_type() {
        let vars = ["f0", "lambda0", "L", "w", "Lg"];
        // The best match first: exact case, then any case, then anywhere in the name.
        assert_eq!(labels("la", 2, &vars), ["lambda0"]);
        assert_eq!(labels("L", 1, &vars)[..3], ["L", "Lg", "lambda0"]);
        // After an operator, in brackets, or in an empty field: every variable, then the
        // constants, and no wall of functions.
        let start = ["f0", "lambda0", "L", "w", "Lg", "pi", "e", "c0", "eta0"];
        assert_eq!(labels("0.55*", 5, &vars), start);
        assert_eq!(labels("", 0, &vars), start);
        assert_eq!(labels("sqrt(", 5, &vars), start);
        assert_eq!(labels("max(L, ", 7, &vars), start);
        // Nowhere a value can't start.
        assert!(labels("L ", 2, &vars).is_empty());
        assert!(labels("(L)", 3, &vars).is_empty());
        assert!(labels("2.5", 3, &vars).is_empty());
        // A finished word has nothing to add, and a stranger matches nothing.
        assert!(labels("lambda0", 7, &vars).is_empty());
        assert!(labels("zzz", 3, &vars).is_empty());
    }

    #[test]
    fn a_number_is_offered_units_not_variables() {
        let vars = ["mass", "L"];
        assert_eq!(labels("5m", 2, &vars), ["m", "mm", "mil", "MHz"]);
        assert_eq!(labels("5 g", 3, &vars), ["GHz"]);
        assert_eq!(labels("2.4G", 4, &vars), ["GHz"]);
        assert!(labels("5", 1, &vars).is_empty());
        assert!(labels("1e", 2, &vars).is_empty(), "on the way to 1e-3");
        assert!(labels("5mm", 3, &vars).is_empty());
    }

    #[test]
    fn choosing_replaces_the_word_and_places_the_caret() {
        let vars = ["lambda0", "L"];
        let pick = |src: &str, caret: usize, want: &str| {
            let c = complete(src, caret, &vars).unwrap();
            let k = c.items.iter().position(|i| i.label == want).unwrap();
            c.apply(src, k)
        };
        assert_eq!(pick("0.55*la", 7, "lambda0"), ("0.55*lambda0".to_string(), 12));
        // The rest of the expression stays, and the whole word goes, not just its start.
        assert_eq!(pick("la+1", 2, "lambda0"), ("lambda0+1".to_string(), 7));
        assert_eq!(pick("lamzz+1", 3, "lambda0"), ("lambda0+1".to_string(), 7));
        // At the start of a word, in front of it.
        assert_eq!(pick("2*L", 2, "lambda0"), ("2*lambda0".to_string(), 9));
        // A function brings its bracket and leaves the caret inside, once only.
        assert_eq!(pick("sq", 2, "sqrt"), ("sqrt(".to_string(), 5));
        assert_eq!(pick("sq(L)", 2, "sqrt"), ("sqrt(L)".to_string(), 4));
        // A unit goes right after its number.
        assert_eq!(pick("5g", 2, "GHz"), ("5GHz".to_string(), 4));
        // Offsets are bytes, with something wider than a byte ahead of the caret.
        let c = complete("µ+la", 5, &vars).unwrap();
        assert_eq!(c.apply("µ+la", 0), ("µ+lambda0".to_string(), 10));
    }

    #[test]
    fn every_function_offered_is_one_the_parser_knows() {
        for f in FUNCTIONS {
            assert!(arity(f).is_some(), "{f}");
        }
    }

    #[test]
    fn a_row_can_only_use_what_doesnt_lead_back_to_it() {
        let rows: Vec<(String, String)> = [("f0", "1GHz"), ("lambda0", "c0/f0"), ("L", "0.5*lambda0"), ("g", "L/10"), ("w", "2mm"), ("broken", "L+")].map(|(a, b)| (a.to_string(), b.to_string())).into();
        let d = |n: &str| dependents(&rows, n).into_iter().collect::<Vec<_>>();
        assert_eq!(d("f0"), ["L", "g", "lambda0"]);
        assert_eq!(d("L"), ["g"]);
        assert!(d("w").is_empty());
        assert!(d("g").is_empty());
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "{a} vs {b}");
    }

    fn ev(src: &str) -> f64 {
        eval(src, &Vars::new()).unwrap()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn precedence_and_associativity() {
        close(ev("2+3*4"), 14.0);
        close(ev("(2+3)*4"), 20.0);
        close(ev("2^3^2"), 512.0);
        close(ev("-2^2"), -4.0);
        close(ev("2^-1"), 0.5);
        close(ev("10-3-2"), 5.0);
        close(ev("24/4/2"), 3.0);
        close(ev("7%3+1"), 2.0);
        close(ev("2*-3"), -6.0);
        close(ev("1+2 < 2*2"), 1.0);
    }

    #[test]
    fn typed_frequencies_land_on_rfid_band_edges() {
        let ghz = |s: &str| parse_ghz(s).unwrap_or(f64::NAN);
        close(ghz("915 MHz"), 0.915);
        close(ghz("865.7MHz"), 0.8657);
        close(ghz("2.45GHz"), 2.45);
        close(ghz("13.56 mhz"), 0.01356);
        close(ghz("0.902"), 0.902);
        close(ghz(" 928 "), 0.928);
        close(ghz("6.5"), 6.5);
        close(ghz("150 GHz"), 150.0);
        for bad in ["", "abc", "5mm", "12 parsecs", "nan", "inf"] {
            assert_eq!(parse_ghz(bad), None, "{bad}");
        }
    }

    #[test]
    fn units_are_converted_to_si() {
        close(ev("5mm + 2mil"), 5e-3 + 2.0 * 25.4e-6);
        close(ev("2.4GHz"), 2.4e9);
        close(ev("1in"), 0.0254);
        close(ev("10um"), 1e-5);
        close(ev("100nm"), 1e-7);
        close(ev("3cm"), 0.03);
        close(ev("2m"), 2.0);
        close(ev("1THz/1MHz"), 1e6);
        close(ev("5kHz"), 5000.0);
        close(ev("1Hz"), 1.0);
        close(ev("180deg"), PI);
        close(ev("1.5e3MHz"), 1.5e9);
        close(ev("2e-3"), 0.002);
        close(ev(".5"), 0.5);
        close(ev("5 mm"), 5e-3);
    }

    #[test]
    fn functions_and_constants() {
        close(ev("sin(pi/2)"), 1.0);
        close(ev("cos(0)"), 1.0);
        close(ev("tan(0)"), 0.0);
        close(ev("asin(1)"), PI / 2.0);
        close(ev("acos(1)"), 0.0);
        close(ev("atan(1)"), PI / 4.0);
        close(ev("atan2(1, 1)"), PI / 4.0);
        close(ev("sinh(0)+cosh(0)+tanh(0)"), 1.0);
        close(ev("sqrt(16)"), 4.0);
        close(ev("exp(1)"), E);
        close(ev("ln(e)"), 1.0);
        close(ev("log(e)"), 1.0);
        close(ev("log10(1000)"), 3.0);
        close(ev("abs(-3)"), 3.0);
        close(ev("min(3, 1, 2)"), 1.0);
        close(ev("max(3, 1, 2)"), 3.0);
        close(ev("pow(2, 10)"), 1024.0);
        close(ev("floor(2.7)+ceil(2.1)+round(2.5)"), 8.0);
        close(ev("c0/1GHz"), C0 / 1e9);
        close(ev("eta0"), ETA0);
    }

    #[test]
    fn variable_shadows_constant() {
        let mut vars = Vars::new();
        vars.insert("e".to_string(), 2.0);
        vars.insert("L".to_string(), 3.0);
        close(eval("e*L", &vars).unwrap(), 6.0);
        close(eval("1e1", &vars).unwrap(), 10.0);
    }

    #[test]
    fn comparison_and_lazy_if() {
        close(ev("if(3>2, 10, 20)"), 10.0);
        close(ev("if(3<=2, 10, 20)"), 20.0);
        close(ev("if(0, 1/0, 5)"), 5.0);
        close(ev("2==2"), 1.0);
        close(ev("2!=2"), 0.0);
        close(ev("2>=3"), 0.0);
    }

    #[test]
    fn useful_errors() {
        let msg = |src: &str| eval(src, &Vars::new()).unwrap_err().to_string();
        assert!(msg("a+1").contains("unknown name 'a'"), "{}", msg("a+1"));
        assert!(msg("1/0").contains("division by zero"), "{}", msg("1/0"));
        assert!(msg("1/(2-2)").contains("1/(2-2)"), "{}", msg("1/(2-2)"));
        assert!(msg("3%0").contains("modulo by zero"));
        assert!(msg("sqrt(-1)").contains("sqrt(-1)"), "{}", msg("sqrt(-1)"));
        assert!(msg("foo(1)").contains("unknown function 'foo'"));
        assert!(msg("sin(1,2)").contains("takes 1 argument"));
        assert!(msg("5xyz").contains("unknown unit 'xyz'"));
        assert!(msg("(1+2").contains("expected ')'"), "{}", msg("(1+2"));
        assert!(msg("1 2").contains("expected an operator"), "{}", msg("1 2"));
        assert!(msg("1+").contains("the end of the expression"));
        assert!(msg("a=1").contains("use =="));
        assert!(msg("(-8)^0.5").contains("not a finite"), "{}", msg("(-8)^0.5"));
        let deep = "(".repeat(5000);
        assert!(eval(&deep, &Vars::new()).is_err());
    }

    #[test]
    fn names_excludes_functions_and_constants() {
        assert_eq!(names("sin(a)+b*pi+a+c0-e").unwrap(), vec!["a", "b"]);
        assert_eq!(names("5mm+2").unwrap(), Vec::<String>::new());
        assert!(names("1+").is_err());
    }

    #[test]
    fn resolve_orders_dependencies() {
        let mut base = Vars::new();
        base.insert("h".to_string(), 2.0);
        let params = pairs(&[("area", "w*len"), ("w", "len/2"), ("len", "h*5mm")]);
        let out = resolve(&params, &base).unwrap();
        close(out["len"], 0.01);
        close(out["w"], 0.005);
        close(out["area"], 5e-5);
        close(out["h"], 2.0);
    }

    #[test]
    fn resolve_overrides_base_and_shadows_constants() {
        let mut base = Vars::new();
        base.insert("a".to_string(), 1.0);
        let params = pairs(&[("b", "a+1"), ("a", "10"), ("e", "3"), ("d", "e*2")]);
        let out = resolve(&params, &base).unwrap();
        close(out["a"], 10.0);
        close(out["b"], 11.0);
        close(out["d"], 6.0);
    }

    #[test]
    fn resolve_reports_problems_by_name() {
        let base = Vars::new();
        let cycle = resolve(&pairs(&[("x", "1"), ("a", "b+1"), ("b", "c*2"), ("c", "a+x")]), &base).unwrap_err().to_string();
        assert!(cycle.contains("circular"), "{cycle}");
        assert!(cycle.contains("a -> b -> c -> a"), "{cycle}");
        let own = resolve(&pairs(&[("a", "a+1")]), &base).unwrap_err().to_string();
        assert!(own.contains("a -> a"), "{own}");
        let unknown = resolve(&pairs(&[("a", "missing*2")]), &base).unwrap_err().to_string();
        assert!(unknown.contains("'a'") && unknown.contains("'missing'"), "{unknown}");
        let twice = resolve(&pairs(&[("a", "1"), ("a", "2")]), &base).unwrap_err().to_string();
        assert!(twice.contains("defined twice"), "{twice}");
        let syntax = resolve(&pairs(&[("a", "1+")]), &base).unwrap_err().to_string();
        assert!(syntax.contains("variable 'a'"), "{syntax}");
        let div = resolve(&pairs(&[("z", "0"), ("a", "1/z")]), &base).unwrap_err().to_string();
        assert!(div.contains("variable 'a'") && div.contains("division by zero"), "{div}");
    }
}
