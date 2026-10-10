//! Go `text/template` rendering for `--export-template` report templates.
//!
//! Legacy bv renders custom Markdown reports with Go's `text/template`
//! (`missingkey=error`, no custom functions). This module implements the
//! language those templates use, so existing templates keep working: text
//! and `{{ }}` actions with `{{-`/`-}}` trimming and `{{/* */}}` comments;
//! fields (`.Issues`, `.Title`), `.`, `$`, and variables (`$x := …`,
//! `$x = …`); string, number, `true`/`false`/`nil` literals; parenthesized
//! and `|` pipelines; `if`/`else if`/`else`, `range` (with `$i, $v :=` and
//! `else`), `with`/`else`; and the builtins `and`, `or`, `not`, `len`,
//! `index`, `slice`, `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `print`, `printf`,
//! `println`, `html`, `js`, and `urlquery`.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

/// Template data: what Go's reflection would see in the report struct.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// Go's `template.IsTrue`: false, zero, nil, and empty values are false.
    fn truthy(&self) -> bool {
        match self {
            Self::Nil => false,
            Self::Bool(b) => *b,
            Self::Int(n) => *n != 0,
            Self::Float(f) => *f != 0.0,
            Self::Str(s) => !s.is_empty(),
            Self::List(items) => !items.is_empty(),
            Self::Map(map) => !map.is_empty(),
        }
    }

    /// Go's `fmt` `%v` rendering.
    fn write_display(&self, out: &mut impl fmt::Write) -> fmt::Result {
        match self {
            Self::Nil => out.write_str("<no value>"),
            Self::Bool(b) => write!(out, "{b}"),
            Self::Int(n) => write!(out, "{n}"),
            Self::Float(f) => write!(out, "{f}"),
            Self::Str(s) => out.write_str(s),
            Self::List(items) => {
                out.write_char('[')?;
                for (i, item) in items.iter().enumerate() {
                    if i != 0 {
                        out.write_char(' ')?;
                    }
                    item.write_display(out)?;
                }
                out.write_char(']')
            }
            Self::Map(map) => {
                out.write_str("map[")?;
                for (i, (key, value)) in map.iter().enumerate() {
                    if i != 0 {
                        out.write_char(' ')?;
                    }
                    write!(out, "{key}:")?;
                    value.write_display(out)?;
                }
                out.write_char(']')
            }
        }
    }

    /// Charge every value, including empty strings and numeric collections.
    /// Counting only text lets aliases repeatedly clone arbitrarily large
    /// containers without consuming the render's allocation/work allowance.
    fn storage_bytes(&self) -> usize {
        let contents = match self {
            Self::Str(text) => text.len(),
            Self::List(items) => items.iter().fold(0usize, |total, value| {
                total.saturating_add(value.storage_bytes())
            }),
            Self::Map(map) => map.iter().fold(0usize, |total, (key, value)| {
                total
                    .saturating_add(std::mem::size_of::<String>())
                    .saturating_add(key.len())
                    .saturating_add(value.storage_bytes())
            }),
            _ => 0,
        };
        std::mem::size_of::<Self>().saturating_add(contents)
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float64",
            Self::Str(_) => "string",
            Self::List(_) => "slice",
            Self::Map(_) => "map",
        }
    }
}

/// Render `template` against `data`, as Go's `text/template` with
/// `missingkey=error` would. Output and individual computed strings are capped
/// at `max_output` bytes. Ranges share an iteration budget, including empty
/// bodies, and generated/copied values share a budget of eight times the output
/// limit (at least 1 MiB). This also bounds assignments whose values are never
/// printed. These budgets apply to the whole render, not each nested action.
/// Statement and parenthesized-expression nesting share a 128-level limit.
pub fn render(template: &str, data: &Value, max_output: usize) -> Result<String, String> {
    let tokens = lex(template)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let nodes = parser.parse_list(&[])?;
    if let Some(token) = parser.tokens.get(parser.pos) {
        return Err(format!("unexpected {}", token.describe()));
    }
    let mut out = TextBuffer::new(max_output);
    let max_value_bytes = max_output.saturating_mul(8).max(1 << 20);
    let root_bytes = data.storage_bytes();
    if root_bytes > max_value_bytes {
        return Err(format!(
            "export template evaluation exceeds {max_value_bytes} bytes of intermediate values"
        ));
    }
    let mut scope = Scope {
        vars: vec![("$".to_string(), data.clone())],
        max_output,
        iterations_left: MAX_RANGE_ITERATIONS,
        max_value_bytes,
        value_bytes_left: max_value_bytes - root_bytes,
    };
    exec_list(&nodes, data, &mut scope, &mut out)?;
    Ok(out.text)
}

/// Empty/nested ranges must terminate even when they never write output.
const MAX_RANGE_ITERATIONS: usize = 1_000_000;
const MAX_NESTING: usize = 128;

fn output_limit_error(limit: usize) -> String {
    format!("rendered export template exceeds {limit} bytes")
}

struct TextBuffer {
    text: String,
    limit: usize,
}

impl TextBuffer {
    const fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }

    fn remaining(&self) -> usize {
        self.limit - self.text.len()
    }

    fn write_padding(&mut self, byte: char, mut count: usize) -> fmt::Result {
        if count > self.remaining() {
            return Err(fmt::Error);
        }
        let chunk = if byte == '0' {
            "00000000000000000000000000000000"
        } else {
            "                                "
        };
        while count != 0 {
            let len = count.min(chunk.len());
            self.write_str(&chunk[..len])?;
            count -= len;
        }
        Ok(())
    }
}

impl fmt::Write for TextBuffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > self.remaining() {
            return Err(fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}

/// Precision truncates rendered Unicode characters, while the underlying
/// buffer still enforces its byte limit before every write.
struct PrecisionWriter<'a> {
    out: &'a mut TextBuffer,
    chars_left: usize,
}

impl fmt::Write for PrecisionWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut end = 0;
        for c in text.chars().take(self.chars_left) {
            end += c.len_utf8();
            self.chars_left -= 1;
        }
        self.out.write_str(&text[..end])
    }
}

