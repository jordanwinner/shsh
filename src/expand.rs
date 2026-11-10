//! Expansion des mots : accolades, tilde, paramètres, substitutions,
//! arithmétique, découpage IFS, globbing et suppression des guillemets.

use crate::ast::*;
use crate::shell::{home_of, Shell};

#[derive(Debug, Clone)]
enum Seg {
    /// Littéral non protégé (globbing actif, pas de découpage)
    Lit(String),
    /// Protégé
    Q(String),
    /// Résultat d'expansion non protégé (découpage + globbing)
    Exp(String),
    /// Séparation forcée de champs ("$@")
    Break,
}

#[derive(Default, Debug)]
struct Field {
    segs: Vec<(String, bool)>,
    has: bool,
}

impl Field {
    fn text(&self) -> String {
        self.segs.iter().map(|s| s.0.as_str()).collect()
    }
}

pub type ExpResult<T> = Result<T, String>;

impl Shell {
    /// Expansion complète d'une liste de mots (arguments de commande).
    pub fn expand_words(&mut self, words: &[Word]) -> ExpResult<Vec<String>> {
        let mut out = Vec::new();
        for w in words {
            for bw in brace_expand(w) {
                self.expand_full(&bw, &mut out)?;
            }
        }
        Ok(out)
    }

