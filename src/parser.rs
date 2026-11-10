//! Lexer + parseur descendant récursif pour la grammaire du shell POSIX.
//! Le lexer travaille à la demande : certaines décisions (here-docs, alias,
//! `((`) dépendent du contexte syntaxique.

use crate::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub msg: String,
    pub pos: usize,
    /// Vrai si l'entrée est seulement inachevée (le prompt continue la saisie).
    pub incomplete: bool,
}

type PResult<T> = Result<T, ParseError>;

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    AndIf,
    OrIf,
    Semi,
    DSemi,
    SemiAmp,
    Amp,
    Pipe,
    PipeAmp,
    LParen,
    RParen,
    Redir(RedirOp, Option<i32>),
    /// << ou <<- (vrai = <<-)
    HereDoc(bool, Option<i32>),
}

#[derive(Debug, Clone)]
pub enum Tok {
    Word(Word, String),
    Op(Op),
    Newline,
    Eof,
}

struct PendingHere {
    delim: String,
    strip_tabs: bool,
    quoted: bool,
    body: Rc<RefCell<HereBody>>,
}

pub struct Parser {
    src: Vec<char>,
    pos: usize,
    peeked: Option<(Tok, usize, usize)>,
    pending: Vec<PendingHere>,
    aliases: Option<Rc<HashMap<String, String>>>,
    alias_budget: usize,
    last_alias: Option<String>,
    /// alias terminé par une espace : le mot suivant est aussi candidat
    alias_next: bool,
    depth: usize,
}

pub const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "case", "esac", "while", "until", "for",
    "in", "{", "}", "!", "function", "[[", "]]", "time",
];

