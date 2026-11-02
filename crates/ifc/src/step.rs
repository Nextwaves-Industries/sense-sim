//! ISO 10303-21 ("STEP physical file") reader.
//!
//! The file is scanned once into a compact index of `(id, type, byte range)`
//! records, and an entity's attributes are only tokenised when something asks
//! for them. A file of hundreds of megabytes costs 24 bytes per entity on top
//! of its own text, and lookups are a binary search over the sorted index.

use anyhow::{Result, bail};
use std::borrow::Cow;
use std::collections::HashMap;

/// Deepest list nesting accepted. Real IFC needs about 4; the limit exists so a
/// hostile `((((((...` cannot overflow the stack.
const MAX_NESTING: usize = 64;
/// How many malformed statements are described individually.
const MAX_PROBLEMS_KEPT: usize = 50;

/// One tokenised attribute value. Strings keep their raw text; see [`decode_string`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value<'a> {
    /// `$`
    Null,
    /// `*`
    Derived,
    Int(i64),
    Real(f64),
    /// `'...'`, raw and undecoded.
    Str(&'a str),
    /// `"..."`
    Bin(&'a str),
    /// `.NAME.`
    Enum(&'a str),
    /// `#123`
    Ref(u64),
    /// `IFCLENGTHMEASURE(0.2)`
    Typed(&'a str, Vec<Value<'a>>),
    List(Vec<Value<'a>>),
}

impl<'a> Value<'a> {
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Value::Null | Value::Derived)
    }

    pub(crate) fn as_ref_id(&self) -> Option<u64> {
        match self {
            Value::Ref(id) => Some(*id),
            _ => None,
        }
    }

    /// The number held here, looking through typed wrappers. Non-finite numbers
    /// (`1E999`) read as absent so they cannot reach the geometry.
    pub(crate) fn as_f64(&self) -> Option<f64> {
        let number = match self {
            Value::Int(i) => *i as f64,
            Value::Real(r) => *r,
            Value::Typed(_, inner) => return inner.first()?.as_f64(),
            _ => return None,
        };
        number.is_finite().then_some(number)
    }

    /// The decoded text held here, looking through typed wrappers like `IFCLABEL('x')`.
    pub(crate) fn as_text(&self) -> Option<String> {
        match self {
            Value::Str(raw) => Some(decode_string(raw).into_owned()),
            Value::Typed(_, inner) => inner.first()?.as_text(),
            _ => None,
        }
    }

    pub(crate) fn as_enum(&self) -> Option<&'a str> {
        match self {
            Value::Enum(name) => Some(name),
            Value::Typed(_, inner) => inner.first()?.as_enum(),
            _ => None,
        }
    }

    pub(crate) fn is_enum(&self, name: &str) -> bool {
        self.as_enum().is_some_and(|e| e.eq_ignore_ascii_case(name))
    }

    pub(crate) fn as_list(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// Every reference in a list (or the single reference, when a schema gives one).
    pub(crate) fn refs(&self) -> Vec<u64> {
        match self {
            Value::Ref(id) => vec![*id],
            Value::List(items) => items.iter().filter_map(Value::as_ref_id).collect(),
            _ => Vec::new(),
        }
    }
}

/// Attribute `i`, reading absent trailing attributes as `$` so that callers
/// never index out of bounds on short or truncated entities.
pub(crate) fn attr<'v, 'a>(args: &'v [Value<'a>], i: usize) -> &'v Value<'a> {
    args.get(i).unwrap_or(&Value::Null)
}

/// Decode a raw STEP string (the text between the quotes) to Unicode.
///
/// Handles `''`, `\\`, `\X2\...\X0\` (UTF-16 code units, so surrogate pairs too),
/// `\X4\...\X0\`, `\X\HH` and `\S\c`. `\P?\` page switches are accepted but `\S\`
/// always decodes as ISO 8859-1, because other pages would need tables and real
/// exporters use `\X2\` for anything outside Latin-1. An escape that does not
/// parse stays verbatim, because a stray backslash in a name is common and
/// harmless. Line breaks inside strings are removed, as the format ignores them.
pub(crate) fn decode_string(raw: &str) -> Cow<'_, str> {
    if !raw.bytes().any(|b| matches!(b, b'\\' | b'\'' | b'\r' | b'\n')) {
        return Cow::Borrowed(raw);
    }
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    let mut literal_from = 0;
    while i < bytes.len() {
        let (replacement, consumed): (Option<Cow<'_, str>>, usize) = match bytes[i] {
            b'\'' if bytes.get(i + 1) == Some(&b'\'') => (Some("'".into()), 2),
            b'\'' => (Some("'".into()), 1),
            b'\r' | b'\n' => (None, 1),
            b'\\' => match decode_escape(&bytes[i..]) {
                Some((text, consumed)) => (Some(text.into()), consumed),
                None => {
                    i += 1;
                    continue;
                }
            },
            _ => {
                i += 1;
                continue;
            }
        };
        out.push_str(raw.get(literal_from..i).unwrap_or(""));
        if let Some(text) = replacement {
            out.push_str(&text);
        }
        i += consumed;
        literal_from = i;
    }
    out.push_str(raw.get(literal_from..).unwrap_or(""));
    Cow::Owned(out)
}