    fn expand_full(&mut self, w: &Word, out: &mut Vec<String>) -> ExpResult<()> {
        let mut segs = Vec::new();
        self.expand_parts(w, false, &mut segs)?;
        let fields = self.split_fields(segs);
        for f in fields {
            let has_glob = !self.opts.noglob
                && f
                    .segs
                    .iter()
                    .any(|(s, active)| *active && s.contains(['*', '?', '[']));
            if has_glob {
                let mut pat = String::new();
                for (s, active) in &f.segs {
                    if *active {
                        pat.push_str(s);
                    } else {
                        pat.push_str(&glob::Pattern::escape(s));
                    }
                }
                let mut matches = glob_paths(&pat, self.opts.dotglob, self.opts.nocaseglob);
                if !matches.is_empty() || self.opts.nullglob {
                    matches.sort_by(|a, b| {
                        a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b))
                    });
                    out.extend(matches);
                    continue;
                }
            }
            out.push(f.text());
        }
        Ok(())
    }

    /// Expansion sans découpage ni globbing (affectations, redirections, case…).
    pub fn expand_str(&mut self, w: &Word) -> ExpResult<String> {
        let mut segs = Vec::new();
        self.expand_parts(w, false, &mut segs)?;
        let mut s = String::new();
        let mut first = true;
        for seg in segs {
            match seg {
                Seg::Lit(t) | Seg::Q(t) | Seg::Exp(t) => s.push_str(&t),
                Seg::Break => {
                    if !first {
                        s.push(' ');
                    }
                }
            }
            first = false;
        }
        Ok(s)
    }

    /// Motif glob : le texte protégé est échappé.
    pub fn expand_pattern(&mut self, w: &Word) -> ExpResult<String> {
        let mut segs = Vec::new();
        self.expand_parts(w, false, &mut segs)?;
        let mut s = String::new();
        for seg in segs {
            match seg {
                Seg::Lit(t) | Seg::Exp(t) => {
                    // \c dans un motif : c littéral
                    let mut it = t.chars();
                    while let Some(c) = it.next() {
                        if c == '\\' {
                            match it.next() {
                                Some(n) => s.push_str(&glob::Pattern::escape(&n.to_string())),
                                None => s.push_str("[\\]"),
                            }
                        } else {
                            s.push(c);
                        }
                    }
                }
                Seg::Q(t) => s.push_str(&glob::Pattern::escape(&t)),
                Seg::Break => s.push(' '),
            }
        }
        Ok(s)
    }

    /// Expression régulière : le texte protégé est échappé (comme bash).
    pub fn expand_regex(&mut self, w: &Word) -> ExpResult<String> {
        let mut segs = Vec::new();
        self.expand_parts(w, false, &mut segs)?;
        let mut s = String::new();
        for seg in segs {
            match seg {
                Seg::Lit(t) | Seg::Exp(t) => s.push_str(&t),
                Seg::Q(t) => s.push_str(&regex::escape(&t)),
                Seg::Break => s.push(' '),
            }
        }
        Ok(s)
    }

    fn expand_parts(&mut self, parts: &[Part], quoted: bool, out: &mut Vec<Seg>) -> ExpResult<()> {
        for p in parts {
            match p {
                Part::Lit(s) => out.push(if quoted {
                    Seg::Q(s.clone())
                } else {
                    Seg::Lit(s.clone())
                }),
                Part::Quoted(s) => out.push(Seg::Q(s.clone())),
                Part::DQuoted(inner) => {
                    let is_at = inner.iter().any(|q| matches!(q, Part::Param(pe)
                        if (pe.name == "@" && pe.index.is_none() && !matches!(pe.op, ParamOp::Length))
                            || (matches!(pe.index, Some(Index::All(false))) && !matches!(pe.op, ParamOp::Length))));
                    if !is_at {
                        out.push(Seg::Q(String::new()));
                    }
                    self.expand_parts(inner, true, out)?;
                }
                Part::Tilde(user) => {
                    let h = if user.is_empty() {
                        Some(self.home())
                    } else if user == "+" {
                        Some(self.cwd())
                    } else if user == "-" {
                        self.get_var("OLDPWD")
                    } else {
                        home_of(user)
                    };
                    match h {
                        Some(h) => out.push(Seg::Q(h)),
                        None => out.push(Seg::Lit(format!("~{user}"))),
                    }
                }
                Part::Param(pe) => self.expand_param(pe, quoted, out)?,
                Part::CmdSub(list) => {
                    let s = self.capture(list);
                    let s = s.trim_end_matches('\n').to_string();
                    out.push(if quoted { Seg::Q(s) } else { Seg::Exp(s) });
                }
                Part::Arith(inner) => {
                    let mut segs = Vec::new();
                    self.expand_parts(inner, true, &mut segs)?;
                    let expr: String = segs
                        .into_iter()
                        .map(|s| match s {
                            Seg::Lit(t) | Seg::Q(t) | Seg::Exp(t) => t,
                            Seg::Break => " ".into(),
                        })
                        .collect();
                    let n = crate::arith::eval(&expr, self)?;
                    out.push(if quoted {
                        Seg::Q(n.to_string())
                    } else {
                        Seg::Exp(n.to_string())
                    });
                }
                Part::ProcSub(is_out, list) => {
                    let path = self.process_substitution(list, *is_out)?;
                    out.push(Seg::Q(path));
                }
                Part::ArrayLit(name, append, items) => {
                    let vals = self.expand_words(items)?;
                    self.pending_arrays.insert(name.clone(), vals);
                    out.push(Seg::Q(format!("\x00ARR\x00{name}\x00{}", *append as u8)));
                }
            }
        }
        Ok(())
    }

    fn expand_param(&mut self, pe: &ParamExp, quoted: bool, out: &mut Vec<Seg>) -> ExpResult<()> {
        let name = pe.name.as_str();
        // ${!nom} : indirection ; ${!a[@]} : indices
        if pe.indirect {
            if let Some(Index::All(star)) = &pe.index {
                let keys: Vec<String> = self.get_array_keys(name).iter().map(|i| i.to_string()).collect();
                self.emit_list(keys, *star, quoted, out);
                return Ok(());
            }
            let target = self.lookup(name).unwrap_or_default();
            if target.is_empty() {
                if self.opts.nounset {
                    return Err(format!("{name} : variable non définie"));
                }
                return Ok(());
            }
            let mut np = ParamExp::simple(target.clone(), pe.op.clone());
            if let Some(open) = target.find('[') {
                if target.ends_with(']') {
                    let inner = &target[open + 1..target.len() - 1];
                    np.index = Some(match inner {
                        "@" => Index::All(false),
                        "*" => Index::All(true),
                        _ => Index::Expr(crate::parser::parse_dq_like(inner).map_err(|e| e.msg)?),
                    });
                    np.name = target[..open].to_string();
                }
            }
            if !crate::parser::is_name(&np.name) && np.name.parse::<usize>().is_err() && !"@*#?$!-".contains(np.name.as_str()) {
                return Err(format!("{target} : nom de variable invalide pour ${{!…}}"));
            }
            return self.expand_param(&np, quoted, out);
        }
        match &pe.index {
            Some(Index::All(star)) => return self.expand_list_param(pe, *star, quoted, out),
            Some(Index::Expr(parts)) => {
                let e = self.expand_str(&vec![Part::DQuoted(parts.clone())])?;
                let i = crate::arith::eval(&e, self)?;
                let val = self.get_elem(name, i);
                return self.expand_scalar(pe, val, quoted, out);
            }
            None => {}
        }
        if name == "@" || name == "*" {
            return self.expand_list_param(pe, name == "*", quoted, out);
        }
        let val = self.lookup(name);
        self.expand_scalar(pe, val, quoted, out)
    }

    fn emit_list(&self, vals: Vec<String>, star: bool, quoted: bool, out: &mut Vec<Seg>) {
        if quoted && star {
            let sep = self
                .get_var("IFS")
                .map(|s| s.chars().next().map(|c| c.to_string()).unwrap_or_default())
                .unwrap_or_else(|| " ".into());
            out.push(Seg::Q(vals.join(&sep)));
            return;
        }
        for (i, p) in vals.into_iter().enumerate() {
            if i > 0 {
                out.push(Seg::Break);
            }
            out.push(if quoted { Seg::Q(p) } else { Seg::Exp(p) });
        }
    }

    /// $@, $*, ${a[@]}, ${a[*]} avec leurs opérateurs (appliqués à chaque élément).
    fn expand_list_param(&mut self, pe: &ParamExp, star: bool, quoted: bool, out: &mut Vec<Seg>) -> ExpResult<()> {
        let positional = pe.name == "@" || pe.name == "*";
        let arr = self.get_array(&pe.name);
        let wrap = |s: String| if quoted { Seg::Q(s) } else { Seg::Exp(s) };
        match &pe.op {
            ParamOp::Plain => self.emit_list(arr, star, quoted, out),
            ParamOp::Length => out.push(wrap(arr.len().to_string())),
            ParamOp::Default(colon, w) | ParamOp::Alt(colon, w) => {
                let set = !arr.is_empty() && !(*colon && arr.iter().all(|x| x.is_empty()));
                match (&pe.op, set) {
                    (ParamOp::Default(..), true) => self.emit_list(arr, star, quoted, out),
                    (ParamOp::Default(..), false) | (ParamOp::Alt(..), true) => self.expand_parts(w, quoted, out)?,
                    _ => {}
                }
            }
            ParamOp::Assign(..) | ParamOp::Error(..) => {
                let joined = arr.join(" ");
                let v = if arr.is_empty() { None } else { Some(joined) };
                return self.expand_scalar(&ParamExp::simple(pe.name.clone(), pe.op.clone()), v, quoted, out);
            }
            ParamOp::TrimPrefix(longest, w) | ParamOp::TrimSuffix(longest, w) => {
                let pat = self.expand_pattern(w)?;
                let prefix = matches!(pe.op, ParamOp::TrimPrefix(..));
                let v = arr.iter().map(|x| trim(x, &pat, prefix, *longest)).collect();
                self.emit_list(v, star, quoted, out);
            }
            ParamOp::Replace(all, anchor, pw, rw) => {
                let pat = self.expand_pattern(pw)?;
                let rep = self.expand_str(rw)?;
                let v = arr.iter().map(|x| replace(x, &pat, &rep, *all, *anchor)).collect();
                self.emit_list(v, star, quoted, out);
            }
            ParamOp::Upper(all, pw) | ParamOp::Lower(all, pw) => {
                let up = matches!(pe.op, ParamOp::Upper(..));
                let pat = self.expand_pattern(pw)?;
                let v = arr.iter().map(|x| change_case(x, up, *all, &pat)).collect();
                self.emit_list(v, star, quoted, out);
            }
            ParamOp::Substr(ow, lw) => {
                // ${@:2} commence à $2 ; ${a[@]:1} à l'élément 1
                let list: Vec<String> = if positional {
                    std::iter::once(self.arg0.clone()).chain(arr).collect()
                } else {
                    arr
                };
                let n = list.len() as i64;
                let o = self.expand_str(ow)?;
                let mut off = crate::arith::eval(&o, self)?;
                if off < 0 {
                    off = (n + off).max(0);
                }
                let off = off.min(n);
                let end = match lw {
                    Some(lw) => {
                        let l = self.expand_str(lw)?;
                        let l = crate::arith::eval(&l, self)?;
                        if l < 0 {
                            return Err("longueur négative pour une tranche de tableau".into());
                        }
                        (off + l).min(n)
                    }
                    None => n,
                };
                self.emit_list(list[off as usize..end as usize].to_vec(), star, quoted, out);
            }
        }
        Ok(())
    }

    fn expand_scalar(&mut self, pe: &ParamExp, val: Option<String>, quoted: bool, out: &mut Vec<Seg>) -> ExpResult<()> {
        let name = pe.name.as_str();
        let wrap = |s: String| if quoted { Seg::Q(s) } else { Seg::Exp(s) };
        let is_set = |v: &Option<String>, colon: bool| match v {
            None => false,
            Some(s) => !(colon && s.is_empty()),
        };
        match &pe.op {
            ParamOp::Plain => match val {
                Some(v) => out.push(wrap(v)),
                None => {
                    if self.opts.nounset && name != "@" && name != "*" {
                        return Err(format!("{name} : variable non définie"));
                    }
                }
            },
            ParamOp::Length => {
                out.push(wrap(val.unwrap_or_default().chars().count().to_string()));
            }
            ParamOp::Default(colon, w) => {
                if is_set(&val, *colon) {
                    out.push(wrap(val.unwrap()));
                } else {
                    self.expand_parts(w, quoted, out)?;
                }
            }
            ParamOp::Assign(colon, w) => {
                if is_set(&val, *colon) {
                    out.push(wrap(val.unwrap()));
                } else {
                    let v = self.expand_str(w)?;
                    if !crate::parser::is_name(name) {
                        return Err(format!("${{{name}:=…}} : affectation impossible"));
                    }
                    self.set_var(name, v.clone())?;
                    out.push(wrap(v));
                }
            }
            ParamOp::Error(colon, w) => {
                if is_set(&val, *colon) {
                    out.push(wrap(val.unwrap()));
                } else {
                    let msg = self.expand_str(w)?;
                    let msg = if msg.is_empty() {
                        "paramètre vide ou non défini".to_string()
                    } else {
                        msg
                    };
                    return Err(format!("{name} : {msg}"));
                }
            }
            ParamOp::Alt(colon, w) => {
                if is_set(&val, *colon) {
                    self.expand_parts(w, quoted, out)?;
                }
            }
            ParamOp::TrimPrefix(longest, w) | ParamOp::TrimSuffix(longest, w) => {
                let v = val.unwrap_or_default();
                let pat = self.expand_pattern(w)?;
                let prefix = matches!(pe.op, ParamOp::TrimPrefix(..));
                out.push(wrap(trim(&v, &pat, prefix, *longest)));
            }
            ParamOp::Replace(all, anchor, pw, rw) => {
                let v = val.unwrap_or_default();
                let pat = self.expand_pattern(pw)?;
                let rep = self.expand_str(rw)?;
                out.push(wrap(replace(&v, &pat, &rep, *all, *anchor)));
            }
            ParamOp::Upper(all, pw) | ParamOp::Lower(all, pw) => {
                let v = val.unwrap_or_default();
                let up = matches!(pe.op, ParamOp::Upper(..));
                let pat = self.expand_pattern(pw)?;
                out.push(wrap(change_case(&v, up, *all, &pat)));
            }
            ParamOp::Substr(ow, lw) => {
                let v = val.unwrap_or_default();
                let chars: Vec<char> = v.chars().collect();
                let n = chars.len() as i64;
                let o = self.expand_str(ow)?;
                let mut off = crate::arith::eval(&o, self)?;
                if off < 0 {
                    off = (n + off).max(0);
                }
                let off = off.min(n);
                let end = match lw {
                    Some(lw) => {
                        let l = self.expand_str(lw)?;
                        let l = crate::arith::eval(&l, self)?;
                        if l < 0 {
                            (n + l).max(off)
                        } else {
                            (off + l).min(n)
                        }
                    }
                    None => n,
                };
                let s: String = chars[off as usize..end as usize].iter().collect();
                out.push(wrap(s));
            }
        }
        Ok(())
    }

    fn split_fields(&self, segs: Vec<Seg>) -> Vec<Field> {
        let ifs = self.get_var("IFS");
        let ifs = ifs.as_deref().unwrap_or(" \t\n");
        let mut fields = Vec::new();
        let mut cur = Field::default();
        for seg in segs {
            match seg {
                Seg::Lit(s) => {
                    cur.segs.push((s, true));
                    cur.has = true;
                }
                Seg::Q(s) => {
                    cur.segs.push((s, false));
                    cur.has = true;
                }
                Seg::Break => {
                    if cur.has {
                        fields.push(std::mem::take(&mut cur));
                    }
                }
                Seg::Exp(s) => {
                    if ifs.is_empty() {
                        if !s.is_empty() {
                            cur.segs.push((s, true));
                            cur.has = true;
                        }
                        continue;
                    }
                    let mut buf = String::new();
                    let mut pending_ws = false;
                    for c in s.chars() {
                        if ifs.contains(c) {
                            let is_ws = c == ' ' || c == '\t' || c == '\n';
                            if !buf.is_empty() {
                                cur.segs.push((std::mem::take(&mut buf), true));
                                cur.has = true;
                            }
                            if is_ws {
                                if cur.has {
                                    fields.push(std::mem::take(&mut cur));
                                }
                                pending_ws = true;
                            } else {
                                // délimiteur non blanc : produit un champ (même vide)
                                // (un blanc collé au délimiteur ne crée pas de champ vide)
                                if cur.has || !pending_ws {
                                    cur.has = true;
                                    fields.push(std::mem::take(&mut cur));
                                }
                                pending_ws = false;
                            }
                        } else {
                            buf.push(c);
                            pending_ws = false;
                        }
                    }
                    if !buf.is_empty() {
                        cur.segs.push((buf, true));
                        cur.has = true;
                    }
                }
            }
        }
        if cur.has {
            fields.push(cur);
        }
        fields
    }

    /// Correspondance d'un motif shell (case, [[ == ]]).
    pub fn pattern_match(&self, pat: &str, s: &str) -> bool {
        if self.opts.nocasematch {
            return match glob::Pattern::new(pat) {
                Ok(p) => p.matches_with(
                    s,
                    glob::MatchOptions {
                        case_sensitive: false,
                        require_literal_separator: false,
                        require_literal_leading_dot: false,
                    },
                ),
                Err(_) => pat.eq_ignore_ascii_case(s),
            };
        }
        glob_match(pat, s)
    }
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Globbing de chemins : composant par composant, `**` récursif,
/// fichiers cachés seulement si le motif commence par un point, jamais . ni ..
pub fn glob_paths(pat: &str, dotglob: bool, nocase: bool) -> Vec<String> {
    let absolute = pat.starts_with('/');
    let trailing_slash = pat.ends_with('/');
    let comps: Vec<&str> = pat.split('/').filter(|c| !c.is_empty()).collect();
    let mut results: Vec<String> = Vec::new();
    let base = if absolute { "/".to_string() } else { String::new() };
    let g = GlobOpts { dotglob, nocase };
    glob_walk(&base, &comps, trailing_slash, &mut results, 0, &g);
    results
}