pub fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    match c.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    c.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_meta(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

/// Mot entièrement littéral (pour reconnaître les mots réservés).
pub fn plain_word(w: &Word) -> Option<String> {
    if w.len() == 1 {
        if let Part::Lit(s) = &w[0] {
            return Some(s.clone());
        }
    }
    None
}

impl Parser {
    pub fn new(src: &str) -> Self {
        Parser {
            src: src.chars().collect(),
            pos: 0,
            peeked: None,
            pending: Vec::new(),
            aliases: None,
            alias_budget: 32,
            last_alias: None,
            alias_next: false,
            depth: 0,
        }
    }

    pub fn with_aliases(src: &str, aliases: Rc<HashMap<String, String>>) -> Self {
        let mut p = Parser::new(src);
        p.aliases = Some(aliases);
        p
    }

    pub fn set_aliases(&mut self, aliases: Rc<HashMap<String, String>>) {
        self.aliases = Some(aliases);
    }

    /// Analyse la prochaine commande complète (jusqu'à une fin de ligne).
    /// Permet d'exécuter un script au fil de l'eau (les alias définis
    /// s'appliquent aux lignes suivantes).
    pub fn parse_one(&mut self) -> PResult<Option<List>> {
        self.skip_newlines()?;
        if matches!(self.peek()?, Tok::Eof) {
            if !self.pending.is_empty() {
                return self.eof_err("here-document non terminé");
            }
            return Ok(None);
        }
        let mut items = Vec::new();
        loop {
            match self.peek()? {
                Tok::Eof => break,
                Tok::Newline => {
                    self.next()?;
                    break;
                }
                Tok::Op(Op::RParen) => return self.err("« ) » inattendue"),
                Tok::Op(Op::DSemi) => return self.err("« ;; » inattendu"),
                _ => {}
            }
            if let Some(r) = self.peek_reserved()? {
                if matches!(r.as_str(), "then" | "else" | "elif" | "fi" | "do" | "done" | "esac" | "}" | "in" | "]]") {
                    return self.err(format!("« {r} » inattendu"));
                }
            }
            let and_or = self.parse_and_or()?;
            let mut background = false;
            match self.peek()? {
                Tok::Op(Op::Semi) => {
                    self.next()?;
                }
                Tok::Op(Op::Amp) => {
                    self.next()?;
                    background = true;
                }
                Tok::Newline => {
                    self.next()?;
                    items.push(ListItem { and_or, background });
                    break;
                }
                Tok::Eof => {
                    items.push(ListItem { and_or, background });
                    break;
                }
                Tok::Word(_, raw) => {
                    let r = raw.clone();
                    return self.err(format!("mot inattendu « {r} »"));
                }
                t => {
                    let s = match t {
                        Tok::Op(o) => op_str(o).to_string(),
                        _ => "?".into(),
                    };
                    return self.err(format!("erreur de syntaxe près de « {s} »"));
                }
            }
            items.push(ListItem { and_or, background });
        }
        Ok(Some(items))
    }

    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            msg: msg.into(),
            pos: self.pos,
            incomplete: false,
        })
    }

    fn eof_err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            msg: msg.into(),
            pos: self.pos,
            incomplete: true,
        })
    }

    fn cur(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn at(&self, off: usize) -> Option<char> {
        self.src.get(self.pos + off).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        let mut i = self.pos;
        for c in s.chars() {
            if self.src.get(i) != Some(&c) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn text(&self, a: usize, b: usize) -> String {
        self.src[a.min(self.src.len())..b.min(self.src.len())]
            .iter()
            .collect()
    }

    // ------------------------------------------------------------------
    // Lexer
    // ------------------------------------------------------------------

    fn skip_blanks(&mut self) {
        loop {
            match self.cur() {
                Some(' ') | Some('\t') => self.pos += 1,
                Some('\\') if self.at(1) == Some('\n') => self.pos += 2,
                Some('#') => {
                    while let Some(c) = self.cur() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn lex(&mut self) -> PResult<(Tok, usize, usize)> {
        self.skip_blanks();
        let start = self.pos;
        let c = match self.cur() {
            None => return Ok((Tok::Eof, start, start)),
            Some(c) => c,
        };
        if c == '\n' {
            self.pos += 1;
            return Ok((Tok::Newline, start, self.pos));
        }
        // Numéro de descripteur devant une redirection : 2>, 10<&...
        if c.is_ascii_digit() {
            let mut i = self.pos;
            while i < self.src.len() && self.src[i].is_ascii_digit() {
                i += 1;
            }
            if i < self.src.len() && (self.src[i] == '<' || self.src[i] == '>') {
                let n: String = self.src[self.pos..i].iter().collect();
                if let Ok(fd) = n.parse::<i32>() {
                    self.pos = i;
                    let op = self.lex_redir(Some(fd));
                    return Ok((Tok::Op(op), start, self.pos));
                }
            }
        }
        let op = match c {
            '&' if self.starts_with("&&") => Some((Op::AndIf, 2)),
            '&' if self.starts_with("&>>") => Some((Op::Redir(RedirOp::AndAppend, None), 3)),
            '&' if self.starts_with("&>") => Some((Op::Redir(RedirOp::AndOut, None), 2)),
            '&' => Some((Op::Amp, 1)),
            '|' if self.starts_with("||") => Some((Op::OrIf, 2)),
            '|' if self.starts_with("|&") => Some((Op::PipeAmp, 2)),
            '|' => Some((Op::Pipe, 1)),
            ';' if self.starts_with(";;") => Some((Op::DSemi, 2)),
            ';' if self.starts_with(";&") => Some((Op::SemiAmp, 2)),
            ';' => Some((Op::Semi, 1)),
            '(' => Some((Op::LParen, 1)),
            ')' => Some((Op::RParen, 1)),
            _ => None,
        };
        if let Some((op, n)) = op {
            self.pos += n;
            return Ok((Tok::Op(op), start, self.pos));
        }
        if (c == '<' || c == '>') && self.at(1) == Some('(') {
            // substitution de processus : traitée comme un mot
        } else if c == '<' || c == '>' {
            let op = self.lex_redir(None);
            return Ok((Tok::Op(op), start, self.pos));
        }
        let (w, raw) = self.lex_word()?;
        Ok((Tok::Word(w, raw), start, self.pos))
    }

    fn lex_redir(&mut self, fd: Option<i32>) -> Op {
        let table: &[(&str, Op)] = &[
            ("<<<", Op::Redir(RedirOp::HereStr, fd)),
            ("<<-", Op::HereDoc(true, fd)),
            ("<<", Op::HereDoc(false, fd)),
            ("<&", Op::Redir(RedirOp::DupIn, fd)),
            ("<>", Op::Redir(RedirOp::ReadWrite, fd)),
            ("<", Op::Redir(RedirOp::In, fd)),
            (">>", Op::Redir(RedirOp::Append, fd)),
            (">&", Op::Redir(RedirOp::DupOut, fd)),
            (">|", Op::Redir(RedirOp::Clobber, fd)),
            (">", Op::Redir(RedirOp::Out, fd)),
        ];
        for (s, op) in table {
            if self.starts_with(s) {
                self.pos += s.chars().count();
                return op.clone();
            }
        }
        unreachable!()
    }

    fn lex_word(&mut self) -> PResult<(Word, String)> {
        self.lex_word_mode(false)
    }

    /// `regex` : opérande droit de =~ — parenthèses, | < > font partie du mot.
    fn lex_word_mode(&mut self, regex: bool) -> PResult<(Word, String)> {
        let mut rdepth = 0usize;
        let start = self.pos;
        let mut parts: Vec<Part> = Vec::new();
        let mut lit = String::new();
        // Pour le tilde après '=' ou ':' dans une affectation.
        let mut assign_like = false;
        let mut brace_depth = 0usize;

        macro_rules! flush {
            () => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
            };
        }

        while let Some(c) = self.cur() {
            if regex {
                // une ) non appariée ferme le groupe de [[ … ]]
                if c == ')' && rdepth == 0 {
                    break;
                }
                match c {
                    '(' => rdepth += 1,
                    ')' => rdepth -= 1,
                    _ => {}
                }
                if matches!(c, '(' | ')' | '|' | '<' | '>') || (rdepth > 0 && matches!(c, ' ' | '\t' | '&' | ';')) {
                    lit.push(c);
                    self.pos += 1;
                    continue;
                }
            }
            if is_meta(c) {
                // <( et >( au milieu d'un mot
                if (c == '<' || c == '>') && self.at(1) == Some('(') {
                    flush!();
                    let out = c == '>';
                    self.pos += 2;
                    let list = self.parse_nested_until_rparen()?;
                    parts.push(Part::ProcSub(out, Rc::new(list)));
                    continue;
                }
                // ${...} peut contenir des espaces : géré dans lex_dollar.
                break;
            }
            match c {
                '\\' => {
                    self.pos += 1;
                    match self.cur() {
                        None => lit.push('\\'),
                        Some('\n') => self.pos += 1,
                        Some(ch) => {
                            flush!();
                            parts.push(Part::Quoted(ch.to_string()));
                            self.pos += 1;
                        }
                    }
                }
                '\'' => {
                    flush!();
                    self.pos += 1;
                    let mut s = String::new();
                    loop {
                        match self.cur() {
                            None => return self.eof_err("guillemet simple non fermé"),
                            Some('\'') => {
                                self.pos += 1;
                                break;
                            }
                            Some(ch) => {
                                s.push(ch);
                                self.pos += 1;
                            }
                        }
                    }
                    parts.push(Part::Quoted(s));
                }
                '"' => {
                    flush!();
                    self.pos += 1;
                    let inner = self.lex_dquoted()?;
                    parts.push(Part::DQuoted(inner));
                }
                '$' => {
                    flush!();
                    let p = self.lex_dollar(false)?;
                    parts.push(p);
                }
                '`' => {
                    flush!();
                    let p = self.lex_backtick()?;
                    parts.push(p);
                }
                '~' if self.pos == start
                    || (assign_like
                        && matches!(lit.chars().last(), Some('=') | Some(':'))
                        && parts.is_empty()) =>
                {
                    // ~ ou ~nom, terminé par / ou fin de mot
                    let mut j = self.pos + 1;
                    let mut name = String::new();
                    while j < self.src.len() {
                        let ch = self.src[j];
                        if ch == '/' || ch == ':' || is_meta(ch) {
                            break;
                        }
                        if !(ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '.' || ch == '+') {
                            name.clear();
                            j = usize::MAX;
                            break;
                        }
                        name.push(ch);
                        j += 1;
                    }
                    if j == usize::MAX {
                        lit.push('~');
                        self.pos += 1;
                    } else {
                        flush!();
                        parts.push(Part::Tilde(name));
                        self.pos = j;
                    }
                }
                '{' => {
                    brace_depth += 1;
                    lit.push(c);
                    self.pos += 1;
                }
                '}' => {
                    brace_depth = brace_depth.saturating_sub(1);
                    lit.push(c);
                    self.pos += 1;
                }
                '=' => {
                    if !assign_like && parts.is_empty() && is_name(lit.trim_end_matches('+')) {
                        assign_like = true;
                    }
                    lit.push(c);
                    self.pos += 1;
                }
                _ => {
                    lit.push(c);
                    self.pos += 1;
                }
            }
        }
        let _ = brace_depth;
        flush!();
        Ok((parts, self.text(start, self.pos)))
    }

    /// Contenu entre guillemets doubles, le " ouvrant déjà consommé.
    fn lex_dquoted(&mut self) -> PResult<Vec<Part>> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let c = match self.cur() {
                None => return self.eof_err("guillemet double non fermé"),
                Some(c) => c,
            };
            match c {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    match self.at(1) {
                        Some(ch @ ('$' | '`' | '"' | '\\')) => {
                            lit.push(ch);
                            self.pos += 2;
                        }
                        Some('\n') => self.pos += 2,
                        Some(_) => {
                            lit.push('\\');
                            self.pos += 1;
                        }
                        None => return self.eof_err("guillemet double non fermé"),
                    }
                }
                '$' => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(self.lex_dollar(true)?);
                }
                '`' => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(self.lex_backtick()?);
                }
                _ => {
                    lit.push(c);
                    self.pos += 1;
                }
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(parts)
    }

    fn lex_backtick(&mut self) -> PResult<Part> {
        self.pos += 1;
        let mut s = String::new();
        loop {
            match self.cur() {
                None => return self.eof_err("accent grave non fermé"),
                Some('`') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    match self.at(1) {
                        Some(ch @ ('`' | '\\' | '$')) => {
                            s.push(ch);
                            self.pos += 2;
                        }
                        _ => {
                            s.push('\\');
                            self.pos += 1;
                        }
                    }
                }
                Some(ch) => {
                    s.push(ch);
                    self.pos += 1;
                }
            }
        }
        let mut sub = Parser::new(&s);
        sub.aliases = self.aliases.clone();
        let list = sub.parse_program().map_err(|mut e| {
            e.pos = self.pos;
            e
        })?;
        Ok(Part::CmdSub(Rc::new(list)))
    }

    /// Analyse une liste de commandes jusqu'à la `)` fermante (consommée).
    fn parse_nested_until_rparen(&mut self) -> PResult<List> {
        let saved_peek = self.peeked.take();
        debug_assert!(saved_peek.is_none());
        let list = self.parse_list(&[])?;
        match self.next()? {
            Tok::Op(Op::RParen) => Ok(list),
            Tok::Eof => self.eof_err("parenthèse non fermée"),
            _ => self.err("« ) » attendue"),
        }
    }

    fn lex_dollar(&mut self, in_dq: bool) -> PResult<Part> {
        // self.cur() == '$'
        let next = self.at(1);
        match next {
            Some('(') if self.at(2) == Some('(') => {
                // $(( ... ))
                let save = self.pos;
                self.pos += 3;
                if let Some(content) = self.scan_arith_body() {
                    let parts = parse_dq_like(&content)?;
                    return Ok(Part::Arith(parts));
                }
                // Pas un calcul : c'est $( (sous-shell) )
                self.pos = save + 2;
                let list = self.parse_nested_until_rparen()?;
                Ok(Part::CmdSub(Rc::new(list)))
            }
            Some('(') => {
                self.pos += 2;
                let list = self.parse_nested_until_rparen()?;
                Ok(Part::CmdSub(Rc::new(list)))
            }
            Some('{') => {
                self.pos += 2;
                self.lex_brace_param(in_dq)
            }
            Some('\'') if !in_dq => {
                self.pos += 2;
                let mut s = String::new();
                loop {
                    match self.cur() {
                        None => return self.eof_err("guillemet $'…' non fermé"),
                        Some('\'') => {
                            self.pos += 1;
                            break;
                        }
                        Some('\\') => {
                            self.pos += 1;
                            let (ch, n) = ansi_escape(&self.src[self.pos..]);
                            s.push_str(&ch);
                            self.pos += n;
                        }
                        Some(ch) => {
                            s.push(ch);
                            self.pos += 1;
                        }
                    }
                }
                Ok(Part::Quoted(s))
            }
            Some('"') if !in_dq => {
                self.pos += 2;
                Ok(Part::DQuoted(self.lex_dquoted()?))
            }
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                self.pos += 1;
                let mut name = String::new();
                while let Some(ch) = self.cur() {
                    if ch == '_' || ch.is_ascii_alphanumeric() {
                        name.push(ch);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Ok(Part::Param(Box::new(ParamExp::simple(name, ParamOp::Plain))))
            }
            Some(c) if c.is_ascii_digit() || "?$#@*!-".contains(c) => {
                self.pos += 2;
                Ok(Part::Param(Box::new(ParamExp::simple(c.to_string(), ParamOp::Plain))))
            }
            _ => {
                self.pos += 1;
                Ok(Part::Lit("$".into()))
            }
        }
    }

    /// Après `((` : renvoie le contenu jusqu'au `))` correspondant.
    fn scan_arith_body(&mut self) -> Option<String> {
        let mut depth = 0i32;
        let mut s = String::new();
        let mut i = self.pos;
        while i < self.src.len() {
            let c = self.src[i];
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                if depth == 0 {
                    if self.src.get(i + 1) == Some(&')') {
                        self.pos = i + 2;
                        return Some(s);
                    }
                    return None;
                }
                depth -= 1;
            }
            s.push(c);
            i += 1;
        }
        None
    }

    fn lex_brace_param(&mut self, in_dq: bool) -> PResult<Part> {
        // après "${"
        let mut length = false;
        if self.cur() == Some('#') && self.at(1).is_some_and(|c| c != '}') {
            length = true;
            self.pos += 1;
        }
        let mut indirect = false;
        if self.cur() == Some('!') && self.at(1).is_some_and(|c| c == '_' || c.is_ascii_alphanumeric()) {
            indirect = true;
            self.pos += 1;
        }
        let mut name = String::new();
        match self.cur() {
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                while let Some(ch) = self.cur() {
                    if ch == '_' || ch.is_ascii_alphanumeric() {
                        name.push(ch);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while let Some(ch) = self.cur() {
                    if ch.is_ascii_digit() {
                        name.push(ch);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            Some(c) if "?$#@*!-".contains(c) => {
                name.push(c);
                self.pos += 1;
            }
            None => return self.eof_err("accolade ${ non fermée"),
            _ => return self.err("substitution ${…} invalide"),
        }
        let mut index = None;
        if self.cur() == Some('[') {
            self.pos += 1;
            let st = self.pos;
            let mut depth = 0;
            loop {
                match self.cur() {
                    None => return self.eof_err("crochet [ non fermé"),
                    Some('[') => depth += 1,
                    Some(']') if depth == 0 => break,
                    Some(']') => depth -= 1,
                    _ => {}
                }
                self.pos += 1;
            }
            let txt = self.text(st, self.pos);
            self.pos += 1;
            index = Some(match txt.as_str() {
                "@" => Index::All(false),
                "*" => Index::All(true),
                _ => Index::Expr(parse_dq_like(&txt)?),
            });
        }
        if length {
            if self.cur() != Some('}') {
                return self.err("substitution ${#…} invalide");
            }
            self.pos += 1;
            return Ok(Part::Param(Box::new(ParamExp {
                name,
                op: ParamOp::Length,
                index,
                indirect,
            })));
        }
        let c = match self.cur() {
            None => return self.eof_err("accolade ${ non fermée"),
            Some(c) => c,
        };
        let op = if c == '}' {
            self.pos += 1;
            ParamOp::Plain
        } else {
            let colon = c == ':';
            let c2 = if colon { self.at(1) } else { Some(c) };
            match c2 {
                Some(o @ ('-' | '=' | '?' | '+')) => {
                    self.pos += if colon { 2 } else { 1 };
                    let w = self.lex_param_word(in_dq, &['}'])?;
                    self.pos += 1;
                    match o {
                        '-' => ParamOp::Default(colon, w),
                        '=' => ParamOp::Assign(colon, w),
                        '?' => ParamOp::Error(colon, w),
                        _ => ParamOp::Alt(colon, w),
                    }
                }
                _ if colon => {
                    self.pos += 1;
                    let off = self.lex_param_word(in_dq, &[':', '}'])?;
                    let len = if self.cur() == Some(':') {
                        self.pos += 1;
                        Some(self.lex_param_word(in_dq, &['}'])?)
                    } else {
                        None
                    };
                    self.pos += 1;
                    ParamOp::Substr(off, len)
                }
                Some('#') | Some('%') => {
                    let prefix = c == '#';
                    self.pos += 1;
                    let longest = self.cur() == Some(c);
                    if longest {
                        self.pos += 1;
                    }
                    let w = self.lex_param_word(in_dq, &['}'])?;
                    self.pos += 1;
                    if prefix {
                        ParamOp::TrimPrefix(longest, w)
                    } else {
                        ParamOp::TrimSuffix(longest, w)
                    }
                }
                Some('/') => {
                    self.pos += 1;
                    let mut all = false;
                    let mut anchor = None;
                    match self.cur() {
                        Some('/') => {
                            all = true;
                            self.pos += 1;
                        }
                        Some(a @ ('#' | '%')) => {
                            anchor = Some(a);
                            self.pos += 1;
                        }
                        _ => {}
                    }
                    let pat = self.lex_param_word(in_dq, &['/', '}'])?;
                    let rep = if self.cur() == Some('/') {
                        self.pos += 1;
                        self.lex_param_word(in_dq, &['}'])?
                    } else {
                        Vec::new()
                    };
                    self.pos += 1;
                    ParamOp::Replace(all, anchor, pat, rep)
                }
                Some(o @ ('^' | ',')) => {
                    self.pos += 1;
                    let all = self.cur() == Some(o);
                    if all {
                        self.pos += 1;
                    }
                    let pat = self.lex_param_word(in_dq, &['}'])?;
                    self.pos += 1;
                    if o == '^' {
                        ParamOp::Upper(all, pat)
                    } else {
                        ParamOp::Lower(all, pat)
                    }
                }
                _ => return self.err("substitution ${…} invalide"),
            }
        };
        Ok(Part::Param(Box::new(ParamExp { name, op, index, indirect })))
    }

    /// Mot à l'intérieur de ${…} jusqu'à un des caractères `stops` (non consommé).
    fn lex_param_word(&mut self, in_dq: bool, stops: &[char]) -> PResult<Word> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut depth = 0usize;
        loop {
            let c = match self.cur() {
                None => return self.eof_err("accolade ${ non fermée"),
                Some(c) => c,
            };
            if depth == 0 && stops.contains(&c) {
                break;
            }
            match c {
                '{' => {
                    depth += 1;
                    lit.push(c);
                    self.pos += 1;
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    lit.push(c);
                    self.pos += 1;
                }
                '\\' => {
                    self.pos += 1;
                    if let Some(ch) = self.cur() {
                        if !lit.is_empty() {
                            parts.push(Part::Lit(std::mem::take(&mut lit)));
                        }
                        if in_dq && !matches!(ch, '$' | '`' | '"' | '\\' | '}') {
                            // \c reste tel quel : motif échappé, ou texte « \c »
                            parts.push(Part::Lit(format!("\\{ch}")));
                        } else {
                            parts.push(Part::Quoted(ch.to_string()));
                        }
                        self.pos += 1;
                    }
                }
                '\'' if !in_dq => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    self.pos += 1;
                    let mut s = String::new();
                    loop {
                        match self.cur() {
                            None => return self.eof_err("guillemet simple non fermé"),
                            Some('\'') => {
                                self.pos += 1;
                                break;
                            }
                            Some(ch) => {
                                s.push(ch);
                                self.pos += 1;
                            }
                        }
                    }
                    parts.push(Part::Quoted(s));
                }
                '"' => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    self.pos += 1;
                    parts.push(Part::DQuoted(self.lex_dquoted()?));
                }
                '$' => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(self.lex_dollar(in_dq)?);
                }
                '`' => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(self.lex_backtick()?);
                }
                _ => {
                    lit.push(c);
                    self.pos += 1;
                }
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        // Même entre guillemets doubles, le motif de ${x#motif} reste un motif :
        // le texte reste donc Lit (l'expansion le protège selon le contexte).
        Ok(parts)
    }

    // ------------------------------------------------------------------
    // Gestion des jetons
    // ------------------------------------------------------------------

    fn peek(&mut self) -> PResult<&Tok> {
        if self.peeked.is_none() {
            let t = self.lex()?;
            self.peeked = Some(t);
        }
        Ok(&self.peeked.as_ref().unwrap().0)
    }

    /// Annule un peek : le lexer repart du début du jeton.
    fn unpeek(&mut self) {
        if let Some((_, start, _)) = self.peeked.take() {
            self.pos = start;
        }
    }

    fn next(&mut self) -> PResult<Tok> {
        let t = match self.peeked.take() {
            Some((t, _, _)) => t,
            None => self.lex()?.0,
        };
        if matches!(t, Tok::Newline) {
            self.read_heredocs()?;
        }
        Ok(t)
    }

    fn read_heredocs(&mut self) -> PResult<()> {
        let pending = std::mem::take(&mut self.pending);
        for h in pending {
            let mut body = String::new();
            let mut found = false;
            while self.pos < self.src.len() {
                let mut line = String::new();
                while let Some(c) = self.cur() {
                    self.pos += 1;
                    if c == '\n' {
                        break;
                    }
                    line.push(c);
                }
                let cmp: &str = if h.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if cmp == h.delim {
                    found = true;
                    break;
                }
                body.push_str(cmp);
                body.push('\n');
            }
            if !found {
                return self.eof_err(format!("here-document « {} » non terminé", h.delim));
            }
            *h.body.borrow_mut() = if h.quoted {
                HereBody::Literal(body)
            } else {
                HereBody::Expand(parse_heredoc_body(&body)?)
            };
        }
        Ok(())
    }

    fn peek_reserved(&mut self) -> PResult<Option<String>> {
        match self.peek()? {
            Tok::Word(w, _) => Ok(plain_word(w).filter(|s| RESERVED.contains(&s.as_str()))),
            _ => Ok(None),
        }
    }

    fn expect_reserved(&mut self, kw: &str) -> PResult<()> {
        self.skip_newlines()?;
        match self.peek_reserved()? {
            Some(s) if s == kw => {
                self.next()?;
                Ok(())
            }
            _ => {
                if matches!(self.peek()?, Tok::Eof) {
                    self.eof_err(format!("« {kw} » attendu"))
                } else {
                    self.err(format!("« {kw} » attendu"))
                }
            }
        }
    }

    fn skip_newlines(&mut self) -> PResult<()> {
        while matches!(self.peek()?, Tok::Newline) {
            self.next()?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Grammaire
    // ------------------------------------------------------------------

    pub fn parse_program(&mut self) -> PResult<List> {
        let list = self.parse_list(&[])?;
        match self.peek()? {
            Tok::Eof => {
                if !self.pending.is_empty() {
                    return self.eof_err("here-document non terminé");
                }
                Ok(list)
            }
            Tok::Op(Op::RParen) => self.err("« ) » inattendue"),
            Tok::Op(Op::DSemi) => self.err("« ;; » inattendu"),
            Tok::Word(_, raw) => {
                let r = raw.clone();
                self.err(format!("mot inattendu « {r} »"))
            }
            t => {
                let t = format!("{t:?}");
                self.err(format!("jeton inattendu {t}"))
            }
        }
    }

    /// Liste de commandes ; s'arrête sur un mot réservé de fin, `)`, `;;` ou EOF.
    fn parse_list(&mut self, _stops: &[&str]) -> PResult<List> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines()?;
            match self.peek()? {
                Tok::Eof | Tok::Op(Op::RParen) | Tok::Op(Op::DSemi) | Tok::Op(Op::SemiAmp) => break,
                _ => {}
            }
            if let Some(r) = self.peek_reserved()? {
                if matches!(
                    r.as_str(),
                    "then" | "else" | "elif" | "fi" | "do" | "done" | "esac" | "}" | "in" | "]]"
                ) {
                    break;
                }
            }
            let and_or = self.parse_and_or()?;
            let mut background = false;
            match self.peek()? {
                Tok::Op(Op::Semi) => {
                    self.next()?;
                }
                Tok::Op(Op::Amp) => {
                    self.next()?;
                    background = true;
                }
                Tok::Newline => {
                    self.next()?;
                }
                _ => {
                    items.push(ListItem { and_or, background });
                    break;
                }
            }
            items.push(ListItem { and_or, background });
        }
        Ok(items)
    }

    fn parse_and_or(&mut self) -> PResult<AndOr> {
        let first = self.parse_pipeline()?;
        let mut rest = Vec::new();
        loop {
            let is_and = match self.peek()? {
                Tok::Op(Op::AndIf) => true,
                Tok::Op(Op::OrIf) => false,
                _ => break,
            };
            self.next()?;
            self.skip_newlines()?;
            rest.push((is_and, self.parse_pipeline()?));
        }
        Ok(AndOr { first, rest })
    }

    fn parse_pipeline(&mut self) -> PResult<Pipeline> {
        self.skip_blanks();
        let start = self.peeked.as_ref().map(|p| p.1).unwrap_or(self.pos);
        let mut negate = false;
        let mut timed = false;
        loop {
            match self.peek_reserved()?.as_deref() {
                Some("!") => {
                    self.next()?;
                    negate = true;
                }
                Some("time") if !timed => {
                    self.next()?;
                    timed = true;
                    // time -p : format POSIX (identique ici)
                    if let Tok::Word(w, _) = self.peek()? {
                        if plain_word(w).as_deref() == Some("-p") {
                            self.next()?;
                        }
                    }
                    // « time » seul en fin de ligne
                    if matches!(self.peek()?, Tok::Eof | Tok::Newline | Tok::Op(Op::Semi)) {
                        let text = String::from("time");
                        return Ok(Pipeline { negate, timed, cmds: vec![Command::Simple(SimpleCommand { assigns: vec![], words: vec![vec![Part::Lit(":".into())]], redirs: vec![] })], text });
                    }
                }
                _ => break,
            }
        }
        let mut cmds = vec![self.parse_command()?];
        loop {
            let both = match self.peek()? {
                Tok::Op(Op::Pipe) => false,
                Tok::Op(Op::PipeAmp) => true,
                _ => break,
            };
            self.next()?;
            if both {
                // cmd |& cmd  ==  cmd 2>&1 | cmd
                let r = Redir {
                    fd: Some(2),
                    op: RedirOp::DupOut,
                    target: RedirTarget::Word(vec![Part::Lit("1".into())]),
                };
                match cmds.last_mut().unwrap() {
                    Command::Simple(s) => s.redirs.push(r),
                    Command::Compound(_, rs) => rs.push(r),
                    Command::FuncDef(..) => {}
                }
            }
            self.skip_newlines()?;
            cmds.push(self.parse_command()?);
        }
        let end = self.peeked.as_ref().map(|p| p.1).unwrap_or(self.pos);
        let text = self.text(start, end).trim().to_string();
        Ok(Pipeline { negate, timed, cmds, text })
    }

    fn parse_command(&mut self) -> PResult<Command> {
        self.depth += 1;
        if self.depth > 2000 {
            self.depth = 0;
            return self.err("imbrication trop profonde");
        }
        let r = self.parse_command_inner();
        self.depth = self.depth.saturating_sub(1);
        r
    }

    fn parse_command_inner(&mut self) -> PResult<Command> {
        // (( expr ))
        if self.peeked.is_none() {
            self.skip_blanks();
        }
        self.unpeek();
        self.skip_blanks();
        if self.starts_with("((") {
            let save = self.pos;
            self.pos += 2;
            if let Some(body) = self.scan_arith_body() {
                let parts = parse_dq_like(&body)?;
                let redirs = self.parse_redirs()?;
                return Ok(Command::Compound(Box::new(Compound::Arith(parts)), redirs));
            }
            self.pos = save;
        }
        let tok = self.peek()?.clone();
        match tok {
            Tok::Op(Op::LParen) => {
                self.next()?;
                let list = self.parse_list(&[])?;
                match self.next()? {
                    Tok::Op(Op::RParen) => {}
                    Tok::Eof => return self.eof_err("« ) » attendue"),
                    _ => return self.err("« ) » attendue"),
                }
                let redirs = self.parse_redirs()?;
                Ok(Command::Compound(Box::new(Compound::Subshell(list)), redirs))
            }
            Tok::Word(ref w, _) => {
                if let Some(kw) = plain_word(w) {
                    match kw.as_str() {
                        "{" | "if" | "while" | "until" | "for" | "case" | "[[" => {
                            let c = self.parse_compound(&kw)?;
                            let redirs = self.parse_redirs()?;
                            return Ok(Command::Compound(Box::new(c), redirs));
                        }
                        "function" => {
                            self.next()?;
                            let name = match self.next()? {
                                Tok::Word(w, raw) if plain_word(&w).is_some() => raw,
                                Tok::Eof => return self.eof_err("nom de fonction attendu"),
                                _ => return self.err("nom de fonction attendu"),
                            };
                            if matches!(self.peek()?, Tok::Op(Op::LParen)) {
                                self.next()?;
                                match self.next()? {
                                    Tok::Op(Op::RParen) => {}
                                    _ => return self.err("« ) » attendue"),
                                }
                            }
                            return self.parse_func_body(name);
                        }
                        "then" | "else" | "elif" | "fi" | "do" | "done" | "esac" | "}" | "in" => {
                            return self.err(format!("« {kw} » inattendu"));
                        }
                        _ => {}
                    }
                }
                self.parse_simple()
            }
            Tok::Op(Op::Redir(..)) | Tok::Op(Op::HereDoc(..)) => self.parse_simple(),
            Tok::Eof => self.eof_err("commande attendue"),
            Tok::Newline => self.eof_err("commande attendue"),
            Tok::Op(op) => {
                let s = op_str(&op);
                self.err(format!("erreur de syntaxe près de « {s} »"))
            }
        }
    }

    /// Éléments d'un tableau après « ( » jusqu'à « ) ».
    fn parse_array_items(&mut self) -> PResult<Vec<Word>> {
        let mut items = Vec::new();
        loop {
            match self.next()? {
                Tok::Word(w, _) => items.push(w),
                Tok::Newline => {}
                Tok::Op(Op::RParen) => break,
                Tok::Eof => return self.eof_err("« ) » attendue pour fermer le tableau"),
                _ => return self.err("élément de tableau invalide"),
            }
        }
        Ok(items)
    }

    fn parse_func_body(&mut self, name: String) -> PResult<Command> {
        self.skip_newlines()?;
        let kw = match self.peek()? {
            Tok::Word(w, _) => plain_word(w),
            Tok::Op(Op::LParen) => Some("(".to_string()),
            Tok::Eof => return self.eof_err("corps de fonction attendu"),
            _ => None,
        };
        let body = match kw.as_deref() {
            Some("(") => {
                self.next()?;
                let list = self.parse_list(&[])?;
                match self.next()? {
                    Tok::Op(Op::RParen) => {}
                    Tok::Eof => return self.eof_err("« ) » attendue"),
                    _ => return self.err("« ) » attendue"),
                }
                Compound::Subshell(list)
            }
            Some(k @ ("{" | "if" | "while" | "until" | "for" | "case" | "[[")) => {
                let k = k.to_string();
                self.parse_compound(&k)?
            }
            _ => return self.err("le corps d'une fonction doit être un bloc { … }"),
        };
        let redirs = self.parse_redirs()?;
        Ok(Command::FuncDef(name, Rc::new(FuncBody { body, redirs })))
    }

    fn parse_compound(&mut self, kw: &str) -> PResult<Compound> {
        self.next()?; // le mot-clé
        match kw {
            "{" => {
                let list = self.parse_list(&[])?;
                self.expect_reserved("}")?;
                Ok(Compound::Brace(list))
            }
            "if" => {
                let mut branches = Vec::new();
                let mut else_body = None;
                let cond = self.parse_list(&[])?;
                self.expect_reserved("then")?;
                let body = self.parse_list(&[])?;
                branches.push((cond, body));
                loop {
                    self.skip_newlines()?;
                    match self.peek_reserved()?.as_deref() {
                        Some("elif") => {
                            self.next()?;
                            let c = self.parse_list(&[])?;
                            self.expect_reserved("then")?;
                            let b = self.parse_list(&[])?;
                            branches.push((c, b));
                        }
                        Some("else") => {
                            self.next()?;
                            else_body = Some(self.parse_list(&[])?);
                            self.expect_reserved("fi")?;
                            break;
                        }
                        Some("fi") => {
                            self.next()?;
                            break;
                        }
                        _ => {
                            if matches!(self.peek()?, Tok::Eof) {
                                return self.eof_err("« fi » attendu");
                            }
                            return self.err("« fi » attendu");
                        }
                    }
                }
                Ok(Compound::If {
                    branches,
                    else_body,
                })
            }
            "while" | "until" => {
                let cond = self.parse_list(&[])?;
                self.expect_reserved("do")?;
                let body = self.parse_list(&[])?;
                self.expect_reserved("done")?;
                Ok(Compound::Loop {
                    until: kw == "until",
                    cond,
                    body,
                })
            }
            "for" => {
                // for (( init; cond; step ))
                self.unpeek();
                self.skip_blanks();
                if self.starts_with("((") {
                    self.pos += 2;
                    let body = match self.scan_arith_body() {
                        Some(b) => b,
                        None => return self.eof_err("« )) » attendu"),
                    };
                    let segs: Vec<&str> = body.splitn(3, ';').collect();
                    if segs.len() != 3 {
                        return self.err("for (( init ; condition ; pas )) attendu");
                    }
                    let init = parse_dq_like(segs[0])?;
                    let cond = parse_dq_like(segs[1])?;
                    let step = parse_dq_like(segs[2])?;
                    if matches!(self.peek()?, Tok::Op(Op::Semi)) {
                        self.next()?;
                    }
                    self.skip_newlines()?;
                    let body = self.parse_do_group()?;
                    return Ok(Compound::ArithFor {
                        init,
                        cond,
                        step,
                        body,
                    });
                }
                let var = match self.next()? {
                    Tok::Word(w, raw) if plain_word(&w).is_some_and(|s| is_name(&s)) => raw,
                    Tok::Eof => return self.eof_err("nom de variable attendu après for"),
                    _ => return self.err("nom de variable attendu après for"),
                };
                let mut items = None;
                self.skip_newlines()?;
                if self.peek_reserved()?.as_deref() == Some("in") {
                    self.next()?;
                    let mut ws = Vec::new();
                    loop {
                        match self.peek()? {
                            Tok::Word(..) => {
                                if let Tok::Word(w, _) = self.next()? {
                                    ws.push(w);
                                }
                            }
                            _ => break,
                        }
                    }
                    items = Some(ws);
                }
                match self.peek()? {
                    Tok::Op(Op::Semi) | Tok::Newline => {
                        self.next()?;
                    }
                    _ => {}
                }
                self.skip_newlines()?;
                let body = self.parse_do_group()?;
                Ok(Compound::For { var, items, body })
            }
            "case" => {
                let word = match self.next()? {
                    Tok::Word(w, _) => w,
                    Tok::Eof => return self.eof_err("mot attendu après case"),
                    _ => return self.err("mot attendu après case"),
                };
                self.expect_reserved("in")?;
                let mut arms = Vec::new();
                loop {
                    self.skip_newlines()?;
                    if self.peek_reserved()?.as_deref() == Some("esac") {
                        self.next()?;
                        break;
                    }
                    if matches!(self.peek()?, Tok::Eof) {
                        return self.eof_err("« esac » attendu");
                    }
                    if matches!(self.peek()?, Tok::Op(Op::LParen)) {
                        self.next()?;
                    }
                    let mut patterns = Vec::new();
                    loop {
                        match self.next()? {
                            Tok::Word(w, _) => patterns.push(w),
                            Tok::Eof => return self.eof_err("motif attendu"),
                            _ => return self.err("motif de case attendu"),
                        }
                        match self.next()? {
                            Tok::Op(Op::Pipe) => continue,
                            Tok::Op(Op::RParen) => break,
                            Tok::Eof => return self.eof_err("« ) » attendue"),
                            _ => return self.err("« ) » attendue après le motif"),
                        }
                    }
                    let body = self.parse_list(&[])?;
                    let mut fallthrough = false;
                    match self.peek()? {
                        Tok::Op(Op::DSemi) => {
                            self.next()?;
                        }
                        Tok::Op(Op::SemiAmp) => {
                            self.next()?;
                            fallthrough = true;
                        }
                        _ => {}
                    }
                    arms.push(CaseArm {
                        patterns,
                        body,
                        fallthrough,
                    });
                }
                Ok(Compound::Case { word, arms })
            }
            "[[" => {
                let mut toks: Vec<CondTok> = Vec::new();
                loop {
                    if matches!(toks.last(), Some(CondTok::W(_, r)) if r == "=~") {
                        self.skip_blanks();
                        let (w, raw) = self.lex_word_mode(true)?;
                        if !raw.is_empty() {
                            toks.push(CondTok::W(w, raw));
                            continue;
                        }
                    }
                    let t = self.next()?;
                    match t {
                        Tok::Word(w, raw) => {
                            if raw == "]]" {
                                break;
                            }
                            toks.push(CondTok::W(w, raw));
                        }
                        Tok::Op(Op::AndIf) => toks.push(CondTok::And),
                        Tok::Op(Op::OrIf) => toks.push(CondTok::Or),
                        Tok::Op(Op::LParen) => toks.push(CondTok::LP),
                        Tok::Op(Op::RParen) => toks.push(CondTok::RP),
                        Tok::Op(Op::Redir(RedirOp::In, None)) => {
                            toks.push(CondTok::W(vec![Part::Lit("<".into())], "<".into()))
                        }
                        Tok::Op(Op::Redir(RedirOp::Out, None)) => {
                            toks.push(CondTok::W(vec![Part::Lit(">".into())], ">".into()))
                        }
                        Tok::Newline => {}
                        Tok::Eof => return self.eof_err("« ]] » attendu"),
                        _ => return self.err("expression [[ … ]] invalide"),
                    }
                }
                let mut i = 0;
                let e = cond_or(&toks, &mut i).map_err(|m| ParseError {
                    msg: m,
                    pos: self.pos,
                    incomplete: false,
                })?;
                if i != toks.len() {
                    return self.err("expression [[ … ]] invalide");
                }
                Ok(Compound::Cond(e))
            }
            _ => unreachable!(),
        }
    }

    fn parse_do_group(&mut self) -> PResult<List> {
        if self.peek_reserved()?.as_deref() == Some("{") {
            self.next()?;
            let body = self.parse_list(&[])?;
            self.expect_reserved("}")?;
            return Ok(body);
        }
        self.expect_reserved("do")?;
        let body = self.parse_list(&[])?;
        self.expect_reserved("done")?;
        Ok(body)
    }

    fn parse_redirs(&mut self) -> PResult<Vec<Redir>> {
        let mut rs = Vec::new();
        while let Some(r) = self.try_redir()? {
            rs.push(r);
        }
        Ok(rs)
    }

    fn try_redir(&mut self) -> PResult<Option<Redir>> {
        match self.peek()?.clone() {
            Tok::Op(Op::Redir(op, fd)) => {
                self.next()?;
                let target = match self.next()? {
                    Tok::Word(w, _) => w,
                    Tok::Eof | Tok::Newline => {
                        return self.err("fichier attendu après la redirection")
                    }
                    _ => return self.err("fichier attendu après la redirection"),
                };
                Ok(Some(Redir {
                    fd,
                    op,
                    target: RedirTarget::Word(target),
                }))
            }
            Tok::Op(Op::HereDoc(strip, fd)) => {
                self.next()?;
                let (w, raw) = match self.next()? {
                    Tok::Word(w, raw) => (w, raw),
                    _ => return self.err("délimiteur de here-document attendu"),
                };
                let quoted = w
                    .iter()
                    .any(|p| matches!(p, Part::Quoted(_) | Part::DQuoted(_)));
                let delim = literal_text(&w).unwrap_or(raw);
                let body = Rc::new(RefCell::new(HereBody::Pending));
                self.pending.push(PendingHere {
                    delim,
                    strip_tabs: strip,
                    quoted,
                    body: body.clone(),
                });
                Ok(Some(Redir {
                    fd,
                    op: RedirOp::In,
                    target: RedirTarget::Here(body),
                }))
            }
            _ => Ok(None),
        }
    }

    fn parse_simple(&mut self) -> PResult<Command> {
        let mut cmd = SimpleCommand {
            assigns: Vec::new(),
            words: Vec::new(),
            redirs: Vec::new(),
        };
        loop {
            if let Some(r) = self.try_redir()? {
                cmd.redirs.push(r);
                continue;
            }
            let (w, raw, start) = match self.peek()? {
                Tok::Word(..) => {
                    let start = self.peeked.as_ref().unwrap().1;
                    match self.next()? {
                        Tok::Word(w, raw) => (w, raw, start),
                        _ => unreachable!(),
                    }
                }
                _ => break,
            };
            let alias_pos = cmd.words.is_empty() || std::mem::take(&mut self.alias_next);
            if alias_pos && !cmd.words.is_empty() {
                if let (Some(aliases), Some(name)) = (self.aliases.clone(), plain_word(&w)) {
                    if let Some(val) = aliases.get(&name) {
                        if self.alias_budget > 0 {
                            self.alias_budget -= 1;
                            self.alias_next = val.ends_with(' ');
                            let end = self.pos;
                            let mut ns: Vec<char> = self.src[..start].to_vec();
                            ns.extend(val.chars());
                            ns.push(' ');
                            ns.extend_from_slice(&self.src[end..]);
                            self.src = ns;
                            self.pos = start;
                            continue;
                        }
                    }
                }
            }
            if cmd.words.is_empty() {
                if let Some(mut a) = split_assign(&w) {
                    if a.value.is_empty() && a.index.is_none() && self.peeked.is_none() && self.cur() == Some('(') {
                        self.pos += 1;
                        a.array = Some(self.parse_array_items()?);
                    }
                    cmd.assigns.push(a);
                    continue;
                }
                // Expansion des alias en position de commande
                // (pas sur « nom() » : c'est une définition de fonction)
                let funcdef_next = {
                    let mut j = self.pos;
                    while matches!(self.src.get(j), Some(' ') | Some('\t')) {
                        j += 1;
                    }
                    let mut k = j + 1;
                    while matches!(self.src.get(k), Some(' ') | Some('\t')) {
                        k += 1;
                    }
                    self.peeked.is_none() && self.src.get(j) == Some(&'(') && self.src.get(k) == Some(&')')
                };
                if let (Some(aliases), false) = (self.aliases.clone(), funcdef_next) {
                    if let Some(name) = plain_word(&w) {
                        if let Some(val) = aliases.get(&name) {
                            if self.alias_budget > 0 && self.last_alias.as_deref() != Some(&name) {
                                self.alias_budget -= 1;
                                self.alias_next = val.ends_with(' ');
                                self.last_alias = Some(name);
                                let end = self.pos;
                                let mut ns: Vec<char> = self.src[..start].to_vec();
                                ns.extend(val.chars());
                                ns.push(' ');
                                ns.extend_from_slice(&self.src[end..]);
                                self.src = ns;
                                self.pos = start;
                                continue;
                            }
                        }
                    }
                }
                // Définition de fonction : nom ( )
                if plain_word(&w).is_some() && matches!(self.peek()?, Tok::Op(Op::LParen)) {
                    let save_peek = self.peeked.clone();
                    self.next()?;
                    if matches!(self.peek()?, Tok::Op(Op::RParen)) {
                        self.next()?;
                        self.last_alias = None;
                        return self.parse_func_body(raw);
                    }
                    let _ = save_peek;
                    return self.err("erreur de syntaxe près de « ( »");
                }
            }
            self.last_alias = None;
            // local a=(…), declare -a a=(…)
            if self.peeked.is_none() && self.cur() == Some('(') {
                let decl = cmd.words.first().and_then(plain_word).is_some_and(|c| {
                    matches!(c.as_str(), "local" | "declare" | "typeset" | "readonly" | "export")
                });
                if decl {
                    if let Some(a) = split_assign(&w) {
                        if a.value.is_empty() && a.index.is_none() {
                            self.pos += 1;
                            let items = self.parse_array_items()?;
                            cmd.words.push(vec![Part::ArrayLit(a.name, a.append, items)]);
                            continue;
                        }
                    }
                }
            }
            cmd.words.push(w);
        }
        self.last_alias = None;
        if cmd.words.is_empty() && cmd.assigns.is_empty() && cmd.redirs.is_empty() {
            return match self.peek()? {
                Tok::Eof => self.eof_err("commande attendue"),
                t => {
                    let s = match t {
                        Tok::Op(o) => op_str(o).to_string(),
                        _ => "fin de ligne".into(),
                    };
                    self.err(format!("erreur de syntaxe près de « {s} »"))
                }
            };
        }
        Ok(Command::Simple(cmd))
    }
}

pub fn op_str(op: &Op) -> &'static str {
    match op {
        Op::AndIf => "&&",
        Op::OrIf => "||",
        Op::Semi => ";",
        Op::DSemi => ";;",
        Op::SemiAmp => ";&",
        Op::Amp => "&",
        Op::Pipe => "|",
        Op::PipeAmp => "|&",
        Op::LParen => "(",
        Op::RParen => ")",
        Op::Redir(..) => "redirection",
        Op::HereDoc(..) => "<<",
    }
}

/// Texte d'un mot sans aucune expansion (délimiteur de here-doc).
fn literal_text(w: &Word) -> Option<String> {
    let mut s = String::new();
    for p in w {
        match p {
            Part::Lit(t) | Part::Quoted(t) => s.push_str(t),
            Part::DQuoted(inner) => {
                for q in inner {
                    match q {
                        Part::Lit(t) | Part::Quoted(t) => s.push_str(t),
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
    }
    Some(s)
}

fn split_assign(w: &Word) -> Option<Assign> {
    let first = match w.first() {
        Some(Part::Lit(s)) => s,
        _ => return None,
    };
    let eq = first.find('=')?;
    let (mut name, append) = (&first[..eq], false);
    let mut append = append;
    if let Some(n) = name.strip_suffix('+') {
        name = n;
        append = true;
    }
    let mut index = None;
    if let Some(open) = name.find('[') {
        if name.ends_with(']') && is_name(&name[..open]) {
            index = parse_dq_like(&name[open + 1..name.len() - 1]).ok();
            name = &name[..open];
        }
    }
    if !is_name(name) {
        return None;
    }
    let mut value: Word = Vec::new();
    let rest = &first[eq + 1..];
    if !rest.is_empty() {
        value.push(Part::Lit(rest.to_string()));
    }
    value.extend(w[1..].iter().cloned());
    Some(Assign {
        name: name.to_string(),
        append,
        value,
        index,
        array: None,
    })
}

/// Analyse un texte « comme entre guillemets doubles » (corps de $((…)) ).
pub fn parse_dq_like(s: &str) -> PResult<Vec<Part>> {
    let mut p = Parser::new(s);
    let mut parts = Vec::new();
    let mut lit = String::new();
    while let Some(c) = p.cur() {
        match c {
            '$' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(p.lex_dollar(true)?);
            }
            '`' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(p.lex_backtick()?);
            }
            _ => {
                lit.push(c);
                p.pos += 1;
            }
        }
    }
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    Ok(parts)
}

fn parse_heredoc_body(s: &str) -> PResult<Vec<Part>> {
    let mut p = Parser::new(s);
    let mut parts = Vec::new();
    let mut lit = String::new();
    while let Some(c) = p.cur() {
        match c {
            '\\' => match p.at(1) {
                Some(ch @ ('$' | '`' | '\\')) => {
                    lit.push(ch);
                    p.pos += 2;
                }
                Some('\n') => p.pos += 2,
                _ => {
                    lit.push('\\');
                    p.pos += 1;
                }
            },
            '$' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(p.lex_dollar(true)?);
            }
            '`' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(p.lex_backtick()?);
            }
            _ => {
                lit.push(c);
                p.pos += 1;
            }
        }
    }
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    Ok(parts)
}

/// Séquence d'échappement $'…' : renvoie (texte, nb de caractères consommés).
fn ansi_escape(s: &[char]) -> (String, usize) {
    let c = match s.first() {
        None => return ("\\".into(), 0),
        Some(c) => *c,
    };
    let simple = match c {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'a' => Some('\x07'),
        'b' => Some('\x08'),
        'e' | 'E' => Some('\x1b'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        '\\' => Some('\\'),
        '\'' => Some('\''),
        '"' => Some('"'),
        '?' => Some('?'),
        _ => None,
    };
    if let Some(ch) = simple {
        return (ch.to_string(), 1);
    }
    let read_digits = |start: usize, max: usize, radix: u32| -> (u32, usize) {
        let mut v = 0u32;
        let mut n = 0;
        while n < max {
            match s.get(start + n).and_then(|c| c.to_digit(radix)) {
                Some(d) => {
                    v = v * radix + d;
                    n += 1;
                }
                None => break,
            }
        }
        (v, n)
    };
    match c {
        'x' => {
            let (v, n) = read_digits(1, 2, 16);
            if n == 0 {
                return ("\\x".into(), 1);
            }
            (char::from_u32(v).unwrap_or('?').to_string(), 1 + n)
        }
        'u' | 'U' => {
            let (v, n) = read_digits(1, if c == 'u' { 4 } else { 8 }, 16);
            if n == 0 {
                return (format!("\\{c}"), 1);
            }
            (char::from_u32(v).unwrap_or('?').to_string(), 1 + n)
        }
        '0'..='7' => {
            let (v, n) = read_digits(0, 3, 8);
            (char::from_u32(v).unwrap_or('?').to_string(), n)
        }
        _ => (format!("\\{c}"), 1),
    }
}

// --- [[ … ]] ---------------------------------------------------------------

#[derive(Debug, Clone)]
enum CondTok {
    W(Word, String),
    And,
    Or,
    LP,
    RP,
}

fn cond_or(t: &[CondTok], i: &mut usize) -> Result<CondExpr, String> {
    let mut l = cond_and(t, i)?;
    while matches!(t.get(*i), Some(CondTok::Or)) {
        *i += 1;
        let r = cond_and(t, i)?;
        l = CondExpr::Or(Box::new(l), Box::new(r));
    }
    Ok(l)
}

fn cond_and(t: &[CondTok], i: &mut usize) -> Result<CondExpr, String> {
    let mut l = cond_not(t, i)?;
    while matches!(t.get(*i), Some(CondTok::And)) {
        *i += 1;
        let r = cond_not(t, i)?;
        l = CondExpr::And(Box::new(l), Box::new(r));
    }
    Ok(l)
}

fn cond_not(t: &[CondTok], i: &mut usize) -> Result<CondExpr, String> {
    if let Some(CondTok::W(_, raw)) = t.get(*i) {
        if raw == "!" {
            *i += 1;
            return Ok(CondExpr::Not(Box::new(cond_not(t, i)?)));
        }
    }
    cond_prim(t, i)
}

fn cond_prim(t: &[CondTok], i: &mut usize) -> Result<CondExpr, String> {
    match t.get(*i) {
        Some(CondTok::LP) => {
            *i += 1;
            let e = cond_or(t, i)?;
            if !matches!(t.get(*i), Some(CondTok::RP)) {
                return Err("« ) » attendue dans [[ … ]]".into());
            }
            *i += 1;
            Ok(e)
        }
        Some(CondTok::W(w, raw)) => {
            // opérateur unaire ?
            if raw.starts_with('-') && raw.len() == 2 {
                if let Some(CondTok::W(arg, _)) = t.get(*i + 1) {
                    let is_bin_next = matches!(t.get(*i + 2), Some(CondTok::W(_, r)) if is_cond_binop(r));
                    if !is_bin_next {
                        *i += 2;
                        return Ok(CondExpr::Unary(raw.clone(), arg.clone()));
                    }
                }
            }
            if let Some(CondTok::W(_, op)) = t.get(*i + 1) {
                if is_cond_binop(op) {
                    if let Some(CondTok::W(rhs, _)) = t.get(*i + 2) {
                        let e = CondExpr::Binary(w.clone(), op.clone(), rhs.clone());
                        *i += 3;
                        return Ok(e);
                    }
                    return Err(format!("opérande attendu après « {op} »"));
                }
            }
            *i += 1;
            Ok(CondExpr::Single(w.clone()))
        }
        _ => Err("expression attendue dans [[ … ]]".into()),
    }
}

fn is_cond_binop(s: &str) -> bool {
    matches!(
        s,
        "==" | "=" | "!=" | "=~" | "<" | ">" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "-nt" | "-ot" | "-ef"
    )
}

// --- Jetons pour la coloration syntaxique ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlKind {
    Command,
    Keyword,
    Arg,
    Option,
    String,
    Variable,
    Operator,
    Redirect,
    Comment,
    Assign,
    Error,
}

/// Découpage approximatif mais tolérant d'une ligne, en octets, pour colorer.
pub fn highlight_spans(line: &str) -> Vec<(usize, usize, HlKind)> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let n = chars.len();
    let byte = |i: usize| if i < n { chars[i].0 } else { line.len() };
    let mut out = Vec::new();
    let mut i = 0;
    let mut cmd_pos = true;
    while i < n {
        let c = chars[i].1;
        if c == ' ' || c == '\t' {
            i += 1;
            continue;
        }
        if c == '\n' {
            cmd_pos = true;
            i += 1;
            continue;
        }
        if c == '#' {
            out.push((byte(i), line.len(), HlKind::Comment));
            // jusqu'à la fin de ligne
            let mut j = i;
            while j < n && chars[j].1 != '\n' {
                j += 1;
            }
            out.pop();
            out.push((byte(i), byte(j), HlKind::Comment));
            i = j;
            continue;
        }
        // opérateurs
        let two: String = chars[i..(i + 2).min(n)].iter().map(|x| x.1).collect();
        if ["&&", "||", ";;", "|&"].contains(&two.as_str()) {
            out.push((byte(i), byte(i + 2), HlKind::Operator));
            i += 2;
            cmd_pos = true;
            continue;
        }
        if matches!(c, '|' | ';' | '&' | '(' | ')') {
            out.push((byte(i), byte(i + 1), HlKind::Operator));
            i += 1;
            cmd_pos = c != ')';
            continue;
        }
        if c.is_ascii_digit() || c == '<' || c == '>' {
            let mut j = i;
            while j < n && chars[j].1.is_ascii_digit() {
                j += 1;
            }
            if j < n && (chars[j].1 == '<' || chars[j].1 == '>') && !(j + 1 < n && chars[j + 1].1 == '(') {
                while j < n && matches!(chars[j].1, '<' | '>' | '&' | '|' | '-') {
                    j += 1;
                }
                out.push((byte(i), byte(j), HlKind::Redirect));
                i = j;
                continue;
            }
        }
        // mot
        let start = i;
        let mut segs: Vec<(usize, usize, HlKind)> = Vec::new();
        let mut j = i;
        while j < n {
            let ch = chars[j].1;
            if matches!(ch, ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')')
                || ((ch == '<' || ch == '>') && !(j + 1 < n && chars[j + 1].1 == '('))
            {
                break;
            }
            match ch {
                '\\' => j += 2,
                '\'' => {
                    let s = j;
                    j += 1;
                    while j < n && chars[j].1 != '\'' {
                        j += 1;
                    }
                    j = (j + 1).min(n);
                    segs.push((s, j, if j <= n && chars.get(j - 1).map(|x| x.1) == Some('\'') && j - s >= 2 { HlKind::String } else { HlKind::Error }));
                }
                '"' => {
                    let s = j;
                    j += 1;
                    let mut closed = false;
                    while j < n {
                        if chars[j].1 == '\\' {
                            j += 2;
                            continue;
                        }
                        if chars[j].1 == '"' {
                            closed = true;
                            j += 1;
                            break;
                        }
                        if chars[j].1 == '$' {
                            let vs = j;
                            j += 1;
                            if j < n && chars[j].1 == '{' {
                                while j < n && chars[j].1 != '}' && chars[j].1 != '"' {
                                    j += 1;
                                }
                                if j < n && chars[j].1 == '}' {
                                    j += 1;
                                }
                            } else {
                                while j < n && (chars[j].1.is_alphanumeric() || chars[j].1 == '_') {
                                    j += 1;
                                }
                                if j == vs + 1 && j < n && "?$#@*!-0123456789".contains(chars[j].1) {
                                    j += 1;
                                }
                            }
                            segs.push((vs, j, HlKind::Variable));
                            continue;
                        }
                        j += 1;
                    }
                    segs.push((s, j.min(n), if closed { HlKind::String } else { HlKind::Error }));
                }
                '$' => {
                    let s = j;
                    j += 1;
                    if j < n && (chars[j].1 == '{' || chars[j].1 == '(') {
                        let (o, cl) = if chars[j].1 == '{' { ('{', '}') } else { ('(', ')') };
                        let mut d = 0;
                        while j < n {
                            if chars[j].1 == o {
                                d += 1;
                            } else if chars[j].1 == cl {
                                d -= 1;
                                if d == 0 {
                                    j += 1;
                                    break;
                                }
                            }
                            j += 1;
                        }
                    } else {
                        while j < n && (chars[j].1.is_alphanumeric() || chars[j].1 == '_') {
                            j += 1;
                        }
                        if j == s + 1 && j < n && "?$#@*!-0123456789".contains(chars[j].1) {
                            j += 1;
                        }
                    }
                    segs.push((s, j.min(n), HlKind::Variable));
                }
                _ => j += 1,
            }
        }
        let j = j.min(n);
        let word: String = chars[start..j].iter().map(|x| x.1).collect();
        let base = if cmd_pos {
            if RESERVED.contains(&word.as_str()) {
                HlKind::Keyword
            } else if word.find('=').is_some_and(|e| is_name(word[..e].trim_end_matches('+'))) {
                HlKind::Assign
            } else {
                HlKind::Command
            }
        } else if word.starts_with('-') {
            HlKind::Option
        } else {
            HlKind::Arg
        };
        out.push((byte(start), byte(j), base));
        for (a, b, k) in segs {
            out.push((byte(a), byte(b), k));
        }
        match base {
            HlKind::Keyword => {
                cmd_pos = !matches!(word.as_str(), "for" | "case" | "in" | "function" | "]]" | "[[" );
                if word == "]]" || word == "fi" || word == "done" || word == "esac" || word == "}" {
                    cmd_pos = false;
                }
            }
            HlKind::Assign => cmd_pos = true,
            _ => cmd_pos = false,
        }
        i = j;
    }
    out
}
