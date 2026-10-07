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
    fn display(&self) -> String {
        match self {
            Self::Nil => "<no value>".to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Int(n) => n.to_string(),
            Self::Float(f) => format!("{f}"),
            Self::Str(s) => s.clone(),
            Self::List(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(Self::display)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Self::Map(map) => format!(
                "map[{}]",
                map.iter()
                    .map(|(k, v)| format!("{k}:{}", v.display()))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
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
/// `missingkey=error` would. Output is capped at `max_output` bytes.
pub fn render(template: &str, data: &Value, max_output: usize) -> Result<String, String> {
    let tokens = lex(template)?;
    let mut parser = Parser { tokens, pos: 0 };
    let nodes = parser.parse_list(&[])?;
    if let Some(token) = parser.tokens.get(parser.pos) {
        return Err(format!("unexpected {}", token.describe()));
    }
    let mut out = String::new();
    let mut scope = Scope {
        vars: vec![("$".to_string(), data.clone())],
        max_output,
    };
    exec_list(&nodes, data, &mut scope, &mut out)?;
    Ok(out)
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
}

impl Parser {
    /// Parse nodes until an action whose keyword is in `stops` (left
    /// unconsumed).
    fn parse_list(&mut self, stops: &[&str]) -> Result<Vec<Node>, String> {
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
                            let pipe = parse_pipeline(&words[1..], line, true)?;
                            let (body, alt) = self.parse_body(line)?;
                            nodes.push(Node::Range(pipe, body, alt));
                        }
                        "with" => {
                            let pipe = parse_pipeline(&words[1..], line, false)?;
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
                        _ => nodes.push(Node::Action(parse_pipeline(&words, line, false)?)),
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
        let mut branches = vec![(parse_pipeline(words, line, false)?, Vec::new())];
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
                    branches.push((parse_pipeline(&words[2..], line, false)?, Vec::new()));
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

fn parse_pipeline(words: &[Word], line: usize, allow_two_vars: bool) -> Result<Pipeline, String> {
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
                let mut depth = 1;
                let start = i + 1;
                while depth > 0 {
                    i += 1;
                    match rest.get(i) {
                        Some(Word::LParen) => depth += 1,
                        Some(Word::RParen) => depth -= 1,
                        Some(_) => {}
                        None => return Err(format!("unclosed ( on line {line}")),
                    }
                }
                let inner = parse_pipeline(&rest[start..i], line, false)?;
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
}

impl Scope {
    fn lookup(&self, name: &str) -> Result<Value, String> {
        self.vars
            .iter()
            .rev()
            .find(|(var, _)| var == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| format!("undefined variable {name:?}"))
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
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

fn write_output(out: &mut String, text: &str, scope: &Scope) -> Result<(), String> {
    if out.len() + text.len() > scope.max_output {
        return Err(format!(
            "rendered export template exceeds {} MiB",
            scope.max_output >> 20
        ));
    }
    out.push_str(text);
    Ok(())
}

fn exec_list(
    nodes: &[Node],
    dot: &Value,
    scope: &mut Scope,
    out: &mut String,
) -> Result<(), String> {
    for node in nodes {
        match node {
            Node::Text(text) => write_output(out, text, scope)?,
            Node::Action(pipe) => {
                let value = eval_pipeline(pipe, dot, scope)?;
                if pipe.decl.is_empty() {
                    write_output(out, &value.display(), scope)?;
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
                let mut header = pipe.clone();
                let decl = std::mem::take(&mut header.decl);
                let value = eval_pipeline(&header, dot, scope)?;
                let items: Vec<(Value, Value)> = match value {
                    Value::List(items) => items
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| (Value::Int(i64::try_from(i).unwrap_or(i64::MAX)), v))
                        .collect(),
                    Value::Map(map) => map.into_iter().map(|(k, v)| (Value::Str(k), v)).collect(),
                    Value::Int(n) => (0..n.max(0))
                        .map(|i| (Value::Int(i), Value::Int(i)))
                        .collect(),
                    Value::Nil => Vec::new(),
                    other => return Err(format!("range can't iterate over {}", other.kind())),
                };
                if items.is_empty() {
                    exec_list(alt, dot, scope, out)?;
                }
                for (key, item) in items {
                    let inner = scope.vars.len();
                    match decl.as_slice() {
                        [value_var] => scope.vars.push((value_var.clone(), item.clone())),
                        [key_var, value_var] => {
                            scope.vars.push((key_var.clone(), key));
                            scope.vars.push((value_var.clone(), item.clone()));
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

fn field(value: &Value, path: &[String]) -> Result<Value, String> {
    let mut current = value.clone();
    for name in path {
        current = match current {
            Value::Map(mut map) => map
                .remove(name)
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
    let mut piped: Option<Value> = None;
    for command in &pipe.commands {
        piped = Some(
            eval_command(command, dot, scope, piped.take())
                .map_err(|error| format!("line {}: {error}", pipe.line))?,
        );
    }
    let value = piped.unwrap_or(Value::Nil);
    match pipe.decl.as_slice() {
        [name] if pipe.declare => scope.vars.push((name.clone(), value.clone())),
        [name] => scope.assign(name, value.clone())?,
        _ => {}
    }
    Ok(value)
}

fn eval_operand(operand: &Operand, dot: &Value, scope: &mut Scope) -> Result<Value, String> {
    match operand {
        Operand::Field(path) => field(dot, path),
        Operand::Var(name, path) => field(&scope.lookup(name)?, path),
        Operand::Literal(value) => Ok(value.clone()),
        Operand::Function(name) => call(name, Vec::new()),
        Operand::Sub(pipe, path) => field(&eval_pipeline(pipe, dot, scope)?, path),
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
                .chain(piped.iter().map(|_| None));
            let mut piped = piped.clone();
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
        return call(name, args);
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

fn call(name: &str, args: Vec<Value>) -> Result<Value, String> {
    use std::cmp::Ordering;
    match name {
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
        "print" => {
            // Go's fmt.Sprint: spaces between operands when neither is a string.
            let mut out = String::new();
            for (i, arg) in args.iter().enumerate() {
                if i > 0 && !matches!(arg, Value::Str(_)) && !matches!(args[i - 1], Value::Str(_)) {
                    out.push(' ');
                }
                out.push_str(&arg.display());
            }
            Ok(Value::Str(out))
        }
        "println" => Ok(Value::Str(format!(
            "{}\n",
            args.iter()
                .map(Value::display)
                .collect::<Vec<_>>()
                .join(" ")
        ))),
        "printf" => {
            let (format, rest) = args.split_first().ok_or("printf needs a format")?;
            let Value::Str(format) = format else {
                return Err("printf format must be a string".to_string());
            };
            Ok(Value::Str(sprintf(format, rest)))
        }
        "html" => Ok(Value::Str(html_escape(&join_args(&args)))),
        "js" => Ok(Value::Str(js_escape(&join_args(&args)))),
        "urlquery" => Ok(Value::Str(url_escape(&join_args(&args)))),
        other => Err(format!("function {other:?} not defined")),
    }
}

fn join_args(args: &[Value]) -> String {
    args.iter().map(Value::display).collect()
}

/// Go's `fmt.Sprintf` for the verbs report templates use: `%v %s %d %q %t
/// %f %%` with `-`/`0` flags, width, and precision.
fn sprintf(format: &str, args: &[Value]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
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
        let mut width = 0usize;
        while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
            width = width * 10 + digit as usize;
            chars.next();
        }
        let precision = if chars.peek() == Some(&'.') {
            chars.next();
            let mut p = 0usize;
            while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
                p = p * 10 + digit as usize;
                chars.next();
            }
            Some(p)
        } else {
            None
        };
        let Some(verb) = chars.next() else {
            out.push_str("%!(NOVERB)");
            break;
        };
        if verb == '%' {
            out.push('%');
            continue;
        }
        let Some(arg) = args.next() else {
            let _ = write!(out, "%!{verb}(MISSING)");
            continue;
        };
        let mut text = match (verb, arg) {
            ('d', Value::Int(n)) => n.to_string(),
            ('f' | 'g' | 'e', Value::Float(f)) => format!("{:.*}", precision.unwrap_or(6), f),
            ('f' | 'g' | 'e', Value::Int(n)) => format!("{:.*}", precision.unwrap_or(6), *n as f64),
            ('q', Value::Str(s)) => format!("{s:?}"),
            ('t', Value::Bool(b)) => b.to_string(),
            ('s' | 'v', value) => {
                let mut text = value.display();
                if let Some(p) = precision {
                    text = text.chars().take(p).collect();
                }
                text
            }
            (verb, value) => format!("%!{verb}({}={})", value.kind(), value.display()),
        };
        let len = text.chars().count();
        if len < width {
            let pad = width - len;
            if left {
                text.push_str(&" ".repeat(pad));
            } else if zero && matches!(arg, Value::Int(_) | Value::Float(_)) {
                let negative = text.starts_with('-');
                let digits = text.trim_start_matches('-').to_string();
                text = format!(
                    "{}{}{digits}",
                    if negative { "-" } else { "" },
                    "0".repeat(pad)
                );
            } else {
                text = format!("{}{text}", " ".repeat(pad));
            }
        }
        out.push_str(&text);
    }
    if args.next().is_some() {
        out.push_str("%!(EXTRA)");
    }
    out
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
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            '\0' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

fn js_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '=' => out.push_str("\\u003D"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

fn url_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
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
}