struct GlobOpts {
    dotglob: bool,
    nocase: bool,
}

fn join(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn glob_walk(base: &str, comps: &[&str], dir_only: bool, out: &mut Vec<String>, depth: usize, g: &GlobOpts) {
    if depth > 64 || out.len() > 200_000 {
        return;
    }
    let Some((first, rest)) = comps.split_first() else {
        if !base.is_empty() {
            if dir_only {
                if std::path::Path::new(base).is_dir() {
                    out.push(format!("{}/", base.trim_end_matches('/')));
                }
            } else {
                out.push(base.to_string());
            }
        }
        return;
    };
    let dir = if base.is_empty() { "." } else { base };
    if *first == "**" {
        // zéro dossier…
        glob_walk(base, rest, dir_only, out, depth + 1, g);
        // …ou un de plus, récursivement (sans suivre les cachés)
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut names: Vec<String> = rd
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.'))
                .collect();
            names.sort();
            for n in names {
                glob_walk(&join(base, &n), comps, dir_only, out, depth + 1, g);
            }
        }
        return;
    }
    if !has_glob(first) {
        let p = join(base, first);
        if rest.is_empty() {
            if std::fs::symlink_metadata(&p).is_ok() {
                glob_walk(&p, rest, dir_only, out, depth + 1, g);
            }
        } else if std::path::Path::new(&p).is_dir() || *first == "." || *first == ".." {
            glob_walk(&p, rest, dir_only, out, depth + 1, g);
        }
        return;
    }
    let pattern = match glob::Pattern::new(first) {
        Ok(p) => p,
        Err(_) => return,
    };
    let opts = glob::MatchOptions {
        case_sensitive: !g.nocase,
        require_literal_separator: true,
        require_literal_leading_dot: !g.dotglob,
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| pattern.matches_with(n, opts))
        .collect();
    names.sort();
    for n in names {
        let p = join(base, &n);
        if rest.is_empty() || std::path::Path::new(&p).is_dir() {
            glob_walk(&p, rest, dir_only, out, depth + 1, g);
        }
    }
}