/// Decode the escape at the start of `b` (which begins with a backslash).
/// Returns the decoded text and how many bytes it took.
fn decode_escape(b: &[u8]) -> Option<(String, usize)> {
    match b.get(1)? {
        b'\\' => Some(("\\".to_string(), 2)),
        b'S' if b.get(2) == Some(&b'\\') => {
            let c = *b.get(3)?;
            c.is_ascii().then(|| (char::from(c | 0x80).to_string(), 4))
        }
        b'P' if b.get(3) == Some(&b'\\') && b.get(2)?.is_ascii_uppercase() => Some((String::new(), 4)),
        b'X' => match (b.get(2)?, b.get(3)) {
            (b'\\', _) => {
                let value = hex_value(b.get(3..5)?)?;
                Some((char::from(value as u8).to_string(), 5))
            }
            (b'2', Some(b'\\')) => decode_wide(b, 4),
            (b'4', Some(b'\\')) => decode_wide(b, 8),
            _ => None,
        },
        _ => None,
    }
}

/// `\X2\` or `\X4\` runs: hex groups of `width` digits ending in `\X0\`.
fn decode_wide(b: &[u8], width: usize) -> Option<(String, usize)> {
    let mut i = 4;
    let mut units16 = Vec::new();
    let mut out = String::new();
    loop {
        if b.get(i..i + 4)? == b"\\X0\\" {
            i += 4;
            break;
        }
        let value = hex_value(b.get(i..i + width)?)?;
        if width == 4 {
            units16.push(u16::try_from(value).ok()?);
        } else {
            out.push(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
        }
        i += width;
    }
    if width == 4 {
        out = char::decode_utf16(units16)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    Some((out, i))
}

fn hex_value(digits: &[u8]) -> Option<u32> {
    digits.iter().try_fold(0u32, |acc, &d| {
        let digit = char::from(d).to_digit(16)?;
        acc.checked_mul(16)?.checked_add(digit)
    })
}

/// Index entry for one `#id=TYPE(...);` instance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rec {
    pub id: u64,
    /// Byte offset of the opening parenthesis.
    start: usize,
    /// Bytes from `start` up to the terminating semicolon.
    len: u32,
    /// Index into [`Step::type_names`].
    pub ty: u32,
}

pub(crate) struct Step<'a> {
    text: &'a str,
    /// Sorted by id, unique.
    recs: Vec<Rec>,
    type_names: Vec<Cow<'a, str>>,
    /// Descriptions of the first few malformed statements.
    problems: Vec<String>,
    problem_count: usize,
    duplicates: usize,
}