// ---------------------------------------------------------------------------
// Lexing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Token {
    Text(String),
    /// The words of an action between `{{` and `}}`, already trimmed.
    Action {
        words: Vec<Word>,
        line: usize,
    },
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Text(_) => "text".to_string(),
            Self::Action { words, line } => format!(
                "{{{{{}}}}} on line {line}",
                words.first().map_or(String::new(), Word::describe)
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Word {
    Ident(String),
    /// `.Field.Sub` (empty path = `.`).
    Field(Vec<String>),
    /// `$name.Field` (`$` alone is the root).
    Var(String, Vec<String>),
    Str(String),
    Int(i64),
    Float(f64),
    LParen,
    RParen,
    Pipe,
    Declare,
    Assign,
    Comma,
}

impl Word {
    fn describe(&self) -> String {
        match self {
            Self::Ident(name) => name.clone(),
            Self::Field(path) => format!(".{}", path.join(".")),
            Self::Var(name, _) => name.clone(),
            Self::Str(s) => format!("{s:?}"),
            Self::Int(n) => n.to_string(),
            Self::Float(f) => f.to_string(),
            Self::LParen => "(".into(),
            Self::RParen => ")".into(),
            Self::Pipe => "|".into(),
            Self::Declare => ":=".into(),
            Self::Assign => "=".into(),
            Self::Comma => ",".into(),
        }
    }
}

fn lex(template: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut rest = template;
    let mut line = 1;
    while let Some(start) = rest.find("{{") {
        let mut text = &rest[..start];
        let after = &rest[start + 2..];
        let trim_left = after.starts_with("- ")
            || after.starts_with("-\t")
            || after.starts_with("-\n")
            || after.starts_with("-\r");
        if trim_left {
            text = text.trim_end();
        }
        if !text.is_empty() {
            tokens.push(Token::Text(text.to_string()));
        }
        line += rest[..start].matches('\n').count();
        let body_start = usize::from(trim_left);
        let Some(end) = find_action_end(&after[body_start..]) else {
            return Err(format!("unclosed action on line {line}"));
        };
        let raw_body = &after[body_start..body_start + end];
        let stripped = raw_body
            .strip_suffix(" -")
            .or_else(|| raw_body.strip_suffix("\t-"))
            .or_else(|| raw_body.strip_suffix("\n-"));
        let trim_right = stripped.is_some();
        let body = stripped.unwrap_or(raw_body);
        line += body.matches('\n').count();
        let trimmed = body.trim();
        if !(trimmed.starts_with("/*") && trimmed.ends_with("*/")) {
            tokens.push(Token::Action {
                words: lex_action(trimmed, line)?,
                line,
            });
        }
        rest = &after[body_start + end + 2..];
        if trim_right {
            rest = rest.trim_start();
        }
    }
    if !rest.is_empty() {
        tokens.push(Token::Text(rest.to_string()));
    }
    Ok(tokens)
}

/// Offset of the `}}` closing an action, skipping quoted strings and
/// comments.
fn find_action_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if quote == b'"' && bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if text[i..].starts_with("/*") => {
                i += text[i..].find("*/")? + 1;
            }
            b'}' if text[i..].starts_with("}}") => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn lex_action(body: &str, line: usize) -> Result<Vec<Word>, String> {
    let chars: Vec<char> = body.chars().collect();
    let mut words = Vec::new();
    let mut i = 0;
    let ident_char = |c: char| c.is_alphanumeric() || c == '_';
    let path = |chars: &[char], i: &mut usize| -> Vec<String> {
        let mut path = Vec::new();
        while *i < chars.len() && chars[*i] == '.' {
            *i += 1;
            let start = *i;
            while *i < chars.len() && ident_char(chars[*i]) {
                *i += 1;
            }
            if *i > start {
                path.push(chars[start..*i].iter().collect());
            }
        }
        path
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                words.push(Word::LParen);
                i += 1;
            }
            ')' => {
                words.push(Word::RParen);
                i += 1;
            }
            '|' => {
                words.push(Word::Pipe);
                i += 1;
            }
            ',' => {
                words.push(Word::Comma);
                i += 1;
            }
            ':' if chars.get(i + 1) == Some(&'=') => {
                words.push(Word::Declare);
                i += 2;
            }
            '=' => {
                words.push(Word::Assign);
                i += 1;
            }
            '.' => words.push(Word::Field(path(&chars, &mut i))),
            '$' => {
                let start = i;
                i += 1;
                while i < chars.len() && ident_char(chars[i]) {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                words.push(Word::Var(name, path(&chars, &mut i)));
            }
            '"' => {
                let mut value = String::new();
                i += 1;
                loop {
                    let Some(&ch) = chars.get(i) else {
                        return Err(format!("unterminated string on line {line}"));
                    };
                    i += 1;
                    match ch {
                        '"' => break,
                        '\\' => {
                            let escaped = chars.get(i).copied().unwrap_or('\\');
                            i += 1;
                            value.push(match escaped {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                other => other,
                            });
                        }
                        other => value.push(other),
                    }
                }
                words.push(Word::Str(value));
            }
            '`' => {
                let start = i + 1;
                let Some(len) = chars[start..].iter().position(|&ch| ch == '`') else {
                    return Err(format!("unterminated raw string on line {line}"));
                };
                words.push(Word::Str(chars[start..start + len].iter().collect()));
                i = start + len + 1;
            }
            c if c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) =>
            {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if text.contains('.') {
                    words.push(Word::Float(
                        text.parse()
                            .map_err(|_| format!("bad number {text} on line {line}"))?,
                    ));
                } else {
                    words.push(Word::Int(
                        text.parse()
                            .map_err(|_| format!("bad number {text} on line {line}"))?,
                    ));
                }
            }
            c if ident_char(c) => {
                let start = i;
                while i < chars.len() && ident_char(chars[i]) {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                words.push(Word::Ident(name));
            }
            other => return Err(format!("unexpected {other:?} in action on line {line}")),
        }
    }
    Ok(words)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Operand {
    Field(Vec<String>),
    Var(String, Vec<String>),
    Literal(Value),
    Function(String),
    Sub(Box<Pipeline>, Vec<String>),
}

#[derive(Debug, Clone)]
struct Pipeline {
    /// Variables declared (`:=`) or assigned (`=`) by this pipeline.
    decl: Vec<String>,
    declare: bool,
    commands: Vec<Vec<Operand>>,
    line: usize,
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Action(Pipeline),
    If(Vec<(Pipeline, Vec<Node>)>, Vec<Node>),
    Range(Pipeline, Vec<Node>, Vec<Node>),
    With(Pipeline, Vec<Node>, Vec<Node>),
}

const FUNCTIONS: &[&str] = &[
    "and", "or", "not", "len", "index", "slice", "eq", "ne", "lt", "le", "gt", "ge", "print",
    "printf", "println", "html", "js", "urlquery",
];

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    /// Parse nodes until an action whose keyword is in `stops` (left
    /// unconsumed).
    fn parse_list(&mut self, stops: &[&str]) -> Result<Vec<Node>, String> {
        if self.depth > MAX_NESTING {
            return Err(format!(
                "export template nesting exceeds {MAX_NESTING} levels"
            ));
        }
        self.depth += 1;
        let result = self.parse_list_body(stops);
        self.depth -= 1;
        result
    }

    fn parse_list_body(&mut self, stops: &[&str]) -> Result<Vec<Node>, String> {
        let mut nodes = Vec::new();
        while let Some(token) = self.tokens.get(self.pos).cloned() {
            match token {
                Token::Text(text) => {
                    self.pos += 1;
                    nodes.push(Node::Text(text));
                }
                Token::Action { words, line } => {
                    let keyword = match words.first() {
                        Some(Word::Ident(name)) => name.as_str(),
                        _ => "",
                    };
                    if stops.contains(&keyword) {
                        return Ok(nodes);
                    }
                    self.pos += 1;
                    match keyword {
                        "if" => nodes.push(self.parse_if(&words[1..], line)?),
                        "range" => {
                            let pipe = parse_pipeline(&words[1..], line, true, self.depth - 1)?;
                            let (body, alt) = self.parse_body(line)?;
                            nodes.push(Node::Range(pipe, body, alt));
                        }
                        "with" => {
                            let pipe = parse_pipeline(&words[1..], line, false, self.depth - 1)?;
                            let (body, alt) = self.parse_body(line)?;
                            nodes.push(Node::With(pipe, body, alt));
                        }
                        "end" | "else" => {
                            return Err(format!("unexpected {{{{{keyword}}}}} on line {line}"));
                        }
                        "define" | "template" | "block" | "break" | "continue" => {
                            return Err(format!(
                                "{{{{{keyword}}}}} is not supported in report templates (line {line})"
                            ));
                        }
                        _ => nodes.push(Node::Action(parse_pipeline(
                            &words,
                            line,
                            false,
                            self.depth - 1,
                        )?)),
                    }
                }
            }
        }
        if stops.is_empty() {
            Ok(nodes)
        } else {
            Err("unexpected end of template: missing {{end}}".to_string())
        }
    }

    /// A body up to `{{end}}`, with an optional `{{else}}` branch.
    fn parse_body(&mut self, line: usize) -> Result<(Vec<Node>, Vec<Node>), String> {
        let body = self.parse_list(&["else", "end"])?;
        let Some(Token::Action { words, .. }) = self.tokens.get(self.pos).cloned() else {
            return Err(format!("missing {{{{end}}}} for action on line {line}"));
        };
        self.pos += 1;
        if matches!(words.first(), Some(Word::Ident(k)) if k == "else") {
            let alt = self.parse_list(&["end"])?;
            self.pos += 1;
            return Ok((body, alt));
        }
        Ok((body, Vec::new()))
    }

    fn parse_if(&mut self, words: &[Word], line: usize) -> Result<Node, String> {
        let mut branches = vec![(
            parse_pipeline(words, line, false, self.depth - 1)?,
            Vec::new(),
        )];
        loop {
            let body = self.parse_list(&["else", "end"])?;
            branches.last_mut().expect("branch").1 = body;
            let Some(Token::Action { words, line }) = self.tokens.get(self.pos).cloned() else {
                return Err(format!("missing {{{{end}}}} for if on line {line}"));
            };
            self.pos += 1;
            match words.get(1) {
                _ if matches!(words.first(), Some(Word::Ident(k)) if k == "end") => {
                    return Ok(Node::If(branches, Vec::new()));
                }
                Some(Word::Ident(k)) if k == "if" => {
                    branches.push((
                        parse_pipeline(&words[2..], line, false, self.depth - 1)?,
                        Vec::new(),
                    ));
                }
                _ => {
                    let alt = self.parse_list(&["end"])?;
                    self.pos += 1;
                    return Ok(Node::If(branches, alt));
                }
            }
        }
    }
}