pub fn glob_match(pat: &str, s: &str) -> bool {
    match glob::Pattern::new(pat) {
        Ok(p) => p.matches_with(
            s,
            glob::MatchOptions {
                case_sensitive: true,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            },
        ),
        Err(_) => pat == s,
    }
}

fn change_case(v: &str, up: bool, all: bool, pat: &str) -> String {
    let conv = |c: char| -> String {
        if !pat.is_empty() && !glob_match(pat, &c.to_string()) {
            return c.to_string();
        }
        if up {
            c.to_uppercase().collect()
        } else {
            c.to_lowercase().collect()
        }
    };
    let mut out = String::new();
    for (i, c) in v.chars().enumerate() {
        if all || i == 0 {
            out.push_str(&conv(c));
        } else {
            out.push(c);
        }
    }
    out
}

fn char_bounds(s: &str) -> Vec<usize> {
    let mut v: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    v.push(s.len());
    v
}

fn trim(v: &str, pat: &str, prefix: bool, longest: bool) -> String {
    let b = char_bounds(v);
    if prefix {
        let iter: Box<dyn Iterator<Item = &usize>> = if longest {
            Box::new(b.iter().rev())
        } else {
            Box::new(b.iter())
        };
        for &i in iter {
            if glob_match(pat, &v[..i]) {
                return v[i..].to_string();
            }
        }
    } else {
        let iter: Box<dyn Iterator<Item = &usize>> = if longest {
            Box::new(b.iter())
        } else {
            Box::new(b.iter().rev())
        };
        for &i in iter {
            if glob_match(pat, &v[i..]) {
                return v[..i].to_string();
            }
        }
    }
    v.to_string()
}