impl<'a> Step<'a> {
    /// Index a whole file. Fails only when the text is not a STEP file of an IFC
    /// schema at all; a malformed statement is noted and skipped.
    pub(crate) fn parse(text: &'a str) -> Result<Self> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let b = text.as_bytes();
        let mut step = Step {
            text,
            recs: Vec::new(),
            type_names: Vec::new(),
            problems: Vec::new(),
            problem_count: 0,
            duplicates: 0,
        };
        let mut interned: HashMap<Cow<'a, str>, u32> = HashMap::new();
        let mut schemas: Vec<String> = Vec::new();
        let mut saw_data = false;
        let last_close = text.rfind("*/");
        let mut i = 0usize;
        loop {
            i = skip_gap(b, i, last_close);
            if i >= b.len() {
                break;
            }
            let statement = i;
            let head = if b[i] == b'#' { instance_head(text, i) } else { None };
            let scan_from = match &head {
                Some(h) => h.args_at,
                None => i,
            };
            let keyword_end = if head.is_none() { keyword(b, i) } else { i };
            let end = match scan_statement(b, scan_from, last_close) {
                Scan::End(end) => end,
                Scan::Broken(resume) => {
                    step.problem(statement, "unterminated string or missing ';'");
                    i = resume;
                    continue;
                }
                Scan::Eof => {
                    step.problem(statement, "file ends inside this statement");
                    break;
                }
            };
            i = end + 1;
            if let Some(head) = head {
                if b.get(head.args_at) != Some(&b'(') {
                    step.problem(statement, "expected '(' after the entity type");
                    continue;
                }
                let Ok(len) = u32::try_from(end - head.args_at) else {
                    step.problem(statement, "entity larger than 4 GB");
                    continue;
                };
                let ty = step.intern(&mut interned, head.type_name);
                step.recs.push(Rec { id: head.id, start: head.args_at, len, ty });
                continue;
            }
            let word = text.get(statement..keyword_end).unwrap_or("");
            if word.eq_ignore_ascii_case("DATA") {
                saw_data = true;
            } else if word.eq_ignore_ascii_case("FILE_SCHEMA") {
                let mut cursor = Cursor { text: text.get(..end).unwrap_or(""), pos: keyword_end };
                cursor.skip_trivia();
                if cursor.peek() == Some(b'(')
                    && let Ok(values) = cursor.list(0)
                {
                    collect_strings(&values, &mut schemas);
                }
            } else if word.eq_ignore_ascii_case("END-ISO-10303-21") {
                break;
            } else if word.is_empty() {
                step.problem(statement, "not a valid entity instance");
            }
        }
        if !saw_data {
            bail!("not an IFC STEP file: no DATA section found");
        }
        if !schemas.is_empty() && !schemas.iter().any(|s| s.to_ascii_uppercase().starts_with("IFC")) {
            bail!("not an IFC file: FILE_SCHEMA is {}", schemas.join(", "));
        }
        step.finish_index();
        Ok(step)
    }

    fn intern(&mut self, table: &mut HashMap<Cow<'a, str>, u32>, name: &'a str) -> u32 {
        let name: Cow<'a, str> = if name.bytes().any(|c| c.is_ascii_lowercase()) {
            Cow::Owned(name.to_ascii_uppercase())
        } else {
            Cow::Borrowed(name)
        };
        if let Some(&id) = table.get(&*name) {
            return id;
        }
        let id = self.type_names.len() as u32;
        self.type_names.push(name.clone());
        table.insert(name, id);
        id
    }

    fn problem(&mut self, at: usize, what: &str) {
        self.problem_count += 1;
        if self.problems.len() < MAX_PROBLEMS_KEPT {
            self.problems.push(format!("byte {at}: {what}"));
        }
    }

    /// Writers emit ascending ids, so the sort is usually just this check. On a
    /// duplicate id the first definition wins.
    fn finish_index(&mut self) {
        self.recs.shrink_to_fit();
        if !self.recs.is_sorted_by_key(|r| r.id) {
            self.recs.sort_by_key(|r| r.id);
        }
        let before = self.recs.len();
        self.recs.dedup_by_key(|r| r.id);
        self.duplicates = before - self.recs.len();
    }

    pub(crate) fn records(&self) -> &[Rec] {
        &self.recs
    }

    pub(crate) fn type_names(&self) -> &[Cow<'a, str>] {
        &self.type_names
    }

    pub(crate) fn problems(&self) -> &[String] {
        &self.problems
    }

    pub(crate) fn problem_count(&self) -> usize {
        self.problem_count
    }

    pub(crate) fn duplicates(&self) -> usize {
        self.duplicates
    }

    pub(crate) fn rec(&self, id: u64) -> Option<&Rec> {
        let at = self.recs.binary_search_by_key(&id, |r| r.id).ok()?;
        self.recs.get(at)
    }

    pub(crate) fn type_name(&self, rec: &Rec) -> &str {
        self.type_names.get(rec.ty as usize).map_or("", |n| n.as_ref())
    }

    /// The upper-case type of entity `id`, or `""` when it does not exist.
    pub(crate) fn type_of(&self, id: u64) -> &str {
        self.rec(id).map_or("", |r| self.type_name(r))
    }

    /// Tokenise an entity's attributes.
    pub(crate) fn args(&self, rec: &Rec) -> Result<Vec<Value<'a>>, String> {
        let end = rec.start.saturating_add(rec.len as usize);
        let text = self.text.get(..end).ok_or("entity range is outside the file")?;
        let mut cursor = Cursor { text, pos: rec.start };
        cursor.list(0)
    }
}