fn parse_pipeline(
    words: &[Word],
    line: usize,
    allow_two_vars: bool,
    depth: usize,
) -> Result<Pipeline, String> {
    if depth > MAX_NESTING {
        return Err(format!(
            "export template nesting exceeds {MAX_NESTING} levels on line {line}"
        ));
    }
    let mut decl = Vec::new();
    let mut declare = false;
    let mut rest = words;
    // `$x :=` / `$x =` / (range) `$i, $v :=`.
    if let Some(op) = rest
        .iter()
        .position(|w| matches!(w, Word::Declare | Word::Assign))
    {
        let targets = &rest[..op];
        let names: Vec<&Word> = targets
            .iter()
            .filter(|w| !matches!(w, Word::Comma))
            .collect();
        let valid = !names.is_empty()
            && names
                .iter()
                .all(|w| matches!(w, Word::Var(_, path) if path.is_empty()))
            && (names.len() == 1 || (allow_two_vars && names.len() == 2));
        if valid {
            decl = names
                .iter()
                .map(|w| match w {
                    Word::Var(name, _) => name.clone(),
                    _ => unreachable!(),
                })
                .collect();
            declare = matches!(rest[op], Word::Declare);
            rest = &rest[op + 1..];
        }
    }
    let mut commands = Vec::new();
    let mut current = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match &rest[i] {
            Word::Pipe => {
                if current.is_empty() {
                    return Err(format!("missing command before | on line {line}"));
                }
                commands.push(std::mem::take(&mut current));
            }
            Word::LParen => {
                let mut paren_depth = 1;
                let start = i + 1;
                while paren_depth > 0 {
                    i += 1;
                    match rest.get(i) {
                        Some(Word::LParen) => paren_depth += 1,
                        Some(Word::RParen) => paren_depth -= 1,
                        Some(_) => {}
                        None => return Err(format!("unclosed ( on line {line}")),
                    }
                }
                let inner = parse_pipeline(&rest[start..i], line, false, depth + 1)?;
                // `(pipeline).Field` arrives as a following Field word.
                let mut fields = Vec::new();
                if let Some(Word::Field(path)) = rest.get(i + 1) {
                    if !path.is_empty() {
                        fields.clone_from(path);
                        i += 1;
                    }
                }
                current.push(Operand::Sub(Box::new(inner), fields));
            }
            Word::RParen => return Err(format!("unexpected ) on line {line}")),
            Word::Field(path) => current.push(Operand::Field(path.clone())),
            Word::Var(name, path) => current.push(Operand::Var(name.clone(), path.clone())),
            Word::Str(s) => current.push(Operand::Literal(Value::Str(s.clone()))),
            Word::Int(n) => current.push(Operand::Literal(Value::Int(*n))),
            Word::Float(f) => current.push(Operand::Literal(Value::Float(*f))),
            Word::Ident(name) => match name.as_str() {
                "true" => current.push(Operand::Literal(Value::Bool(true))),
                "false" => current.push(Operand::Literal(Value::Bool(false))),
                "nil" => current.push(Operand::Literal(Value::Nil)),
                name if FUNCTIONS.contains(&name) => {
                    current.push(Operand::Function(name.to_string()));
                }
                name => return Err(format!("function {name:?} not defined (line {line})")),
            },
            Word::Comma | Word::Declare | Word::Assign => {
                return Err(format!("unexpected {} on line {line}", rest[i].describe()));
            }
        }
        i += 1;
    }
    if !current.is_empty() {
        commands.push(current);
    }
    if commands.is_empty() {
        return Err(format!("missing value for command on line {line}"));
    }
    Ok(Pipeline {
        decl,
        declare,
        commands,
        line,
    })
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

struct Scope {
    vars: Vec<(String, Value)>,
    max_output: usize,
    iterations_left: usize,
    max_value_bytes: usize,
    value_bytes_left: usize,
}