fn replace(v: &str, pat: &str, rep: &str, all: bool, anchor: Option<char>) -> String {
    if pat.is_empty() {
        return v.to_string();
    }
    let b = char_bounds(v);
    match anchor {
        Some('#') => {
            for &j in b.iter().rev() {
                if glob_match(pat, &v[..j]) {
                    return format!("{rep}{}", &v[j..]);
                }
            }
            return v.to_string();
        }
        Some('%') => {
            for &i in b.iter() {
                if glob_match(pat, &v[i..]) {
                    return format!("{}{rep}", &v[..i]);
                }
            }
            return v.to_string();
        }
        _ => {}
    }
    let mut out = String::new();
    let mut k = 0; // indice dans b
    let mut done = false;
    while k < b.len() - 1 {
        let i = b[k];
        if !done {
            // plus longue correspondance à partir de i
            let mut found = None;
            for m in (k + 1..b.len()).rev() {
                if glob_match(pat, &v[i..b[m]]) {
                    found = Some(m);
                    break;
                }
            }
            if let Some(m) = found {
                out.push_str(rep);
                k = m;
                if !all {
                    done = true;
                }
                continue;
            }
        }
        out.push_str(&v[i..b[k + 1]]);
        k += 1;
    }
    out
}

// --- Expansion des accolades -------------------------------------------------