/// Collect every string in nested values (for `FILE_SCHEMA(('IFC4'))`).
fn collect_strings(values: &[Value<'_>], out: &mut Vec<String>) {
    for value in values {
        match value {
            Value::Str(raw) => out.push(decode_string(raw).into_owned()),
            Value::List(items) => collect_strings(items, out),
            _ => {}
        }
    }
}

struct Head<'a> {
    id: u64,
    type_name: &'a str,
    /// Where the attribute list should start.
    args_at: usize,
}

/// Parse `#123 = TYPE` at `i`. The type is empty for complex instances `#1=(A()B());`.
fn instance_head(text: &str, i: usize) -> Option<Head<'_>> {
    let b = text.as_bytes();
    let (id, after_id) = reference_at(b, i)?;
    let eq = skip_trivia(b, after_id);
    if b.get(eq) != Some(&b'=') {
        return None;
    }
    let type_start = skip_trivia(b, eq + 1);
    let mut type_end = type_start;
    while b.get(type_end).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
        type_end += 1;
    }
    Some(Head {
        id,
        type_name: text.get(type_start..type_end)?,
        args_at: skip_trivia(b, type_end),
    })
}

/// `#digits` at `i`: the id and the offset after it. An id too large for u64 is `None`.
fn reference_at(b: &[u8], i: usize) -> Option<(u64, usize)> {
    if b.get(i) != Some(&b'#') {
        return None;
    }
    let mut pos = i + 1;
    let mut id = 0u64;
    while let Some(digit) = b.get(pos).filter(|c| c.is_ascii_digit()) {
        id = id.checked_mul(10)?.checked_add(u64::from(digit - b'0'))?;
        pos += 1;
    }
    (pos > i + 1).then_some((id, pos))
}

/// End of a section keyword such as `HEADER` or `END-ISO-10303-21`.
fn keyword(b: &[u8], i: usize) -> usize {
    let mut end = i;
    while b.get(end).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')) {
        end += 1;
    }
    end
}

fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        while matches!(b.get(i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            i += 1;
        }
        if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'*') {
            i = comment_end(b, i + 2).unwrap_or(b.len());
        } else {
            return i;
        }
    }
}

/// True when a `/*` at `i` has a `*/` somewhere after it. Without one it is damage,
/// not a comment, and honouring it would discard the rest of the file.
fn comment_can_close(i: usize, last_close: Option<usize>) -> bool {
    last_close.is_some_and(|close| i + 2 <= close)
}

/// Like [`skip_trivia`] between statements, leaving a `/*` that never closes.
fn skip_gap(b: &[u8], mut i: usize, last_close: Option<usize>) -> usize {
    loop {
        while matches!(b.get(i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            i += 1;
        }
        if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'*') && comment_can_close(i, last_close) {
            i = comment_end(b, i + 2).unwrap_or(b.len());
        } else {
            return i;
        }
    }
}