impl Scope {
    fn consume_value_bytes(&mut self, bytes: usize) -> Result<(), String> {
        self.value_bytes_left = self.value_bytes_left.checked_sub(bytes).ok_or_else(|| {
            format!(
                "export template evaluation exceeds {} bytes of intermediate values",
                self.max_value_bytes
            )
        })?;
        Ok(())
    }

    fn copy_value(&mut self, value: &Value) -> Result<Value, String> {
        self.consume_value_bytes(value.storage_bytes())?;
        Ok(value.clone())
    }

    fn lookup(&mut self, name: &str, path: &[String]) -> Result<Value, String> {
        self.consume_value_bytes(name.len())?;
        self.consume_path(path)?;
        let value = self
            .vars
            .iter()
            .rev()
            .find(|(var, _)| var == name)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("undefined variable {name:?}"))?;
        let value = field(value, path)?;
        let bytes = value.storage_bytes();
        if bytes > self.value_bytes_left {
            // Check before cloning; aliases must share the same value budget.
            return Err(format!(
                "export template evaluation exceeds {} bytes of intermediate values",
                self.max_value_bytes
            ));
        }
        let value = value.clone();
        self.consume_value_bytes(bytes)?;
        Ok(value)
    }

    fn consume_iterations(&mut self, count: usize) -> Result<(), String> {
        self.iterations_left = self.iterations_left.checked_sub(count).ok_or_else(|| {
            format!("export template evaluation exceeds {MAX_RANGE_ITERATIONS} range iterations")
        })?;
        Ok(())
    }

    fn consume_path(&mut self, path: &[String]) -> Result<(), String> {
        let bytes = path
            .iter()
            .fold(0usize, |total, part| total.saturating_add(part.len()));
        self.consume_value_bytes(bytes)
    }

    fn declare(&mut self, name: &str, value: Value) -> Result<(), String> {
        self.consume_value_bytes(name.len())?;
        self.vars.push((name.to_string(), value));
        Ok(())
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
        self.consume_value_bytes(name.len())?;
        let slot = self
            .vars
            .iter_mut()
            .rev()
            .find(|(var, _)| var == name)
            .ok_or_else(|| format!("undefined variable {name:?}"))?;
        slot.1 = value;
        Ok(())
    }
}

fn write_output(out: &mut TextBuffer, text: &str) -> Result<(), String> {
    out.write_str(text)
        .map_err(|_| output_limit_error(out.limit))
}

fn exec_list(
    nodes: &[Node],
    dot: &Value,
    scope: &mut Scope,
    out: &mut TextBuffer,
) -> Result<(), String> {
    for node in nodes {
        match node {
            Node::Text(text) => write_output(out, text)?,
            Node::Action(pipe) => {
                let value = eval_pipeline(pipe, dot, scope)?;
                if pipe.decl.is_empty() {
                    value
                        .write_display(out)
                        .map_err(|_| output_limit_error(out.limit))?;
                }
            }
            Node::If(branches, alt) => {
                let depth = scope.vars.len();
                let mut taken = false;
                for (cond, body) in branches {
                    if eval_pipeline(cond, dot, scope)?.truthy() {
                        exec_list(body, dot, scope, out)?;
                        taken = true;
                        break;
                    }
                }
                if !taken {
                    exec_list(alt, dot, scope, out)?;
                }
                scope.vars.truncate(depth);
            }
            Node::With(pipe, body, alt) => {
                let depth = scope.vars.len();
                let value = eval_pipeline(pipe, dot, scope)?;
                if value.truthy() {
                    exec_list(body, &value, scope, out)?;
                } else {
                    exec_list(alt, dot, scope, out)?;
                }
                scope.vars.truncate(depth);
            }
            Node::Range(pipe, body, alt) => {
                let depth = scope.vars.len();
                // Evaluate the header without declaring range variables or
                // repeatedly cloning its template-controlled syntax tree.
                let value = eval_pipeline_commands(pipe, dot, scope)?;
                let count = match &value {
                    Value::List(items) => items.len(),
                    Value::Map(map) => map.len(),
                    Value::Int(n) => usize::try_from((*n).max(0)).unwrap_or(usize::MAX),
                    Value::Nil => 0,
                    other => return Err(format!("range can't iterate over {}", other.kind())),
                };
                scope.consume_iterations(count)?;
                // Integer ranges produce one item at a time. No allocation is
                // proportional to a template-provided integer.
                let items: Box<dyn Iterator<Item = (Value, Value)>> = match value {
                    Value::List(items) => Box::new(
                        items
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| (Value::Int(i64::try_from(i).unwrap_or(i64::MAX)), v)),
                    ),
                    Value::Map(map) => Box::new(map.into_iter().map(|(k, v)| (Value::Str(k), v))),
                    Value::Int(n) => {
                        Box::new((0..n.max(0)).map(|i| (Value::Int(i), Value::Int(i))))
                    }
                    _ => Box::new(std::iter::empty()),
                };
                if count == 0 {
                    exec_list(alt, dot, scope, out)?;
                }
                for (key, item) in items {
                    let inner = scope.vars.len();
                    match pipe.decl.as_slice() {
                        [value_var] => {
                            let value = scope.copy_value(&item)?;
                            scope.declare(value_var, value)?;
                        }
                        [key_var, value_var] => {
                            scope.declare(key_var, key)?;
                            let value = scope.copy_value(&item)?;
                            scope.declare(value_var, value)?;
                        }
                        _ => {}
                    }
                    exec_list(body, &item, scope, out)?;
                    scope.vars.truncate(inner);
                }
                scope.vars.truncate(depth);
            }
        }
    }
    Ok(())
}

fn field<'a>(value: &'a Value, path: &[String]) -> Result<&'a Value, String> {
    let mut current = value;
    for name in path {
        current = match current {
            Value::Map(map) => map
                .get(name)
                .ok_or_else(|| format!("map has no entry for key {name:?}"))?,
            Value::Nil => return Err(format!("nil pointer evaluating .{name}")),
            other => {
                return Err(format!(
                    "can't evaluate field {name} in type {}",
                    other.kind()
                ));
            }
        };
    }
    Ok(current)
}

fn eval_pipeline(pipe: &Pipeline, dot: &Value, scope: &mut Scope) -> Result<Value, String> {
    let value = eval_pipeline_commands(pipe, dot, scope)?;
    match pipe.decl.as_slice() {
        [name] if pipe.declare => {
            let copy = scope.copy_value(&value)?;
            scope.declare(name, copy)?;
        }
        [name] => {
            let copy = scope.copy_value(&value)?;
            scope.assign(name, copy)?;
        }
        _ => {}
    }
    Ok(value)
}

fn eval_pipeline_commands(
    pipe: &Pipeline,
    dot: &Value,
    scope: &mut Scope,
) -> Result<Value, String> {
    let mut piped: Option<Value> = None;
    for command in &pipe.commands {
        piped = Some(
            eval_command(command, dot, scope, piped.take())
                .map_err(|error| format!("line {}: {error}", pipe.line))?,
        );
    }
    Ok(piped.unwrap_or(Value::Nil))
}