pub fn brace_expand(w: &Word) -> Vec<Word> {
    for (pi, part) in w.iter().enumerate() {
        if let Part::Lit(s) = part {
            if let Some((open, close, alts)) = find_brace(s) {
                let mut out = Vec::new();
                for alt in alts {
                    let mut nw: Word = w[..pi].to_vec();
                    let mut mid = String::new();
                    mid.push_str(&s[..open]);
                    mid.push_str(&alt);
                    nw.push(Part::Lit(mid));
                    if close + 1 < s.len() {
                        nw.push(Part::Lit(s[close + 1..].to_string()));
                    }
                    nw.extend_from_slice(&w[pi + 1..]);
                    out.extend(brace_expand(&nw));
                }
                return out;
            }
        }
    }
    vec![w.clone()]
}

fn find_brace(s: &str) -> Option<(usize, usize, Vec<String>)> {
    let bytes: Vec<(usize, char)> = s.char_indices().collect();
    for (k, &(i, c)) in bytes.iter().enumerate() {
        if c != '{' {
            continue;
        }
        let mut depth = 0;
        let mut commas = Vec::new();
        for &(j, d) in &bytes[k..] {
            match d {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &s[i + 1..j];
                        if !commas.is_empty() {
                            let mut alts = Vec::new();
                            let mut last = i + 1;
                            for &cpos in &commas {
                                alts.push(s[last..cpos].to_string());
                                last = cpos + 1;
                            }
                            alts.push(s[last..j].to_string());
                            return Some((i, j, alts));
                        }
                        if let Some(seq) = brace_seq(inner) {
                            return Some((i, j, seq));
                        }
                        break;
                    }
                }
                ',' if depth == 1 => commas.push(j),
                _ => {}
            }
        }
    }
    None
}