/// Offset just past the `*/` closing a comment whose body starts at `i`.
fn comment_end(b: &[u8], mut i: usize) -> Option<usize> {
    while i + 1 < b.len() {
        if b[i] == b'*' && b[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

enum Scan {
    /// Offset of the terminating semicolon.
    End(usize),
    /// The statement never closed; resume at this next statement.
    Broken(usize),
    Eof,
}

/// A line starting `#123=` cannot be inside an entity. Seeing one mid-statement
/// means a quote or semicolon was lost, and stopping there confines the damage
/// to that one entity instead of inverting every string after it.
fn starts_instance(b: &[u8], mut i: usize) -> bool {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\r')) {
        i += 1;
    }
    if b.get(i) != Some(&b'#') {
        return false;
    }
    let Some((_, after)) = reference_at(b, i) else {
        return false;
    };
    let mut after = after;
    while matches!(b.get(after), Some(b' ' | b'\t')) {
        after += 1;
    }
    b.get(after) == Some(&b'=')
}

/// Find the `;` ending the statement, stepping over strings and comments.
fn scan_statement(b: &[u8], mut i: usize, last_close: Option<usize>) -> Scan {
    while let Some(&c) = b.get(i) {
        match c {
            b';' => return Scan::End(i),
            b'\n' if starts_instance(b, i + 1) => return Scan::Broken(i + 1),
            b'\'' => {
                i += 1;
                loop {
                    match b.get(i) {
                        None => return Scan::Eof,
                        Some(b'\'') if b.get(i + 1) == Some(&b'\'') => i += 2,
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(b'\n') if starts_instance(b, i + 1) => return Scan::Broken(i + 1),
                        Some(_) => i += 1,
                    }
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') && comment_can_close(i, last_close) => {
                i = comment_end(b, i + 2).unwrap_or(b.len());
            }
            _ => i += 1,
        }
    }
    Scan::Eof
}

/// Attribute tokeniser over a slice ending at the statement's semicolon.
struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_trivia(&mut self) {
        self.pos = skip_trivia(self.text.as_bytes(), self.pos);
    }

    /// A parenthesised list, with the cursor on its `(`.
    fn list(&mut self, depth: usize) -> Result<Vec<Value<'a>>, String> {
        if depth > MAX_NESTING {
            return Err(format!("lists nested deeper than {MAX_NESTING} levels"));
        }
        if self.peek() != Some(b'(') {
            return Err("expected '('".to_string());
        }
        self.pos += 1;
        let mut out = Vec::new();
        self.skip_trivia();
        if self.peek() == Some(b')') {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            out.push(self.value(depth + 1)?);
            self.skip_trivia();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(c) => return Err(format!("expected ',' or ')' but found '{}'", char::from(c))),
                None => return Err("list is not closed".to_string()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>, String> {
        self.skip_trivia();
        let b = self.text.as_bytes();
        let Some(c) = self.peek() else {
            return Err("unexpected end of entity".to_string());
        };
        match c {
            b'$' => {
                self.pos += 1;
                Ok(Value::Null)
            }
            b'*' => {
                self.pos += 1;
                Ok(Value::Derived)
            }
            b'#' => match reference_at(b, self.pos) {
                Some((id, after)) => {
                    self.pos = after;
                    Ok(Value::Ref(id))
                }
                None => Err("bad entity reference".to_string()),
            },
            b'\'' => self.string(),
            b'"' => {
                let start = self.pos + 1;
                let end = start + find_byte(b, start, b'"').ok_or("unterminated binary value")?;
                self.pos = end + 1;
                Ok(Value::Bin(self.slice(start, end)?))
            }
            b'(' => Ok(Value::List(self.list(depth)?)),
            b'.' if b.get(self.pos + 1).is_some_and(|n| n.is_ascii_alphabetic() || *n == b'_') => {
                let start = self.pos + 1;
                let end = start + find_byte(b, start, b'.').ok_or("unterminated enumeration")?;
                self.pos = end + 1;
                Ok(Value::Enum(self.slice(start, end)?))
            }
            b'0'..=b'9' | b'+' | b'-' | b'.' => self.number(),
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let start = self.pos;
                while self.peek().is_some_and(|n| n.is_ascii_alphanumeric() || n == b'_') {
                    self.pos += 1;
                }
                let name = self.slice(start, self.pos)?;
                self.skip_trivia();
                Ok(Value::Typed(name, self.list(depth)?))
            }
            other => Err(format!("unexpected character '{}'", char::from(other))),
        }
    }

    fn slice(&self, start: usize, end: usize) -> Result<&'a str, String> {
        self.text.get(start..end).ok_or_else(|| "bad token boundary".to_string())
    }

    fn string(&mut self) -> Result<Value<'a>, String> {
        let b = self.text.as_bytes();
        let start = self.pos + 1;
        let mut i = start;
        loop {
            match b.get(i) {
                None => return Err("unterminated string".to_string()),
                Some(b'\'') if b.get(i + 1) == Some(&b'\'') => i += 2,
                Some(b'\'') => {
                    self.pos = i + 1;
                    return Ok(Value::Str(self.slice(start, i)?));
                }
                Some(_) => i += 1,
            }
        }
    }

    fn number(&mut self) -> Result<Value<'a>, String> {
        let b = self.text.as_bytes();
        let start = self.pos;
        let mut i = start;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let mut real = false;
        if b.get(i) == Some(&b'.') {
            real = true;
            i += 1;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            let mut exp = i + 1;
            if matches!(b.get(exp), Some(b'+' | b'-')) {
                exp += 1;
            }
            if b.get(exp).is_some_and(u8::is_ascii_digit) {
                real = true;
                i = exp;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
        }
        let token = self.slice(start, i)?;
        self.pos = i;
        if !real && let Ok(int) = token.parse::<i64>() {
            return Ok(Value::Int(int));
        }
        // An integer too large for i64 is still a usable number.
        token
            .parse::<f64>()
            .map(Value::Real)
            .map_err(|_| format!("bad number '{token}'"))
    }
}

/// Offset of `needle` relative to `from`.
fn find_byte(b: &[u8], from: usize, needle: u8) -> Option<usize> {
    b.get(from..)?.iter().position(|&c| c == needle)
}