fn eval_operand(operand: &Operand, dot: &Value, scope: &mut Scope) -> Result<Value, String> {
    match operand {
        Operand::Field(path) => {
            scope.consume_path(path)?;
            scope.copy_value(field(dot, path)?)
        }
        Operand::Var(name, path) => scope.lookup(name, path),
        Operand::Literal(value) => scope.copy_value(value),
        Operand::Function(name) => call(name, Vec::new(), scope),
        Operand::Sub(pipe, path) => {
            let value = eval_pipeline(pipe, dot, scope)?;
            if path.is_empty() {
                Ok(value)
            } else {
                scope.consume_path(path)?;
                scope.copy_value(field(&value, path)?)
            }
        }
    }
}

fn eval_command(
    command: &[Operand],
    dot: &Value,
    scope: &mut Scope,
    piped: Option<Value>,
) -> Result<Value, String> {
    let Some(first) = command.first() else {
        return Err("empty command".to_string());
    };
    if let Operand::Function(name) = first {
        // `and`/`or` short-circuit like Go's.
        if name == "and" || name == "or" {
            let mut last = Value::Nil;
            let args = command[1..]
                .iter()
                .map(Some)
                .chain(std::iter::once(None).take(usize::from(piped.is_some())));
            let mut piped = piped;
            for arg in args {
                last = match arg {
                    Some(operand) => eval_operand(operand, dot, scope)?,
                    None => piped.take().unwrap_or(Value::Nil),
                };
                if (name == "and") != last.truthy() {
                    return Ok(last);
                }
            }
            return Ok(last);
        }
        let mut args = command[1..]
            .iter()
            .map(|operand| eval_operand(operand, dot, scope))
            .collect::<Result<Vec<_>, _>>()?;
        args.extend(piped);
        return call(name, args, scope);
    }
    if command.len() > 1 {
        return Err(format!("can't give argument to non-function {first:?}"));
    }
    if piped.is_some() {
        return Err("can't pipe a value into a non-function".to_string());
    }
    eval_operand(first, dot, scope)
}

fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let as_f = |v: &Value| match v {
                Value::Int(n) => *n as f64,
                Value::Float(f) => *f,
                _ => 0.0,
            };
            as_f(a)
                .partial_cmp(&as_f(b))
                .ok_or_else(|| "incomparable values".to_string())
        }
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        _ => Err(format!(
            "incompatible types for comparison: {} and {}",
            a.kind(),
            b.kind()
        )),
    }
}

fn arity(name: &str, args: &[Value], want: usize) -> Result<(), String> {
    if args.len() == want {
        Ok(())
    } else {
        Err(format!(
            "wrong number of args for {name}: want {want} got {}",
            args.len()
        ))
    }
}