fn brace_seq(inner: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = inner.split("..").collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let step: i64 = if parts.len() == 3 {
        parts[2].parse().ok()?
    } else {
        1
    };
    let step = step.abs().max(1);
    if let (Ok(a), Ok(b)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
        let width = if (parts[0].starts_with('0') && parts[0].len() > 1)
            || (parts[1].starts_with('0') && parts[1].len() > 1)
            || parts[0].starts_with("-0")
        {
            parts[0].len().max(parts[1].len())
        } else {
            0
        };
        let mut v = Vec::new();
        let mut x = a;
        let count = ((a - b).abs() / step + 1) as usize;
        if count > 100_000 {
            return None;
        }
        for _ in 0..count {
            v.push(if width > 0 {
                format!("{x:0width$}")
            } else {
                x.to_string()
            });
            x += if a <= b { step } else { -step };
        }
        return Some(v);
    }
    let ca: Vec<char> = parts[0].chars().collect();
    let cb: Vec<char> = parts[1].chars().collect();
    if ca.len() == 1 && cb.len() == 1 && ca[0].is_ascii_alphabetic() && cb[0].is_ascii_alphabetic() {
        let (a, b) = (ca[0] as i64, cb[0] as i64);
        let mut v = Vec::new();
        let mut x = a;
        loop {
            v.push((x as u8 as char).to_string());
            if x == b {
                break;
            }
            x += if a <= b { step } else { -step };
            if (a <= b && x > b) || (a > b && x < b) {
                break;
            }
        }
        return Some(v);
    }
    None
}