fn call(name: &str, args: Vec<Value>, scope: &mut Scope) -> Result<Value, String> {
    use std::cmp::Ordering;
    let value = match name {
        "not" => {
            arity(name, &args, 1)?;
            Ok(Value::Bool(!args[0].truthy()))
        }
        "len" => {
            arity(name, &args, 1)?;
            let n = match &args[0] {
                Value::Str(s) => s.len(),
                Value::List(items) => items.len(),
                Value::Map(map) => map.len(),
                other => return Err(format!("len of type {}", other.kind())),
            };
            Ok(Value::Int(i64::try_from(n).unwrap_or(i64::MAX)))
        }
        "index" => {
            let mut iter = args.into_iter();
            let mut current = iter.next().ok_or("index of untyped nil")?;
            for key in iter {
                current = match (current, key) {
                    (Value::List(items), Value::Int(i)) => usize::try_from(i)
                        .ok()
                        .and_then(|i| items.get(i).cloned())
                        .ok_or_else(|| format!("index out of range: {i}"))?,
                    (Value::Map(map), Value::Str(k)) => map.get(&k).cloned().unwrap_or(Value::Nil),
                    (Value::Str(s), Value::Int(i)) => usize::try_from(i)
                        .ok()
                        .and_then(|i| s.as_bytes().get(i).copied())
                        .map(|b| Value::Int(i64::from(b)))
                        .ok_or_else(|| format!("index out of range: {i}"))?,
                    (container, key) => {
                        return Err(format!(
                            "can't index item of type {} with {}",
                            container.kind(),
                            key.kind()
                        ));
                    }
                };
            }
            Ok(current)
        }
        "slice" => {
            let mut iter = args.into_iter();
            let target = iter.next().ok_or("slice of untyped nil")?;
            let bounds: Vec<usize> = iter
                .map(|v| match v {
                    Value::Int(n) => {
                        usize::try_from(n).map_err(|_| "negative slice index".to_string())
                    }
                    other => Err(format!("slice index of type {}", other.kind())),
                })
                .collect::<Result<_, _>>()?;
            match target {
                Value::Str(s) => {
                    let lo = bounds.first().copied().unwrap_or(0);
                    let hi = bounds.get(1).copied().unwrap_or(s.len());
                    s.get(lo..hi)
                        .map(|t| Value::Str(t.to_string()))
                        .ok_or_else(|| "slice index out of range".to_string())
                }
                Value::List(items) => {
                    let lo = bounds.first().copied().unwrap_or(0);
                    let hi = bounds.get(1).copied().unwrap_or(items.len());
                    items
                        .get(lo..hi)
                        .map(|t| Value::List(t.to_vec()))
                        .ok_or_else(|| "slice index out of range".to_string())
                }
                other => Err(format!("can't slice item of type {}", other.kind())),
            }
        }
        "eq" => {
            let (first, rest) = args
                .split_first()
                .ok_or("missing argument for comparison")?;
            if rest.is_empty() {
                return Err("missing argument for comparison".to_string());
            }
            for other in rest {
                if compare(first, other).is_ok_and(|ord| ord == Ordering::Equal) || first == other {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        "ne" | "lt" | "le" | "gt" | "ge" => {
            arity(name, &args, 2)?;
            let ord = compare(&args[0], &args[1]);
            let result = match name {
                "ne" => ord.map_or(args[0] != args[1], |o| o != Ordering::Equal),
                "lt" => ord? == Ordering::Less,
                "le" => ord? != Ordering::Greater,
                "gt" => ord? == Ordering::Greater,
                _ => ord? != Ordering::Less,
            };
            Ok(Value::Bool(result))
        }
        "print" | "println" => {
            // Go's fmt.Sprint: spaces between operands when neither is a string.
            text_value(scope.max_output, |out| {
                for (i, arg) in args.iter().enumerate() {
                    if i > 0
                        && (name == "println"
                            || (!matches!(arg, Value::Str(_))
                                && !matches!(args[i - 1], Value::Str(_))))
                    {
                        out.write_char(' ')?;
                    }
                    arg.write_display(out)?;
                }
                if name == "println" {
                    out.write_char('\n')?;
                }
                Ok(())
            })
        }
        "printf" => {
            let (format, rest) = args.split_first().ok_or("printf needs a format")?;
            let Value::Str(format) = format else {
                return Err("printf format must be a string".to_string());
            };
            sprintf(format, rest, scope.max_output).map(Value::Str)
        }
        "html" | "js" | "urlquery" => text_value(scope.max_output, |out| {
            for arg in &args {
                let mut text = TextBuffer::new(scope.max_output);
                arg.write_display(&mut text)?;
                match name {
                    "html" => write_html_escape(&text.text, out)?,
                    "js" => write_js_escape(&text.text, out)?,
                    _ => write_url_escape(&text.text, out)?,
                }
            }
            Ok(())
        }),
        other => Err(format!("function {other:?} not defined")),
    }?;
    scope.consume_value_bytes(value.storage_bytes())?;
    Ok(value)
}

fn text_value(
    limit: usize,
    write: impl FnOnce(&mut TextBuffer) -> fmt::Result,
) -> Result<Value, String> {
    let mut out = TextBuffer::new(limit);
    write(&mut out).map_err(|_| output_limit_error(limit))?;
    Ok(Value::Str(out.text))
}

/// Go's `fmt.Sprintf` for the verbs report templates use: `%v %s %d %q %t
/// %f %%` with `-`/`0` flags, width, and precision.
fn sprintf(format: &str, args: &[Value], limit: usize) -> Result<String, String> {
    let mut out = TextBuffer::new(limit);
    let mut args = args.iter();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.write_char(c).map_err(|_| output_limit_error(limit))?;
            continue;
        }
        let mut left = false;
        let mut zero = false;
        while let Some(&flag) = chars.peek() {
            match flag {
                '-' => left = true,
                '0' => zero = true,
                '+' | ' ' | '#' => {}
                _ => break,
            }
            chars.next();
        }
        let width = format_number(&mut chars, limit, "width")?;
        let precision = if chars.peek() == Some(&'.') {
            chars.next();
            Some(format_number(&mut chars, limit, "precision")?)
        } else {
            None
        };
        let Some(verb) = chars.next() else {
            write_output(&mut out, "%!(NOVERB)")?;
            break;
        };
        if verb == '%' {
            write_output(&mut out, "%")?;
            continue;
        }
        let Some(arg) = args.next() else {
            write!(out, "%!{verb}(MISSING)").map_err(|_| output_limit_error(limit))?;
            continue;
        };
        let mut text = TextBuffer::new(out.remaining());
        let rendered = match (verb, arg) {
            ('d', Value::Int(n)) => write!(text, "{n}"),
            ('f' | 'g' | 'e', Value::Float(f)) => {
                write_float(&mut text, *f, precision.unwrap_or(6))
            }
            ('f' | 'g' | 'e', Value::Int(n)) => {
                write_float(&mut text, *n as f64, precision.unwrap_or(6))
            }
            ('q', Value::Str(s)) => write!(text, "{s:?}"),
            ('t', Value::Bool(b)) => write!(text, "{b}"),
            ('s' | 'v', value) => {
                if let Some(p) = precision {
                    value.write_display(&mut PrecisionWriter {
                        out: &mut text,
                        chars_left: p,
                    })
                } else {
                    value.write_display(&mut text)
                }
            }
            (verb, value) => (|| {
                write!(text, "%!{verb}({}=", value.kind())?;
                value.write_display(&mut text)?;
                text.write_char(')')
            })(),
        };
        rendered.map_err(|_| output_limit_error(limit))?;
        let pad = width.saturating_sub(text.text.chars().count());
        if pad > out.remaining().saturating_sub(text.text.len()) {
            return Err(output_limit_error(limit));
        }
        let padded = (|| {
            if left {
                out.write_str(&text.text)?;
                out.write_padding(' ', pad)
            } else if zero && matches!(arg, Value::Int(_) | Value::Float(_)) {
                if text.text.starts_with('-') {
                    out.write_char('-')?;
                }
                out.write_padding('0', pad)?;
                out.write_str(text.text.trim_start_matches('-'))
            } else {
                out.write_padding(' ', pad)?;
                out.write_str(&text.text)
            }
        })();
        padded.map_err(|_| output_limit_error(limit))?;
    }
    if args.next().is_some() {
        write_output(&mut out, "%!(EXTRA)")?;
    }
    Ok(out.text)
}

fn format_number(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    limit: usize,
    kind: &str,
) -> Result<usize, String> {
    let mut number = 0usize;
    while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
        number = number
            .checked_mul(10)
            .and_then(|n| n.checked_add(digit as usize))
            .filter(|&n| n <= limit)
            .ok_or_else(|| format!("printf {kind} exceeds {limit} bytes"))?;
        chars.next();
    }
    Ok(number)
}

fn write_float(out: &mut TextBuffer, value: f64, precision: usize) -> fmt::Result {
    // A binary64 fraction terminates within 1074 decimal places. Format that
    // bounded prefix, then stream any additional zeros. Passing a template's
    // arbitrary precision directly into Rust formatting can panic or allocate
    // before the writer gets a chance to enforce the output limit.
    let prefix_precision = precision.min(1074);
    let minimum_len = precision
        .saturating_add(1)
        .saturating_add(usize::from(precision != 0))
        .saturating_add(usize::from(value.is_sign_negative()));
    if value.is_finite() && minimum_len > out.remaining() {
        return Err(fmt::Error);
    }
    write!(out, "{value:.prefix_precision$}")?;
    if value.is_finite() {
        out.write_padding('0', precision - prefix_precision)?;
    }
    Ok(())
}

/// Legacy bv's `escapeReportText`: issue text in a report template stays
/// literal Markdown.
///
/// Markdown punctuation is backslash-escaped,
/// then HTML-sensitive characters become entities, and leading blanks of
/// each line become character references so field indentation cannot start
/// a code block.
#[must_use]
pub fn escape_report_text(value: &str) -> String {
    const MARKDOWN_PUNCTUATION: &str = "\\`*_[]|#!$%()+,-./:;=?@^{}~";
    let mut markdown = String::with_capacity(value.len());
    for c in value.chars() {
        if MARKDOWN_PUNCTUATION.contains(c) {
            markdown.push('\\');
        }
        markdown.push(c);
    }
    let escaped = html_escape(&markdown);
    let mut literal = String::with_capacity(escaped.len());
    let mut line_start = true;
    for c in escaped.chars() {
        if line_start && c == ' ' {
            literal.push_str("&#32;");
            continue;
        }
        if line_start && c == '\t' {
            literal.push_str("&#9;");
            continue;
        }
        literal.push(c);
        line_start = c == '\n' || c == '\r';
    }
    literal
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // String's fmt::Write implementation cannot fail.
    let _ = write_html_escape(text, &mut out);
    out
}

fn write_html_escape(text: &str, out: &mut impl fmt::Write) -> fmt::Result {
    for c in text.chars() {
        match c {
            '&' => out.write_str("&amp;"),
            '<' => out.write_str("&lt;"),
            '>' => out.write_str("&gt;"),
            '"' => out.write_str("&#34;"),
            '\'' => out.write_str("&#39;"),
            '\0' => out.write_char('\u{FFFD}'),
            c => out.write_char(c),
        }?;
    }
    Ok(())
}

fn write_js_escape(text: &str, out: &mut impl fmt::Write) -> fmt::Result {
    for c in text.chars() {
        match c {
            '\\' => out.write_str("\\\\"),
            '\'' => out.write_str("\\'"),
            '"' => out.write_str("\\\""),
            '<' => out.write_str("\\u003C"),
            '>' => out.write_str("\\u003E"),
            '&' => out.write_str("\\u0026"),
            '=' => out.write_str("\\u003D"),
            '\n' => out.write_str("\\n"),
            '\r' => out.write_str("\\r"),
            '\t' => out.write_str("\\t"),
            c if c.is_control() => write!(out, "\\u{:04X}", u32::from(c)),
            c => out.write_char(c),
        }?;
    }
    Ok(())
}

fn write_url_escape(text: &str, out: &mut impl fmt::Write) -> fmt::Result {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.write_char(char::from(byte))
            }
            b' ' => out.write_char('+'),
            other => write!(out, "%{other:02X}"),
        }?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Value, render};
    use std::collections::BTreeMap;

    fn data() -> Value {
        let issue = |id: &str, status: &str, priority: i64, labels: &[&str]| {
            Value::Map(BTreeMap::from([
                ("ID".to_string(), Value::Str(id.to_string())),
                ("Title".to_string(), Value::Str(format!("{id} title"))),
                ("Status".to_string(), Value::Str(status.to_string())),
                ("Priority".to_string(), Value::Int(priority)),
                (
                    "Labels".to_string(),
                    Value::List(
                        labels
                            .iter()
                            .map(|l| Value::Str((*l).to_string()))
                            .collect(),
                    ),
                ),
            ]))
        };
        Value::Map(BTreeMap::from([
            ("Title".to_string(), Value::Str("Report".to_string())),
            (
                "Issues".to_string(),
                Value::List(vec![
                    issue("A", "open", 0, &["api", "ui"]),
                    issue("B", "closed", 2, &[]),
                ]),
            ),
            ("Graph".to_string(), Value::Str(String::new())),
        ]))
    }

    fn ok(template: &str) -> String {
        render(template, &data(), 1 << 20).unwrap_or_else(|e| panic!("{template}: {e}"))
    }

    #[test]
    fn fields_ranges_and_trim_markers() {
        let out = ok(
            "# {{.Title}}\n{{range .Issues -}}\n- {{.ID}}: {{.Title}} (P{{.Priority}})\n{{end -}}\nTotal: {{len .Issues}}",
        );
        assert_eq!(
            out,
            "# Report\n- A: A title (P0)\n- B: B title (P2)\nTotal: 2"
        );
    }

    #[test]
    fn conditionals_comparisons_and_else_branches() {
        let template = "{{range .Issues}}{{if eq .Status \"open\"}}[ ]{{else if eq .Status \"closed\"}}[x]{{else}}[?]{{end}} {{.ID}}{{if gt .Priority 1}} low{{end}};{{end}}";
        assert_eq!(ok(template), "[ ] A;[x] B low;");
        assert_eq!(ok("{{if .Graph}}graph{{else}}no graph{{end}}"), "no graph");
        assert_eq!(
            ok("{{range .Issues}}{{range .Labels}}<{{.}}>{{else}}none{{end}};{{end}}"),
            "<api><ui>;none;"
        );
    }

    #[test]
    fn variables_pipelines_and_builtins() {
        let template = "{{$t := .Title}}{{range $i, $issue := .Issues}}{{$i}}={{$issue.ID}}/{{$t}} {{end}}{{.Title | printf \"%-8s|\"}}{{printf \"%03d\" 7}}";
        assert_eq!(ok(template), "0=A/Report 1=B/Report Report  |007");
        assert_eq!(
            ok(
                "{{with index .Issues 0}}{{.ID}}:{{index .Labels 1}}{{end}}{{with .Graph}}x{{else}}-{{end}}"
            ),
            "A:ui-"
        );
        assert_eq!(
            ok("{{if and (gt (len .Issues) 1) (not .Graph)}}y{{end}}"),
            "y"
        );
    }

    #[test]
    fn missing_keys_unknown_functions_and_unclosed_actions_fail() {
        let err = render("{{.Nope}}", &data(), 1 << 20).unwrap_err();
        assert!(err.contains("map has no entry for key \"Nope\""), "{err}");
        let err = render("{{upper .Title}}", &data(), 1 << 20).unwrap_err();
        assert!(err.contains("function \"upper\" not defined"), "{err}");
        assert!(render("{{if .Title}}x", &data(), 1 << 20).is_err());
        assert!(render("{{.Title", &data(), 1 << 20).is_err());
    }

    #[test]
    fn report_text_escaping_matches_legacy() {
        assert_eq!(super::escape_report_text("a*b <c>"), "a\\*b &lt;c&gt;");
        assert_eq!(super::escape_report_text("x\n  y"), "x\n&#32;&#32;y");
        assert_eq!(super::escape_report_text("it's #1"), "it&#39;s \\#1");
    }

    #[test]
    fn comments_escaping_and_output_cap() {
        assert_eq!(ok("a{{/* note */}}b{{- /* trimmed */ -}} c"), "abc");
        assert_eq!(
            ok("{{html \"<a&b>\"}} {{urlquery \"a b&c\"}}"),
            "&lt;a&amp;b&gt; a+b%26c"
        );
        let err = render("{{range .Issues}}{{.Title}}{{end}}", &data(), 4).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn integer_ranges_preserve_values_and_empty_branches() {
        assert_eq!(
            ok("{{range $i, $v := 3}}{{$i}}={{$v}};{{end}}"),
            "0=0;1=1;2=2;"
        );
        assert_eq!(
            ok("{{range 0}}x{{else}}zero{{end}}/{{range -1}}x{{else}}negative{{end}}"),
            "zero/negative"
        );
        assert_eq!(
            render("{{range 1000000}}{{end}}ok", &Value::Nil, 2).unwrap(),
            "ok"
        );
    }

    #[test]
    fn range_budget_is_shared_by_nested_and_empty_loops() {
        for template in [
            "{{range 9223372036854775807}}x{{end}}",
            "{{range 1000001}}{{end}}",
            "{{range 500000}}{{end}}{{range 500001}}{{end}}",
            "{{range 1000}}{{range 1000}}{{end}}{{end}}",
        ] {
            let error = render(template, &Value::Nil, 16 << 20).unwrap_err();
            assert!(
                error.contains("exceeds 1000000 range iterations"),
                "{template}: {error}"
            );
        }
    }

    #[test]
    fn printf_preserves_padding_precision_and_unicode_byte_boundaries() {
        for (template, expected) in [
            ("{{printf \"%05d\" -7}}", "-0007"),
            ("{{printf \"%-5d\" -7}}", "-7   "),
            ("{{printf \"%5.2s\" \"é中z\"}}", "   é中"),
            ("{{printf \"%.0f\" 1.0}}", "1"),
            ("{{printf \"%.2f\" 1.25}}", "1.25"),
            ("{{printf \"%q\" \"a\\nb\"}}", "\"a\\nb\""),
            ("{{printf \"%t%%\" true}}", "true%"),
        ] {
            assert_eq!(
                render(template, &Value::Nil, expected.len()).unwrap(),
                expected,
                "{template}"
            );
            assert!(
                render(template, &Value::Nil, expected.len() - 1).is_err(),
                "{template} must enforce bytes, including Unicode and padding"
            );
        }
    }

    #[test]
    fn printf_rejects_huge_or_overflowing_width_and_precision() {
        for (template, detail) in [
            ("{{printf \"%1000000000s\" \"x\"}}", "width"),
            ("{{printf \"%-1000000000s\" \"x\"}}", "width"),
            ("{{printf \"%01000000000d\" -1}}", "width"),
            ("{{printf \"%.1000000000f\" 1.0}}", "precision"),
            ("{{printf \"%184467440737095516160s\" \"x\"}}", "width"),
        ] {
            let error = render(template, &Value::Nil, 256).unwrap_err();
            assert!(
                error.contains(&format!("printf {detail} exceeds")),
                "{error}"
            );
        }
        for format in ["%184467440737095516160s", "%.184467440737095516160f"] {
            // A permissive caller still cannot make numeric parsing overflow.
            assert!(super::sprintf(format, &[Value::Int(1)], usize::MAX).is_err());
        }
    }

    #[test]
    fn large_float_precision_streams_without_formatter_panics() {
        let template = "{{printf \"%.65536f\" 1.0}}";
        let output = render(template, &Value::Nil, 65_538).unwrap();
        assert_eq!(output.len(), 65_538);
        assert!(output.starts_with("1."));
        assert!(output[2..].bytes().all(|byte| byte == b'0'));
        assert!(render(template, &Value::Nil, 65_537).is_err());
    }

    #[test]
    fn composed_formatting_and_escaping_share_output_limits() {
        for (template, expected) in [
            ("{{printf \"%4s%4s\" \"a\" \"b\"}}", "   a   b"),
            (
                "{{print (printf \"%4s\" \"a\") (printf \"%4s\" \"b\")}}",
                "   a   b",
            ),
            ("{{println \"a\" \"b\"}}", "a b\n"),
            ("{{html (printf \"%s\" \"<&>\")}}", "&lt;&amp;&gt;"),
            ("{{js \"<\"}}", "\\u003C"),
            ("{{urlquery \"é\"}}", "%C3%A9"),
        ] {
            assert_eq!(
                render(template, &Value::Nil, expected.len()).unwrap(),
                expected
            );
            let error = render(template, &Value::Nil, expected.len() - 1).unwrap_err();
            assert!(error.contains("exceeds"), "{template}: {error}");
        }
        // Truncation at an outer step cannot authorize an oversized temporary.
        assert!(render("{{printf \"%.1s\" (printf \"%9s\" \"x\")}}", &Value::Nil, 8).is_err());
        // Existing input text may be inspected/truncated without rendering it all.
        assert_eq!(
            render("{{printf \"%.1s\" \"abcdef\"}}", &Value::Nil, 1).unwrap(),
            "a"
        );
    }

    #[test]
    fn intermediate_value_budget_covers_unprinted_assignments_and_aliases() {
        let allowed = "{{$x := printf \"%65536s\" \"x\"}}{{range 6}}{{$y := $x}}{{end}}ok";
        let excessive = "{{$x := printf \"%65536s\" \"x\"}}{{range 7}}{{$y := $x}}{{end}}ok";
        assert_eq!(render(allowed, &Value::Nil, 128 << 10).unwrap(), "ok");
        let error = render(excessive, &Value::Nil, 128 << 10).unwrap_err();
        assert!(
            error.contains("exceeds 1048576 bytes of intermediate values"),
            "{error}"
        );
        // A separate render has a fresh allowance after a rejected template.
        assert_eq!(render(allowed, &Value::Nil, 128 << 10).unwrap(), "ok");
    }

    #[test]
    fn intermediate_value_budget_counts_empty_collection_storage() {
        let values = Value::List(vec![Value::Str(String::new()); 1024]);
        assert_eq!(
            render(
                "{{$items := .}}{{range 2}}{{$copy := $items}}{{end}}ok",
                &values,
                128 << 10
            )
            .unwrap(),
            "ok"
        );
        let error = render(
            "{{$items := .}}{{range 1000000}}{{$copy := $items}}{{end}}ok",
            &values,
            128 << 10,
        )
        .unwrap_err();
        assert!(
            error.contains("exceeds 1048576 bytes of intermediate values"),
            "{error}"
        );
    }

    #[test]
    fn intermediate_value_budget_counts_variable_names() {
        let name = "v".repeat(64 << 10);
        let body = ["{{$", &name, " := 1}}{{if $", &name, "}}ok{{end}}"].concat();
        let allowed = format!("{{{{range 2}}}}{body}{{{{end}}}}");
        let excessive = format!("{{{{range 1000000}}}}{body}{{{{end}}}}");
        assert_eq!(render(&allowed, &Value::Nil, 128 << 10).unwrap(), "okok");
        let error = render(&excessive, &Value::Nil, 128 << 10).unwrap_err();
        assert!(
            error.contains("exceeds 1048576 bytes of intermediate values"),
            "{error}"
        );
    }

    #[test]
    fn nesting_limit_covers_blocks_parentheses_and_combinations() {
        let nested = |blocks: usize, expressions: usize| {
            format!(
                "{}{{{{{}print \"ok\"{}}}}}{}",
                "{{if true}}".repeat(blocks),
                "(".repeat(expressions),
                ")".repeat(expressions),
                "{{end}}".repeat(blocks),
            )
        };
        for (blocks, expressions) in [(128, 0), (0, 128), (64, 64)] {
            assert_eq!(
                render(&nested(blocks, expressions), &Value::Nil, 2).unwrap(),
                "ok"
            );
        }
        for (blocks, expressions) in [(129, 0), (0, 129), (64, 65)] {
            let error = render(&nested(blocks, expressions), &Value::Nil, 2).unwrap_err();
            assert!(error.contains("nesting exceeds 128 levels"), "{error}");
        }
    }

    #[test]
    fn field_lookup_copies_only_the_selected_value() {
        let data = Value::Map(BTreeMap::from([
            ("Large".into(), Value::Str("x".repeat(1 << 20))),
            ("Small".into(), Value::Str("ok".into())),
        ]));
        let expected = "ok".repeat(16);
        for template in [
            "{{.Small}}".repeat(16),
            "{{range 16}}{{$.Small}}{{end}}".into(),
        ] {
            assert_eq!(render(&template, &data, 256 << 10).unwrap(), expected);
        }
    }
}
