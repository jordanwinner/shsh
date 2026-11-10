//! Commandes internes.

use crate::exec::{flush_stdout, quote_if_needed, write_fd};
use crate::jobs::ProcState;
use crate::parser::is_name;
use crate::shell::*;
use nix::sys::signal::{self, Signal};
use nix::unistd::{self, Pid};
use std::rc::Rc;

pub type Builtin = fn(&mut Shell, &[String]) -> ExecResult;

pub const NAMES: &[(&str, &str)] = &[
    (":", "ne fait rien, réussit"),
    (".", "exécute un fichier dans le shell courant"),
    ("[", "évalue une condition"),
    ("abbr", "abréviations développées à la frappe"),
    ("alias", "définit ou affiche des alias"),
    ("bg", "relance une tâche en arrière-plan"),
    ("break", "sort d'une boucle"),
    ("builtin", "exécute une commande interne"),
    ("cd", "change de dossier"),
    ("command", "exécute une commande en ignorant les fonctions"),
    ("continue", "passe à l'itération suivante"),
    ("declare", "déclare des variables"),
    ("dirs", "affiche la pile de dossiers"),
    ("disown", "détache une tâche du shell"),
    ("echo", "affiche ses arguments"),
    ("eval", "évalue ses arguments comme une commande"),
    ("exec", "remplace le shell par une commande"),
    ("exit", "quitte le shell"),
    ("export", "exporte des variables vers l'environnement"),
    ("false", "échoue"),
    ("fg", "ramène une tâche au premier plan"),
    ("getopts", "analyse les options d'un script"),
    ("hash", "cache des chemins de commandes"),
    ("help", "aide de shsh"),
    ("history", "historique des commandes"),
    ("jobs", "liste les tâches"),
    ("kill", "envoie un signal"),
    ("let", "évalue des expressions arithmétiques"),
    ("local", "variables locales à une fonction"),
    ("mapfile", "lit des lignes dans un tableau (alias readarray)"),
    ("popd", "dépile un dossier"),
    ("printf", "affichage formaté"),
    ("pushd", "empile un dossier"),
    ("pwd", "affiche le dossier courant"),
    ("read", "lit une ligne"),
    ("readonly", "variables en lecture seule"),
    ("return", "sort d'une fonction"),
    ("set", "options du shell et paramètres"),
    ("shift", "décale les paramètres positionnels"),
    ("shopt", "options étendues (nullglob, dotglob, nocasematch…)"),
    ("source", "exécute un fichier dans le shell courant"),
    ("test", "évalue une condition"),
    ("times", "temps processeur consommé"),
    ("trap", "pièges sur signaux"),
    ("true", "réussit"),
    ("type", "indique la nature d'une commande"),
    ("typeset", "déclare des variables"),
    ("ulimit", "limites de ressources"),
    ("umask", "masque de création de fichiers"),
    ("unalias", "supprime des alias"),
    ("unset", "supprime variables ou fonctions"),
    ("wait", "attend la fin de tâches"),
];

pub fn lookup(name: &str) -> Option<Builtin> {
    Some(match name {
        ":" | "true" => |_, _| Ok(0),
        "false" => |_, _| Ok(1),
        "." | "source" => b_source,
        "[" | "test" => b_test,
        "abbr" => b_abbr,
        "alias" => b_alias,
        "bg" => b_bg,
        "break" => b_break,
        "continue" => b_continue,
        "cd" => b_cd,
        "command" => b_command,
        "declare" | "typeset" => b_declare,
        "dirs" => b_dirs,
        "disown" => b_disown,
        "echo" => b_echo,
        "eval" => b_eval,
        "exit" => b_exit,
        "export" => b_export,
        "fg" => b_fg,
        "getopts" => b_getopts,
        "hash" => b_hash,
        "help" => b_help,
        "history" => b_history,
        "jobs" => b_jobs,
        "kill" => b_kill,
        "let" => b_let,
        "local" => b_local,
        "popd" => b_popd,
        "printf" => b_printf,
        "pushd" => b_pushd,
        "pwd" => b_pwd,
        "read" => b_read,
        "readonly" => b_readonly,
        "return" => b_return,
        "set" => b_set,
        "mapfile" | "readarray" => b_mapfile,
        "shopt" => b_shopt,
        "shift" => b_shift,
        "times" => b_times,
        "trap" => b_trap,
        "type" => b_type,
        "ulimit" => b_ulimit,
        "umask" => b_umask,
        "unalias" => b_unalias,
        "unset" => b_unset,
        "wait" => b_wait,
        "builtin" => |sh, a| {
            if a.len() < 2 {
                return Ok(0);
            }
            match lookup(&a[1]) {
                Some(b) => b(sh, &a[1..]),
                None => {
                    sh.error(format!("builtin : {} n'est pas une commande interne", a[1]));
                    Ok(1)
                }
            }
        },
        _ => return None,
    })
}

pub fn is_special(name: &str) -> bool {
    matches!(
        name,
        ":" | "." | "source" | "break" | "continue" | "eval" | "exec" | "exit" | "export"
            | "readonly" | "return" | "set" | "shift" | "trap" | "unset"
    )
}

pub fn run(sh: &mut Shell, argv: &[String]) -> ExecResult {
    match lookup(&argv[0]) {
        Some(b) => b(sh, argv),
        None => Ok(127),
    }
}

/// Argument « nom=(…) » transmis par le parseur (marqueur + valeurs en attente).
fn array_arg(sh: &mut Shell, arg: &str) -> Option<(String, bool, Vec<String>)> {
    let rest = arg.strip_prefix("\x00ARR\x00")?;
    let (name, app) = rest.split_once('\x00')?;
    let vals = sh.pending_arrays.remove(name).unwrap_or_default();
    Some((name.to_string(), app == "1", vals))
}

fn assign_array_arg(sh: &mut Shell, name: &str, append: bool, mut vals: Vec<String>) -> Result<(), String> {
    if append {
        return sh.append_array(name, std::mem::take(&mut vals));
    }
    sh.set_array(name, vals)
}

fn out(s: &str) {
    flush_stdout();
    write_fd(1, s);
}

// --------------------------------------------------------------------------
// Dossiers
// --------------------------------------------------------------------------

/// Normalise un chemin logique (résout . et .. sans suivre les liens).
fn normalize(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            c => stack.push(c),
        }
    }
    format!("/{}", stack.join("/"))
}

pub fn change_dir(sh: &mut Shell, target: &str, print: bool) -> Result<(), String> {
    let cur = sh.cwd();
    let logical = if target.starts_with('/') {
        normalize(target)
    } else {
        normalize(&format!("{cur}/{target}"))
    };
    let ok = std::env::set_current_dir(&logical).is_ok() || {
        // Repli physique (lien symbolique puis ..)
        std::env::set_current_dir(target).is_ok()
    };
    if !ok {
        let e = std::fs::metadata(target);
        let why = match e {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "fichier ou dossier introuvable",
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => "permission refusée",
            Ok(m) if !m.is_dir() => "n'est pas un dossier",
            _ => "accès impossible",
        };
        return Err(format!("cd : {target} : {why}"));
    }
    let newpwd = if std::path::Path::new(&logical).is_dir() && same_file(&logical, ".") {
        logical
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(logical)
    };
    sh.set_exported("OLDPWD", cur);
    sh.set_exported("PWD", newpwd.clone());
    if print {
        out(&format!("{}\n", tilde_path(sh, &newpwd)));
    }
    // Crochet chpwd
    if let Some(f) = sh.functions.get("chpwd").cloned() {
        let _ = sh.call_function(&f, &["chpwd".into()]);
    }
    Ok(())
}

pub fn tilde_path(sh: &Shell, p: &str) -> String {
    let home = sh.home();
    if home.len() > 1 && (p == home || p.starts_with(&format!("{home}/"))) {
        format!("~{}", &p[home.len()..])
    } else {
        p.to_string()
    }
}

fn b_cd(sh: &mut Shell, a: &[String]) -> ExecResult {
    let args: Vec<&String> = a[1..].iter().filter(|x| *x != "-L" && *x != "-P").collect();
    let (target, print) = match args.first().map(|s| s.as_str()) {
        None => (sh.home(), false),
        Some("-") => match sh.get_var("OLDPWD") {
            Some(o) => (o, true),
            None => {
                sh.error("cd : OLDPWD non défini");
                return Ok(1);
            }
        },
        Some(t) => {
            // CDPATH
            let mut t = t.to_string();
            let mut print = false;
            if !t.starts_with('/') && !t.starts_with('.') {
                if let Some(cdpath) = sh.get_var("CDPATH") {
                    for d in cdpath.split(':').filter(|d| !d.is_empty()) {
                        let cand = format!("{d}/{t}");
                        if std::path::Path::new(&cand).is_dir() {
                            t = cand;
                            print = true;
                            break;
                        }
                    }
                }
            }
            (t, print)
        }
    };
    match change_dir(sh, &target, print) {
        Ok(()) => Ok(0),
        Err(e) => {
            sh.error(e);
            Ok(1)
        }
    }
}

fn b_pwd(sh: &mut Shell, a: &[String]) -> ExecResult {
    let p = if a.iter().any(|x| x == "-P") {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        sh.cwd()
    };
    out(&format!("{p}\n"));
    Ok(0)
}

fn b_pushd(sh: &mut Shell, a: &[String]) -> ExecResult {
    let cur = sh.cwd();
    let target = match a.get(1) {
        Some(t) => t.clone(),
        None => match sh.dir_stack.pop() {
            Some(t) => t,
            None => {
                sh.error("pushd : pas d'autre dossier");
                return Ok(1);
            }
        },
    };
    if let Err(e) = change_dir(sh, &target, false) {
        sh.error(e.replacen("cd", "pushd", 1));
        return Ok(1);
    }
    sh.dir_stack.push(cur);
    b_dirs(sh, &[])
}

fn b_popd(sh: &mut Shell, _a: &[String]) -> ExecResult {
    match sh.dir_stack.pop() {
        Some(d) => {
            if let Err(e) = change_dir(sh, &d, false) {
                sh.error(e.replacen("cd", "popd", 1));
                return Ok(1);
            }
            b_dirs(sh, &[])
        }
        None => {
            sh.error("popd : pile de dossiers vide");
            Ok(1)
        }
    }
}

fn b_dirs(sh: &mut Shell, _a: &[String]) -> ExecResult {
    let mut v = vec![tilde_path(sh, &sh.cwd())];
    for d in sh.dir_stack.iter().rev() {
        v.push(tilde_path(sh, d));
    }
    out(&format!("{}\n", v.join(" ")));
    Ok(0)
}

// --------------------------------------------------------------------------
// Affichage
// --------------------------------------------------------------------------

fn echo_escapes(s: &str) -> (String, bool) {
    let mut o = String::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\\' && i + 1 < c.len() {
            i += 1;
            match c[i] {
                'n' => o.push('\n'),
                't' => o.push('\t'),
                'r' => o.push('\r'),
                'a' => o.push('\x07'),
                'b' => o.push('\x08'),
                'e' | 'E' => o.push('\x1b'),
                'f' => o.push('\x0c'),
                'v' => o.push('\x0b'),
                '\\' => o.push('\\'),
                'c' => return (o, true),
                '0' => {
                    let mut v = 0u32;
                    let mut n = 0;
                    while n < 3 && i + 1 < c.len() && c[i + 1].is_digit(8) {
                        i += 1;
                        v = v * 8 + c[i].to_digit(8).unwrap();
                        n += 1;
                    }
                    o.push(char::from_u32(v).unwrap_or('?'));
                }
                'x' => {
                    let mut v = 0u32;
                    let mut n = 0;
                    while n < 2 && i + 1 < c.len() && c[i + 1].is_ascii_hexdigit() {
                        i += 1;
                        v = v * 16 + c[i].to_digit(16).unwrap();
                        n += 1;
                    }
                    if n == 0 {
                        o.push_str("\\x");
                    } else {
                        o.push(char::from_u32(v).unwrap_or('?'));
                    }
                }
                'u' | 'U' => {
                    let max = if c[i] == 'u' { 4 } else { 8 };
                    let mut v = 0u32;
                    let mut n = 0;
                    while n < max && i + 1 < c.len() && c[i + 1].is_ascii_hexdigit() {
                        i += 1;
                        v = v * 16 + c[i].to_digit(16).unwrap();
                        n += 1;
                    }
                    o.push(char::from_u32(v).unwrap_or('?'));
                }
                other => {
                    o.push('\\');
                    o.push(other);
                }
            }
        } else {
            o.push(c[i]);
        }
        i += 1;
    }
    (o, false)
}

fn b_echo(_sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut newline = true;
    let mut esc = false;
    let mut i = 1;
    while i < a.len() {
        let s = &a[i];
        if s.len() > 1 && s.starts_with('-') && s[1..].chars().all(|c| "neE".contains(c)) {
            for c in s[1..].chars() {
                match c {
                    'n' => newline = false,
                    'e' => esc = true,
                    'E' => esc = false,
                    _ => {}
                }
            }
            i += 1;
        } else {
            break;
        }
    }
    let mut s = a[i..].join(" ");
    if esc {
        let (t, stop) = echo_escapes(&s);
        s = t;
        if stop {
            newline = false;
        }
    }
    if newline {
        s.push('\n');
    }
    if !write_fd(1, &s) {
        return Ok(1);
    }
    Ok(0)
}

fn parse_num_arg(s: &str) -> i64 {
    let t = s.trim();
    if let Some(c) = t.strip_prefix('\'').or_else(|| t.strip_prefix('"')) {
        return c.chars().next().map(|c| c as i64).unwrap_or(0);
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).unwrap_or(0)
    } else if body.len() > 1 && body.starts_with('0') {
        i64::from_str_radix(&body[1..], 8).unwrap_or(0)
    } else {
        body.parse().unwrap_or(0)
    };
    if neg {
        -v
    } else {
        v
    }
}

pub fn printf_format(fmt: &str, args: &[String]) -> (String, bool) {
    let mut o = String::new();
    let f: Vec<char> = fmt.chars().collect();
    let mut ai = 0;
    let mut ok = true;
    loop {
        let start_ai = ai;
        let mut i = 0;
        while i < f.len() {
            let c = f[i];
            if c == '\\' {
                let rest: String = f[i..].iter().collect();
                // séquence d'échappement : on prend le plus court préfixe
                let mut j = i + 1;
                if j < f.len() {
                    let lim = match f[j] {
                        '0' => 4,
                        'x' => 3,
                        'u' => 5,
                        'U' => 9,
                        _ => 1,
                    };
                    let mut k = 1;
                    while k < lim && j + k < f.len() {
                        let ch = f[j + k];
                        let okc = match f[j] {
                            '0' => ch.is_digit(8),
                            _ => ch.is_ascii_hexdigit(),
                        };
                        if !okc {
                            break;
                        }
                        k += 1;
                    }
                    j += k;
                }
                let seq: String = f[i..j].iter().collect();
                let _ = rest;
                o.push_str(&echo_escapes(&seq).0);
                i = j;
                continue;
            }
            if c != '%' {
                o.push(c);
                i += 1;
                continue;
            }
            i += 1;
            if i < f.len() && f[i] == '%' {
                o.push('%');
                i += 1;
                continue;
            }
            let mut flags = String::new();
            while i < f.len() && "-+ 0#".contains(f[i]) {
                flags.push(f[i]);
                i += 1;
            }
            let mut width: Option<usize> = None;
            if i < f.len() && f[i] == '*' {
                let w = parse_num_arg(args.get(ai).map(|s| s.as_str()).unwrap_or("0"));
                ai += 1;
                if w < 0 {
                    flags.push('-');
                }
                width = Some(w.unsigned_abs() as usize);
                i += 1;
            } else {
                let mut w = String::new();
                while i < f.len() && f[i].is_ascii_digit() {
                    w.push(f[i]);
                    i += 1;
                }
                if !w.is_empty() {
                    width = w.parse().ok();
                }
            }
            let mut prec: Option<usize> = None;
            if i < f.len() && f[i] == '.' {
                i += 1;
                if i < f.len() && f[i] == '*' {
                    prec = Some(parse_num_arg(args.get(ai).map(|s| s.as_str()).unwrap_or("0")).max(0) as usize);
                    ai += 1;
                    i += 1;
                } else {
                    let mut p = String::new();
                    while i < f.len() && f[i].is_ascii_digit() {
                        p.push(f[i]);
                        i += 1;
                    }
                    prec = Some(p.parse().unwrap_or(0));
                }
            }
            // modificateurs de longueur ignorés
            while i < f.len() && "hlLqjzt".contains(f[i]) {
                i += 1;
            }
            if i >= f.len() {
                o.push('%');
                break;
            }
            let conv = f[i];
            i += 1;
            let arg = args.get(ai).cloned();
            ai += 1;
            let left = flags.contains('-');
            let zero = flags.contains('0') && !left;
            let pad = |s: String, numeric: bool| -> String {
                let w = width.unwrap_or(0);
                let len = s.chars().count();
                if len >= w {
                    return s;
                }
                let fill = w - len;
                if left {
                    format!("{s}{}", " ".repeat(fill))
                } else if zero && numeric {
                    let (sign, digits) = if s.starts_with('-') || s.starts_with('+') {
                        s.split_at(1)
                    } else {
                        ("", s.as_str())
                    };
                    format!("{sign}{}{digits}", "0".repeat(fill))
                } else {
                    format!("{}{s}", " ".repeat(fill))
                }
            };
            let s = match conv {
                's' => {
                    let mut s = arg.unwrap_or_default();
                    if let Some(p) = prec {
                        s = s.chars().take(p).collect();
                    }
                    pad(s, false)
                }
                'b' => {
                    let (mut s, _) = echo_escapes(&arg.unwrap_or_default());
                    if let Some(p) = prec {
                        s = s.chars().take(p).collect();
                    }
                    pad(s, false)
                }
                'q' => pad(quote_if_needed(&arg.unwrap_or_default()), false),
                'c' => pad(
                    arg.unwrap_or_default().chars().next().map(|c| c.to_string()).unwrap_or_default(),
                    false,
                ),
                'd' | 'i' => {
                    let a = arg.unwrap_or_else(|| "0".into());
                    let n = parse_num_arg(&a);
                    if !a.trim().is_empty() && n == 0 && !a.trim().trim_start_matches(['-', '+']).starts_with('0') && !a.starts_with('\'') {
                        ok = false;
                    }
                    let mut s = n.to_string();
                    if let Some(p) = prec {
                        let neg = n < 0;
                        let d = n.unsigned_abs().to_string();
                        s = format!("{}{:0>p$}", if neg { "-" } else { "" }, d, p = p);
                    }
                    if n >= 0 && flags.contains('+') {
                        s = format!("+{s}");
                    } else if n >= 0 && flags.contains(' ') {
                        s = format!(" {s}");
                    }
                    pad(s, true)
                }
                'u' => pad((parse_num_arg(&arg.unwrap_or_else(|| "0".into())) as u64).to_string(), true),
                'o' => {
                    let n = parse_num_arg(&arg.unwrap_or_else(|| "0".into()));
                    let mut s = format!("{:o}", n);
                    if flags.contains('#') {
                        s = format!("0{s}");
                    }
                    pad(s, true)
                }
                'x' | 'X' => {
                    let n = parse_num_arg(&arg.unwrap_or_else(|| "0".into()));
                    let mut s = if conv == 'x' { format!("{:x}", n) } else { format!("{:X}", n) };
                    if let Some(p) = prec {
                        s = format!("{:0>p$}", s, p = p);
                    }
                    if flags.contains('#') && n != 0 {
                        s = format!("{}{s}", if conv == 'x' { "0x" } else { "0X" });
                    }
                    pad(s, true)
                }
                'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                    let v: f64 = arg
                        .as_deref()
                        .map(|s| s.trim().parse().unwrap_or(0.0))
                        .unwrap_or(0.0);
                    let p = prec.unwrap_or(6);
                    let mut s = match conv {
                        'f' | 'F' => format!("{v:.p$}"),
                        'e' => format!("{v:.p$e}"),
                        'E' => format!("{v:.p$E}"),
                        _ => {
                            let s = format!("{v:.p$}", p = p.max(1).saturating_sub(1).min(16));
                            if s.contains('.') {
                                s.trim_end_matches('0').trim_end_matches('.').to_string()
                            } else {
                                s
                            }
                        }
                    };
                    if v >= 0.0 && flags.contains('+') {
                        s = format!("+{s}");
                    }
                    pad(s, true)
                }
                other => {
                    ok = false;
                    format!("%{other}")
                }
            };
            o.push_str(&s);
        }
        if ai >= args.len() || ai == start_ai {
            break;
        }
    }
    (o, ok)
}

fn b_printf(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut args = &a[1..];
    let mut var: Option<String> = None;
    if args.first().map(|s| s.as_str()) == Some("-v") {
        var = args.get(1).cloned();
        args = &args[args.len().min(2)..];
    }
    if args.first().map(|s| s.as_str()) == Some("--") {
        args = &args[1..];
    }
    let fmt = match args.first() {
        Some(f) => f,
        None => {
            sh.error("printf : usage : printf [-v var] format [arguments]");
            return Ok(2);
        }
    };
    let (s, ok) = printf_format(fmt, &args[1..]);
    match var {
        Some(v) => {
            if let Err(e) = sh.set_var(&v, s) {
                sh.error(e);
                return Ok(1);
            }
        }
        None => {
            write_fd(1, &s);
        }
    }
    Ok(if ok { 0 } else { 1 })
}

// --------------------------------------------------------------------------
// Variables
// --------------------------------------------------------------------------

fn b_export(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut unexport = false;
    let mut st = 0;
    let mut names = Vec::new();
    for x in &a[1..] {
        if let Some((name, app, vals)) = array_arg(sh, x) {
            let _ = assign_array_arg(sh, &name, app, vals);
            names.push(name);
            continue;
        }
        match x.as_str() {
            "-n" => unexport = true,
            "-p" | "-f" => {}
            _ => names.push(x.clone()),
        }
    }
    if names.is_empty() {
        let mut v: Vec<(&String, &Var)> = sh.vars.iter().filter(|(_, v)| v.exported).collect();
        v.sort_by_key(|(k, _)| k.as_str());
        let mut s = String::new();
        for (k, var) in v {
            s.push_str(&format!("export {k}={}\n", quote_if_needed(&var.value)));
        }
        out(&s);
        return Ok(0);
    }
    for n in names {
        let (name, val) = match n.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (n.clone(), None),
        };
        if !is_name(&name) {
            sh.error(format!("export : « {n} » : identifiant invalide"));
            st = 1;
            continue;
        }
        if let Some(v) = val {
            if let Err(e) = sh.set_var(&name, v) {
                sh.error(e);
                st = 1;
                continue;
            }
        } else if !sh.vars.contains_key(&name) {
            if unexport {
                continue;
            }
            let _ = sh.set_var(&name, String::new());
            if let Some(var) = sh.vars.get_mut(&name) {
                var.exported = true;
                var.value.clear();
            }
            continue;
        }
        if let Some(var) = sh.vars.get_mut(&name) {
            var.exported = !unexport;
        }
    }
    Ok(st)
}

fn b_readonly(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() == 1 || a[1] == "-p" {
        let mut v: Vec<(&String, &Var)> = sh.vars.iter().filter(|(_, v)| v.readonly).collect();
        v.sort_by_key(|(k, _)| k.as_str());
        let mut s = String::new();
        for (k, var) in v {
            s.push_str(&format!("readonly {k}={}\n", quote_if_needed(&var.value)));
        }
        out(&s);
        return Ok(0);
    }
    for n in &a[1..] {
        let (name, val) = match n.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (n.clone(), None),
        };
        if let Some(v) = val {
            if let Err(e) = sh.set_var(&name, v) {
                sh.error(e);
                return Ok(1);
            }
        } else if !sh.vars.contains_key(&name) {
            let _ = sh.set_var(&name, String::new());
        }
        if let Some(var) = sh.vars.get_mut(&name) {
            var.readonly = true;
        }
    }
    Ok(0)
}

fn b_local(sh: &mut Shell, a: &[String]) -> ExecResult {
    if sh.func_depth == 0 {
        sh.error("local : utilisable seulement dans une fonction");
        return Ok(1);
    }
    let mut st = 0;
    let is_arr = a[1..].iter().any(|x| x.starts_with('-') && x.contains('a'));
    for n in &a[1..] {
        if let Some((name, app, vals)) = array_arg(sh, n) {
            sh.make_local(&name);
            if let Err(e) = assign_array_arg(sh, &name, app, vals) {
                sh.error(e);
                st = 1;
            }
            continue;
        }
        if n.starts_with('-') {
            continue;
        }
        if is_arr && !n.contains('=') {
            sh.make_local(n);
            let _ = sh.set_array(n, vec![]);
            continue;
        }
        let (name, val) = match n.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (n.clone(), None),
        };
        if !is_name(&name) {
            sh.error(format!("local : « {n} » : identifiant invalide"));
            st = 1;
            continue;
        }
        sh.make_local(&name);
        let v = val.unwrap_or_default();
        if let Err(e) = sh.set_var(&name, v) {
            sh.error(e);
            st = 1;
        }
    }
    Ok(st)
}

fn b_declare(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut export = false;
    let mut ro = false;
    let mut print = false;
    let mut funcs = false;
    let mut is_arr = false;
    let mut names = Vec::new();
    for x in &a[1..] {
        if let Some((name, app, vals)) = array_arg(sh, x) {
            if sh.func_depth > 0 {
                sh.make_local(&name);
            }
            if let Err(e) = assign_array_arg(sh, &name, app, vals) {
                sh.error(e);
                return Ok(1);
            }
            names.push(name);
            continue;
        }
        if x.starts_with('-') && x.len() > 1 {
            for c in x[1..].chars() {
                match c {
                    'a' => is_arr = true,
                    'x' => export = true,
                    'r' => ro = true,
                    'p' => print = true,
                    'f' | 'F' => funcs = true,
                    _ => {}
                }
            }
        } else {
            names.push(x.clone());
        }
    }
    if funcs {
        let mut v: Vec<&String> = sh.functions.keys().collect();
        v.sort();
        out(&v.iter().map(|f| format!("declare -f {f}\n")).collect::<String>());
        return Ok(0);
    }
    if names.is_empty() && !print && is_arr {
        return Ok(0);
    }
    if (names.is_empty() || print) && !(is_arr && !print) {
        let mut v: Vec<(&String, &Var)> = sh
            .vars
            .iter()
            .filter(|(k, _)| names.is_empty() || names.contains(k))
            .collect();
        v.sort_by_key(|(k, _)| k.as_str());
        let mut s = String::new();
        for (k, var) in v {
            let mut fl = String::from("-");
            if var.exported {
                fl.push('x');
            }
            if var.readonly {
                fl.push('r');
            }
            if var.array.is_some() {
                fl.push('a');
            }
            if fl == "-" {
                fl.push('-');
            }
            match &var.array {
                Some(arr) => {
                    let items: Vec<String> = arr
                        .iter()
                        .map(|(i, x)| format!("[{i}]={}", quote_if_needed(x)))
                        .collect();
                    s.push_str(&format!("declare {fl} {k}=({})\n", items.join(" ")));
                }
                None => s.push_str(&format!("declare {fl} {k}={}\n", quote_if_needed(&var.value))),
            }
        }
        out(&s);
        return Ok(0);
    }
    for n in names {
        let (name, val) = match n.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (n.clone(), None),
        };
        if sh.func_depth > 0 && !sh.is_array(&name) {
            sh.make_local(&name);
        }
        if let Some(v) = val {
            if let Err(e) = sh.set_var(&name, v) {
                sh.error(e);
                return Ok(1);
            }
        } else if !sh.vars.contains_key(&name) {
            if is_arr {
                let _ = sh.set_array(&name, vec![]);
            } else {
                let _ = sh.set_var(&name, String::new());
            }
        }
        if let Some(var) = sh.vars.get_mut(&name) {
            if export {
                var.exported = true;
            }
            if ro {
                var.readonly = true;
            }
        }
    }
    Ok(0)
}

fn b_unset(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut funcs = false;
    let mut st = 0;
    for n in &a[1..] {
        match n.as_str() {
            "-f" => funcs = true,
            "-v" => funcs = false,
            _ => {
                if let (Some(open), true) = (n.find('['), n.ends_with(']')) {
                    let name = &n[..open];
                    let idx = &n[open + 1..n.len() - 1];
                    if let Ok(i) = crate::arith::eval(idx, sh) {
                        let _ = sh.unset_elem(name, i);
                    }
                    continue;
                }
                if funcs {
                    sh.functions.remove(n);
                } else if let Err(e) = sh.unset_var(n) {
                    sh.error(format!("unset : {e}"));
                    st = 1;
                } else if !sh.vars.contains_key(n) && sh.functions.contains_key(n) && false {
                    sh.functions.remove(n);
                }
            }
        }
    }
    Ok(st)
}

fn b_set(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() == 1 {
        let mut v: Vec<(&String, &Var)> = sh.vars.iter().collect();
        v.sort_by_key(|(k, _)| k.as_str());
        let mut s = String::new();
        for (k, var) in v {
            s.push_str(&format!("{k}={}\n", quote_if_needed(&var.value)));
        }
        out(&s);
        return Ok(0);
    }
    let mut i = 1;
    while i < a.len() {
        let x = &a[i];
        if x == "--" {
            sh.positional = a[i + 1..].to_vec();
            return Ok(0);
        }
        if x == "-" {
            sh.opts.xtrace = false;
            sh.opts.verbose = false;
            sh.positional = a[i + 1..].to_vec();
            return Ok(0);
        }
        let on = x.starts_with('-');
        if !(x.starts_with('-') || x.starts_with('+')) || x.len() < 2 {
            sh.positional = a[i..].to_vec();
            return Ok(0);
        }
        for c in x[1..].chars() {
            match c {
                'e' => sh.opts.errexit = on,
                'u' => sh.opts.nounset = on,
                'x' => sh.opts.xtrace = on,
                'f' => sh.opts.noglob = on,
                'C' => sh.opts.noclobber = on,
                'v' => sh.opts.verbose = on,
                'a' => sh.opts.allexport = on,
                'm' | 'b' | 'h' | 'B' | 'H' => {}
                'o' => {
                    i += 1;
                    match a.get(i).map(|s| s.as_str()) {
                        None => {
                            let o = &sh.opts;
                            let rows = [
                                ("allexport", o.allexport),
                                ("errexit", o.errexit),
                                ("noclobber", o.noclobber),
                                ("noglob", o.noglob),
                                ("nounset", o.nounset),
                                ("pipefail", o.pipefail),
                                ("verbose", o.verbose),
                                ("xtrace", o.xtrace),
                            ];
                            let s: String = rows
                                .iter()
                                .map(|(n, v)| format!("{n:<12}{}\n", if *v { "activé" } else { "désactivé" }))
                                .collect();
                            out(&s);
                        }
                        Some(name) => match name {
                            "errexit" => sh.opts.errexit = on,
                            "nounset" => sh.opts.nounset = on,
                            "xtrace" => sh.opts.xtrace = on,
                            "pipefail" => sh.opts.pipefail = on,
                            "noglob" => sh.opts.noglob = on,
                            "noclobber" => sh.opts.noclobber = on,
                            "verbose" => sh.opts.verbose = on,
                            "allexport" => sh.opts.allexport = on,
                            "emacs" | "vi" | "monitor" | "posix" | "interactive-comments" | "histexpand" | "notify" | "ignoreeof" => {}
                            other => {
                                sh.error(format!("set : option inconnue « {other} »"));
                                return Ok(2);
                            }
                        },
                    }
                }
                other => {
                    sh.error(format!("set : option inconnue « -{other} »"));
                    return Ok(2);
                }
            }
        }
        i += 1;
    }
    Ok(0)
}

pub const SHOPT_NAMES: &[&str] = &[
    "autocd", "cdspell", "checkhash", "checkjobs", "checkwinsize", "cmdhist", "dotglob", "eval_unsafe_arith",
    "expand_aliases", "extglob", "extquote", "failglob", "globstar", "histappend", "histverify",
    "hostcomplete", "interactive_comments", "lastpipe", "lithist", "login_shell", "nocaseglob",
    "nocasematch", "nullglob", "progcomp", "promptvars", "sourcepath", "xpg_echo", "direxpand", "dirspell",
    "globasciiranges", "inherit_errexit", "huponexit", "no_empty_cmd_completion",
];

fn b_shopt(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut set: Option<bool> = None;
    let mut quiet = false;
    let mut names = Vec::new();
    for x in &a[1..] {
        match x.as_str() {
            "-s" => set = Some(true),
            "-u" => set = Some(false),
            "-q" => quiet = true,
            "-p" | "-o" => {}
            _ => names.push(x.clone()),
        }
    }
    let get = |sh: &Shell, n: &str| -> bool {
        match n {
            "nullglob" => sh.opts.nullglob,
            "dotglob" => sh.opts.dotglob,
            "nocaseglob" => sh.opts.nocaseglob,
            "nocasematch" => sh.opts.nocasematch,
            "globstar" | "expand_aliases" | "autocd" | "interactive_comments" | "extquote" | "progcomp" | "promptvars" | "sourcepath" | "checkwinsize" | "cmdhist" | "histappend" => true,
            "login_shell" => sh.login,
            other => sh.shopts.contains(other),
        }
    };
    if names.is_empty() {
        let mut s = String::new();
        for n in SHOPT_NAMES {
            let v = get(sh, n);
            if set.is_none() || set == Some(v) {
                s.push_str(&format!("{n:<24}{}\n", if v { "on" } else { "off" }));
            }
        }
        if !quiet {
            out(&s);
        }
        return Ok(0);
    }
    let mut st = 0;
    for n in names {
        if !SHOPT_NAMES.contains(&n.as_str()) {
            if !quiet {
                sh.error(format!("shopt : {n} : nom d'option invalide"));
            }
            st = 1;
            continue;
        }
        match set {
            Some(v) => match n.as_str() {
                "nullglob" => sh.opts.nullglob = v,
                "dotglob" => sh.opts.dotglob = v,
                "nocaseglob" => sh.opts.nocaseglob = v,
                "nocasematch" => sh.opts.nocasematch = v,
                other => {
                    if v {
                        sh.shopts.insert(other.to_string());
                    } else {
                        sh.shopts.remove(other);
                    }
                }
            },
            None => {
                let v = get(sh, &n);
                if !quiet {
                    out(&format!("{n:<24}{}\n", if v { "on" } else { "off" }));
                }
                if !v {
                    st = 1;
                }
            }
        }
    }
    Ok(st)
}

fn b_mapfile(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut strip = false;
    let mut delim = b'\n';
    let mut max: Option<usize> = None;
    let mut skip = 0usize;
    let mut name = "MAPFILE".to_string();
    let mut i = 1;
    while i < a.len() {
        match a[i].as_str() {
            "-t" => strip = true,
            "-d" => {
                i += 1;
                delim = a.get(i).and_then(|s| s.bytes().next()).unwrap_or(0);
            }
            "-n" => {
                i += 1;
                max = a.get(i).and_then(|s| s.parse().ok()).filter(|n| *n > 0);
            }
            "-s" => {
                i += 1;
                skip = a.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            x if !x.starts_with('-') => name = x.to_string(),
            _ => {}
        }
        i += 1;
    }
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    let mut vals = Vec::new();
    for (k, chunk) in data.split_inclusive(|b| *b == delim).enumerate() {
        if k < skip {
            continue;
        }
        if max.is_some_and(|m| vals.len() >= m) {
            break;
        }
        let mut c = chunk;
        if strip && c.last() == Some(&delim) {
            c = &c[..c.len() - 1];
        }
        vals.push(String::from_utf8_lossy(c).into_owned());
    }
    match sh.set_array(&name, vals) {
        Ok(()) => Ok(0),
        Err(e) => {
            sh.error(format!("mapfile : {e}"));
            Ok(1)
        }
    }
}

fn b_shift(sh: &mut Shell, a: &[String]) -> ExecResult {
    let n: usize = match a.get(1) {
        Some(s) => match s.parse() {
            Ok(n) => n,
            Err(_) => {
                sh.error(format!("shift : {s} : nombre attendu"));
                return Ok(1);
            }
        },
        None => 1,
    };
    if n > sh.positional.len() {
        return Ok(1);
    }
    sh.positional.drain(..n);
    Ok(0)
}

fn b_let(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut last = 0;
    for e in &a[1..] {
        match crate::arith::eval(e, sh) {
            Ok(v) => last = v,
            Err(m) => {
                sh.error(format!("let : {m}"));
                return Ok(1);
            }
        }
    }
    Ok((last == 0) as i32)
}

// --------------------------------------------------------------------------
// Contrôle
// --------------------------------------------------------------------------

fn level(sh: &Shell, a: &[String], what: &str) -> Result<usize, String> {
    let n = match a.get(1) {
        Some(s) => s
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or(format!("{what} : {s} : nombre positif attendu"))?,
        None => 1,
    };
    if sh.loop_depth == 0 {
        return Err(format!("{what} : seulement dans une boucle"));
    }
    Ok(n.min(sh.loop_depth))
}

fn b_break(sh: &mut Shell, a: &[String]) -> ExecResult {
    match level(sh, a, "break") {
        Ok(n) => Err(Flow::Break(n)),
        Err(e) => {
            sh.error(e);
            Ok(1)
        }
    }
}

fn b_continue(sh: &mut Shell, a: &[String]) -> ExecResult {
    match level(sh, a, "continue") {
        Ok(n) => Err(Flow::Continue(n)),
        Err(e) => {
            sh.error(e);
            Ok(1)
        }
    }
}

fn status_arg(sh: &Shell, a: &[String], what: &str) -> Result<i32, String> {
    match a.get(1) {
        Some(s) => s
            .parse::<i64>()
            .map(|n| (n & 0xff) as i32)
            .map_err(|_| format!("{what} : {s} : nombre attendu")),
        None => Ok(sh.last_status),
    }
}

fn b_return(sh: &mut Shell, a: &[String]) -> ExecResult {
    if sh.func_depth == 0 && sh.source_depth == 0 {
        sh.error("return : seulement dans une fonction ou un script sourcé");
        return Ok(1);
    }
    match status_arg(sh, a, "return") {
        Ok(n) => Err(Flow::Return(n)),
        Err(e) => {
            sh.error(e);
            Err(Flow::Return(2))
        }
    }
}

fn b_exit(sh: &mut Shell, a: &[String]) -> ExecResult {
    if sh.interactive && !sh.is_subshell {
        let stopped = sh.jobs.jobs.iter().filter(|j| j.is_stopped()).count();
        if stopped > 0 && !sh.vars.contains_key("__SHSH_EXIT_WARNED") {
            sh.error(format!("il reste {stopped} tâche(s) stoppée(s) — retapez exit pour quitter quand même"));
            let _ = sh.set_var("__SHSH_EXIT_WARNED", "1".into());
            return Ok(1);
        }
    }
    match status_arg(sh, a, "exit") {
        Ok(n) => Err(Flow::Exit(n)),
        Err(e) => {
            sh.error(e);
            Err(Flow::Exit(2))
        }
    }
}

fn b_eval(sh: &mut Shell, a: &[String]) -> ExecResult {
    let src = a[1..].join(" ");
    if src.trim().is_empty() {
        return Ok(0);
    }
    sh.run_source(&src, "")
}

fn find_source_file(sh: &mut Shell, name: &str) -> Option<String> {
    if name.contains('/') {
        return Some(name.to_string());
    }
    if std::path::Path::new(name).is_file() {
        return Some(name.to_string());
    }
    let path = sh.get_var("PATH").unwrap_or_default();
    for d in path.split(':') {
        let p = format!("{d}/{name}");
        if std::path::Path::new(&p).is_file() {
            return Some(p);
        }
    }
    None
}

pub fn source_file(sh: &mut Shell, path: &str, args: &[String]) -> ExecResult {
    let content = match std::fs::read(path) {
        Ok(c) => String::from_utf8_lossy(&c).into_owned(),
        Err(e) => {
            let why = if e.kind() == std::io::ErrorKind::NotFound {
                "fichier introuvable".to_string()
            } else {
                e.to_string()
            };
            sh.error(format!("{path} : {why}"));
            return Ok(1);
        }
    };
    let saved = if args.is_empty() {
        None
    } else {
        Some(std::mem::replace(&mut sh.positional, args.to_vec()))
    };
    sh.source_depth += 1;
    let r = sh.run_source(&content, path);
    sh.source_depth -= 1;
    if let Some(p) = saved {
        sh.positional = p;
    }
    match r {
        Err(Flow::Return(n)) => Ok(n),
        other => other,
    }
}

fn b_source(sh: &mut Shell, a: &[String]) -> ExecResult {
    let name = match a.get(1) {
        Some(n) => n.clone(),
        None => {
            sh.error(format!("{} : nom de fichier requis", a[0]));
            return Ok(2);
        }
    };
    let path = match find_source_file(sh, &name) {
        Some(p) => p,
        None => {
            sh.error(format!("{name} : fichier introuvable"));
            return Ok(1);
        }
    };
    source_file(sh, &path, &a[2..])
}

fn b_command(sh: &mut Shell, a: &[String]) -> ExecResult {
    // command -v / -V (l'exécution simple est gérée par dispatch)
    let verbose = a.get(1).map(|s| s.as_str()) == Some("-V");
    let mut st = 0;
    let mut s = String::new();
    for name in a.iter().skip(2) {
        if let Some(v) = sh.aliases.get(name) {
            if verbose {
                s.push_str(&format!("{name} est un alias vers « {v} »\n"));
            } else {
                s.push_str(&format!("alias {name}={}\n", quote_if_needed(v)));
            }
        } else if sh.functions.contains_key(name) || lookup(name).is_some() || crate::parser::RESERVED.contains(&name.as_str()) {
            if verbose {
                s.push_str(&format!("{name} est une commande interne ou une fonction\n"));
            } else {
                s.push_str(&format!("{name}\n"));
            }
        } else if let Some(p) = sh.find_in_path(name) {
            if verbose {
                s.push_str(&format!("{name} est {p}\n"));
            } else {
                s.push_str(&format!("{p}\n"));
            }
        } else {
            st = 1;
            if verbose {
                sh.error(format!("command : {name} : introuvable"));
            }
        }
    }
    out(&s);
    Ok(st)
}

fn b_type(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut kind_only = false;
    let mut path_only = false;
    let mut all = false;
    let mut st = 0;
    let mut s = String::new();
    for name in &a[1..] {
        match name.as_str() {
            "-t" => {
                kind_only = true;
                continue;
            }
            "-p" | "-P" => {
                path_only = true;
                continue;
            }
            "-a" => {
                all = true;
                continue;
            }
            _ => {}
        }
        let mut found = false;
        if !path_only {
            if let Some(v) = sh.aliases.get(name) {
                found = true;
                s.push_str(&if kind_only {
                    "alias\n".into()
                } else {
                    format!("{name} est un alias vers « {v} »\n")
                });
            }
            if (!found || all) && crate::parser::RESERVED.contains(&name.as_str()) {
                found = true;
                s.push_str(&if kind_only {
                    "keyword\n".into()
                } else {
                    format!("{name} est un mot-clé du shell\n")
                });
            }
            if (!found || all) && sh.functions.contains_key(name) {
                found = true;
                s.push_str(&if kind_only {
                    "function\n".into()
                } else {
                    format!("{name} est une fonction\n")
                });
            }
            if (!found || all) && lookup(name).is_some() {
                found = true;
                s.push_str(&if kind_only {
                    "builtin\n".into()
                } else {
                    format!("{name} est une commande interne de shsh\n")
                });
            }
        }
        if !found || all {
            if let Some(p) = sh.find_in_path(name) {
                found = true;
                s.push_str(&if kind_only {
                    "file\n".into()
                } else if path_only {
                    format!("{p}\n")
                } else {
                    format!("{name} est {p}\n")
                });
            }
        }
        if !found {
            st = 1;
            if !kind_only && !path_only {
                sh.error(format!("type : {name} : introuvable"));
            }
        }
    }
    out(&s);
    Ok(st)
}

fn b_hash(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.get(1).map(|s| s.as_str()) == Some("-r") {
        sh.cmd_cache.clear();
        return Ok(0);
    }
    if a.len() == 1 {
        let mut v: Vec<(&String, &String)> = sh.cmd_cache.iter().collect();
        v.sort();
        out(&v.iter().map(|(k, p)| format!("{k}\t{p}\n")).collect::<String>());
        return Ok(0);
    }
    let mut st = 0;
    for n in &a[1..] {
        if sh.find_in_path(n).is_none() {
            sh.error(format!("hash : {n} : introuvable"));
            st = 1;
        }
    }
    Ok(st)
}

// --------------------------------------------------------------------------
// Alias & abréviations
// --------------------------------------------------------------------------

fn b_alias(sh: &mut Shell, a: &[String]) -> ExecResult {
    let args: Vec<&String> = a[1..].iter().filter(|x| *x != "-p").collect();
    if args.is_empty() {
        let mut v: Vec<(&String, &String)> = sh.aliases.iter().collect();
        v.sort();
        out(&v
            .iter()
            .map(|(k, val)| format!("alias {k}={}\n", quote_if_needed(val)))
            .collect::<String>());
        return Ok(0);
    }
    let mut st = 0;
    for x in args {
        match x.split_once('=') {
            Some((k, v)) => {
                Rc::make_mut(&mut sh.aliases).insert(k.to_string(), v.to_string());
            }
            None => match sh.aliases.get(x.as_str()) {
                Some(v) => out(&format!("alias {x}={}\n", quote_if_needed(v))),
                None => {
                    sh.error(format!("alias : {x} : introuvable"));
                    st = 1;
                }
            },
        }
    }
    Ok(st)
}

fn b_unalias(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut st = 0;
    for x in &a[1..] {
        if x == "-a" {
            Rc::make_mut(&mut sh.aliases).clear();
            continue;
        }
        if Rc::make_mut(&mut sh.aliases).remove(x).is_none() {
            sh.error(format!("unalias : {x} : introuvable"));
            st = 1;
        }
    }
    Ok(st)
}

fn b_abbr(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() == 1 {
        let mut v: Vec<(&String, &String)> = sh.abbrs.iter().collect();
        v.sort();
        out(&v
            .iter()
            .map(|(k, val)| format!("abbr {k}={}\n", quote_if_needed(val)))
            .collect::<String>());
        return Ok(0);
    }
    if a[1] == "-e" || a[1] == "--erase" {
        for k in &a[2..] {
            sh.abbrs.remove(k);
        }
        sh.abbrs_changed = true;
        return Ok(0);
    }
    for x in &a[1..] {
        match x.split_once('=') {
            Some((k, v)) => {
                sh.abbrs.insert(k.to_string(), v.to_string());
            }
            None => {
                sh.error(format!("abbr : usage : abbr nom=expansion (ex. abbr gs='git status')"));
                return Ok(2);
            }
        }
    }
    sh.abbrs_changed = true;
    Ok(0)
}

// --------------------------------------------------------------------------
// Tâches
// --------------------------------------------------------------------------

fn job_arg(sh: &mut Shell, a: &[String], what: &str) -> Option<usize> {
    let id = match a.get(1) {
        Some(s) => sh.jobs.resolve(s),
        None => sh.jobs.current(),
    };
    if id.is_none() {
        sh.error(format!(
            "{what} : {} : tâche introuvable",
            a.get(1).map(|s| s.as_str()).unwrap_or("courante")
        ));
    }
    id
}

fn b_jobs(sh: &mut Shell, a: &[String]) -> ExecResult {
    sh.jobs.reap();
    let long = a.iter().any(|x| x == "-l");
    let pids = a.iter().any(|x| x == "-p");
    let mut s = String::new();
    let ids: Vec<usize> = sh.jobs.jobs.iter().map(|j| j.id).collect();
    for id in ids {
        let j = sh.jobs.get(id).unwrap();
        if pids {
            s.push_str(&format!("{}\n", j.pgid));
            continue;
        }
        let m = sh.jobs.marker(id);
        if long {
            s.push_str(&format!("[{id}]{m}  {} {:<22} {}\n", j.pgid, j.state_label(), j.text));
        } else {
            s.push_str(&format!("[{id}]{m}  {:<22} {}\n", j.state_label(), j.text));
        }
    }
    out(&s);
    // Les tâches terminées affichées sont retirées
    let done: Vec<usize> = sh.jobs.jobs.iter().filter(|j| j.is_done()).map(|j| j.id).collect();
    for id in done {
        sh.jobs.remove(id);
    }
    Ok(0)
}

fn continue_job(sh: &mut Shell, id: usize, fg: bool) -> i32 {
    let (pgid, text, tmodes) = {
        let j = sh.jobs.get_mut(id).unwrap();
        for p in j.procs.iter_mut() {
            if p.state == ProcState::Stopped {
                p.state = ProcState::Running;
            }
        }
        j.background = !fg;
        (j.pgid, j.text.clone(), j.tmodes.clone())
    };
    sh.jobs.touch(id);
    if fg {
        out(&format!("{text}\n"));
        if sh.job_control {
            let _ = unistd::tcsetpgrp(std::io::stdin(), pgid);
            if let Some(t) = tmodes {
                let _ = nix::sys::termios::tcsetattr(
                    std::io::stdin(),
                    nix::sys::termios::SetArg::TCSADRAIN,
                    &t,
                );
            }
        }
        let _ = signal::killpg(pgid, Signal::SIGCONT);
        sh.wait_fg(id)
    } else {
        let _ = signal::killpg(pgid, Signal::SIGCONT);
        out(&format!("[{id}]+ {text} &\n"));
        0
    }
}

fn b_fg(sh: &mut Shell, a: &[String]) -> ExecResult {
    if !sh.job_control {
        sh.error("fg : pas de contrôle des tâches");
        return Ok(1);
    }
    match job_arg(sh, a, "fg") {
        Some(id) => Ok(continue_job(sh, id, true)),
        None => Ok(1),
    }
}

fn b_bg(sh: &mut Shell, a: &[String]) -> ExecResult {
    if !sh.job_control {
        sh.error("bg : pas de contrôle des tâches");
        return Ok(1);
    }
    match job_arg(sh, a, "bg") {
        Some(id) => Ok(continue_job(sh, id, false)),
        None => Ok(1),
    }
}

fn b_disown(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.get(1).map(|s| s.as_str()) == Some("-a") {
        sh.jobs.clear();
        return Ok(0);
    }
    match job_arg(sh, a, "disown") {
        Some(id) => {
            sh.jobs.remove(id);
            Ok(0)
        }
        None => Ok(1),
    }
}

fn b_wait(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() == 1 {
        let ids: Vec<usize> = sh.jobs.jobs.iter().map(|j| j.id).collect();
        for id in ids {
            if sh.jobs.get(id).is_some_and(|j| !j.is_stopped()) {
                sh.wait_fg_silent(id);
            }
        }
        return Ok(0);
    }
    let mut st = 0;
    for x in &a[1..] {
        let id = if x.starts_with('%') {
            sh.jobs.resolve(x)
        } else {
            x.parse::<i32>().ok().and_then(|pid| {
                sh.jobs
                    .jobs
                    .iter()
                    .find(|j| j.procs.iter().any(|p| p.pid.as_raw() == pid))
                    .map(|j| j.id)
            })
        };
        match id {
            Some(id) => st = sh.wait_fg_silent(id),
            None => st = 127,
        }
    }
    Ok(st)
}

impl Shell {
    /// Attente d'une tâche (wait) sans donner le terminal.
    pub fn wait_fg_silent(&mut self, id: usize) -> i32 {
        use nix::sys::wait::{waitpid, WaitPidFlag};
        loop {
            match self.jobs.get(id) {
                Some(j) if j.is_done() => break,
                None => return 0,
                _ => {}
            }
            if INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
                return 130;
            }
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WUNTRACED)) {
                Ok(st) => {
                    self.jobs.update(st);
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
        let st = self.jobs.get(id).map(|j| j.status(self.opts.pipefail)).unwrap_or(0);
        self.jobs.remove(id);
        st
    }
}

fn b_kill(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut sig = libc::SIGTERM;
    let mut i = 1;
    if a.get(1).map(|s| s.as_str()) == Some("-l") {
        let mut s = String::new();
        for n in 1..32 {
            s.push_str(&format!("{n:>2}) {:<8}", signal_name(n)));
            if n % 6 == 0 {
                s.push('\n');
            }
        }
        s.push('\n');
        out(&s);
        return Ok(0);
    }
    if let Some(x) = a.get(1) {
        if x == "-s" || x == "-n" {
            match a.get(2).and_then(|s| parse_signal(s)) {
                Some(s) => sig = s,
                None => {
                    sh.error("kill : signal invalide");
                    return Ok(1);
                }
            }
            i = 3;
        } else if let Some(name) = x.strip_prefix('-') {
            match parse_signal(name) {
                Some(s) => sig = s,
                None => {
                    sh.error(format!("kill : {name} : signal invalide"));
                    return Ok(1);
                }
            }
            i = 2;
        }
    }
    if i >= a.len() {
        sh.error("kill : usage : kill [-s signal | -signal] pid | %tâche …");
        return Ok(2);
    }
    let mut st = 0;
    for x in &a[i..] {
        let r = if x.starts_with('%') {
            let target = sh.jobs.resolve(x).and_then(|id| {
                sh.jobs
                    .get(id)
                    .map(|j| (j.pgid, j.is_stopped(), j.procs.iter().map(|p| p.pid).collect::<Vec<_>>()))
            });
            match target {
                Some((pg, stopped, pids)) => {
                    let send = |s: i32| -> i32 {
                        if sh.job_control {
                            unsafe { libc::kill(-pg.as_raw(), s) }
                        } else {
                            // sans contrôle des tâches, pas de groupe dédié : un par un
                            let mut r = -1;
                            for p in &pids {
                                if unsafe { libc::kill(p.as_raw(), s) } == 0 {
                                    r = 0;
                                }
                            }
                            r
                        }
                    };
                    let r = send(sig);
                    // une tâche stoppée ne reçoit le signal qu'une fois relancée
                    if stopped && r == 0 && sig != libc::SIGCONT && sig != libc::SIGSTOP && sig != libc::SIGTSTP {
                        send(libc::SIGCONT);
                    }
                    r
                }
                None => {
                    sh.error(format!("kill : {x} : tâche introuvable"));
                    st = 1;
                    continue;
                }
            }
        } else {
            match x.parse::<i32>() {
                Ok(pid) => unsafe { libc::kill(pid, sig) },
                Err(_) => {
                    sh.error(format!("kill : {x} : pid ou %tâche attendu"));
                    st = 1;
                    continue;
                }
            }
        };
        if r < 0 {
            sh.error(format!("kill : ({x}) : {}", std::io::Error::last_os_error()));
            st = 1;
        }
    }
    Ok(st)
}

// --------------------------------------------------------------------------
// read
// --------------------------------------------------------------------------

fn b_read(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut raw = false;
    let mut silent = false;
    let mut prompt: Option<String> = None;
    let mut nchars: Option<usize> = None;
    let mut delim = b'\n';
    let mut timeout: Option<f64> = None;
    let mut array_name: Option<String> = None;
    let mut names = Vec::new();
    let mut i = 1;
    while i < a.len() {
        let x = &a[i];
        if x == "--" {
            names.extend(a[i + 1..].iter().cloned());
            break;
        }
        if !(x.starts_with('-') && x.len() > 1) || !names.is_empty() {
            names.push(x.clone());
            i += 1;
            continue;
        }
        // options groupées : -rs, -rd '' , -p"invite", -n3…
        let chars: Vec<char> = x[1..].chars().collect();
        let mut k = 0;
        while k < chars.len() {
            let c = chars[k];
            let takes = matches!(c, 'p' | 'n' | 'N' | 'd' | 't' | 'a' | 'u');
            let val = if takes {
                let rest: String = chars[k + 1..].iter().collect();
                if !rest.is_empty() {
                    k = chars.len();
                    Some(rest)
                } else {
                    i += 1;
                    k += 1;
                    a.get(i).cloned()
                }
            } else {
                k += 1;
                None
            };
            match c {
                'r' => raw = true,
                's' => silent = true,
                'e' => {}
                'p' => prompt = val,
                'n' | 'N' => nchars = val.and_then(|v| v.parse().ok()),
                'd' => delim = val.and_then(|v| v.bytes().next()).unwrap_or(0),
                't' => timeout = val.and_then(|v| v.parse().ok()),
                'a' => array_name = val,
                'u' => {}
                other => {
                    sh.error(format!("read : -{other} : option invalide"));
                    return Ok(2);
                }
            }
        }
        i += 1;
    }
    if names.is_empty() {
        names.push("REPLY".into());
    }
    if let Some(p) = &prompt {
        if unsafe { libc::isatty(0) } == 1 {
            let _ = write_fd(2, p);
        }
    }
    let saved_term = if silent && unsafe { libc::isatty(0) } == 1 {
        nix::sys::termios::tcgetattr(std::io::stdin()).ok().inspect(|t| {
            let mut n = t.clone();
            n.local_flags.remove(nix::sys::termios::LocalFlags::ECHO);
            let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &n);
        })
    } else {
        None
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut got_any = false;
    let mut eof = false;
    loop {
        if let Some(n) = nchars {
            if String::from_utf8_lossy(&buf).chars().count() >= n {
                break;
            }
        }
        if let Some(t) = timeout {
            let mut fds = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let r = unsafe { libc::poll(&mut fds, 1, (t * 1000.0) as i32) };
            if r == 0 {
                if let Some(t) = &saved_term {
                    let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t);
                }
                return Ok(142);
            }
        }
        let mut b = [0u8; 1];
        let n = unsafe { libc::read(0, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                if INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                continue;
            }
            eof = true;
            break;
        }
        if n == 0 {
            eof = true;
            break;
        }
        got_any = true;
        if b[0] == delim && nchars.is_none() {
            break;
        }
        if !raw && b[0] == b'\\' {
            let mut c = [0u8; 1];
            let n = unsafe { libc::read(0, c.as_mut_ptr() as *mut libc::c_void, 1) };
            if n <= 0 {
                break;
            }
            if c[0] == b'\n' {
                continue;
            }
            buf.push(0x01); // marqueur : caractère protégé
            buf.push(c[0]);
            continue;
        }
        buf.push(b[0]);
    }
    if let Some(t) = &saved_term {
        let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t);
        let _ = write_fd(2, "\n");
    }
    let line = String::from_utf8_lossy(&buf).into_owned();
    if let Some(an) = array_name {
        let ifs = sh.get_var("IFS").unwrap_or_else(|| " \t\n".into());
        let mut vals: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x01' {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                continue;
            }
            if ifs.contains(c) {
                if !cur.is_empty() || !c.is_whitespace() {
                    vals.push(std::mem::take(&mut cur));
                }
                continue;
            }
            cur.push(c);
        }
        if !cur.is_empty() {
            vals.push(cur);
        }
        if let Err(e) = sh.set_array(&an, vals) {
            sh.error(format!("read : {e}"));
            return Ok(1);
        }
        return Ok(if eof && buf.is_empty() { 1 } else { 0 });
    }
    // Découpage IFS : les caractères protégés (marqueur \x01) ne séparent pas
    let ifs = sh.get_var("IFS").unwrap_or_else(|| " \t\n".into());
    let mut fields: Vec<String> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut cur = String::new();
    let mut k = 0;
    let nvars = names.len();
    let mut in_field = false;
    while k < chars.len() {
        let c = chars[k];
        if c == '\x01' && k + 1 < chars.len() {
            cur.push(chars[k + 1]);
            in_field = true;
            k += 2;
            continue;
        }
        if ifs.contains(c) && fields.len() + 1 < nvars {
            if in_field || !c.is_whitespace() {
                fields.push(std::mem::take(&mut cur));
                in_field = false;
            }
            k += 1;
            continue;
        }
        if !in_field && ifs.contains(c) && c.is_whitespace() && cur.is_empty() {
            k += 1;
            continue;
        }
        cur.push(c);
        in_field = true;
        k += 1;
    }
    // le dernier champ garde le reste, sans blancs IFS finaux
    let ws: Vec<char> = ifs.chars().filter(|c| c.is_whitespace()).collect();
    fields.push(cur.trim_end_matches(&ws[..]).to_string());
    for (idx, name) in names.iter().enumerate() {
        let v = fields.get(idx).cloned().unwrap_or_default();
        let v = if nvars == 1 && name == "REPLY" && raw { line.replace('\x01', "") } else { v };
        if let Err(e) = sh.set_var(name, v) {
            sh.error(format!("read : {e}"));
            return Ok(1);
        }
    }
    Ok(if eof && !got_any || (eof && buf.is_empty()) { 1 } else if eof { 1 } else { 0 })
}

// --------------------------------------------------------------------------
// test / [
// --------------------------------------------------------------------------

pub fn test_unary(_sh: &mut Shell, op: &str, a: &str) -> Result<bool, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let md = || std::fs::metadata(a);
    let lmd = || std::fs::symlink_metadata(a);
    let acc = |f: nix::unistd::AccessFlags| nix::unistd::access(a, f).is_ok();
    Ok(match op {
        "-z" => a.is_empty(),
        "-n" => !a.is_empty(),
        "-e" | "-a" => md().is_ok(),
        "-f" => md().map(|m| m.is_file()).unwrap_or(false),
        "-d" => md().map(|m| m.is_dir()).unwrap_or(false),
        "-L" | "-h" => lmd().map(|m| m.file_type().is_symlink()).unwrap_or(false),
        "-r" => acc(nix::unistd::AccessFlags::R_OK),
        "-w" => acc(nix::unistd::AccessFlags::W_OK),
        "-x" => acc(nix::unistd::AccessFlags::X_OK),
        "-s" => md().map(|m| m.len() > 0).unwrap_or(false),
        "-p" => md().map(|m| m.file_type().is_fifo()).unwrap_or(false),
        "-S" => md().map(|m| m.file_type().is_socket()).unwrap_or(false),
        "-b" => md().map(|m| m.file_type().is_block_device()).unwrap_or(false),
        "-c" => md().map(|m| m.file_type().is_char_device()).unwrap_or(false),
        "-u" => md().map(|m| m.permissions().mode() & 0o4000 != 0).unwrap_or(false),
        "-g" => md().map(|m| m.permissions().mode() & 0o2000 != 0).unwrap_or(false),
        "-k" => md().map(|m| m.permissions().mode() & 0o1000 != 0).unwrap_or(false),
        "-O" => md().map(|m| m.uid() == nix::unistd::geteuid().as_raw()).unwrap_or(false),
        "-G" => md().map(|m| m.gid() == nix::unistd::getegid().as_raw()).unwrap_or(false),
        "-N" => md().map(|m| m.mtime() > m.atime()).unwrap_or(false),
        "-t" => {
            let fd: i32 = a.parse().map_err(|_| format!("{a} : descripteur attendu"))?;
            unsafe { libc::isatty(fd) == 1 }
        }
        "-v" => _sh.vars.contains_key(a),
        _ => return Err(format!("{op} : opérateur unaire inconnu")),
    })
}

pub fn test_binary(a: &str, op: &str, b: &str) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    let num = |s: &str| -> Result<i64, String> {
        s.trim()
            .parse::<i64>()
            .map_err(|_| format!("{s} : nombre entier attendu"))
    };
    Ok(match op {
        "=" | "==" => a == b,
        "!=" => a != b,
        "<" => a < b,
        ">" => a > b,
        "-eq" => num(a)? == num(b)?,
        "-ne" => num(a)? != num(b)?,
        "-lt" => num(a)? < num(b)?,
        "-le" => num(a)? <= num(b)?,
        "-gt" => num(a)? > num(b)?,
        "-ge" => num(a)? >= num(b)?,
        "-nt" => match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.mtime() > y.mtime() || (x.mtime() == y.mtime() && x.mtime_nsec() > y.mtime_nsec()),
            (Ok(_), Err(_)) => true,
            _ => false,
        },
        "-ot" => match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.mtime() < y.mtime() || (x.mtime() == y.mtime() && x.mtime_nsec() < y.mtime_nsec()),
            (Err(_), Ok(_)) => true,
            _ => false,
        },
        "-ef" => same_file(a, b),
        _ => return Err(format!("{op} : opérateur binaire inconnu")),
    })
}

fn is_binop(s: &str) -> bool {
    matches!(
        s,
        "=" | "==" | "!=" | "<" | ">" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "-nt" | "-ot" | "-ef"
    )
}

fn is_unop(s: &str) -> bool {
    s.len() == 2 && s.starts_with('-') && "zneafdLhrwxspSbcugkOGNtv".contains(&s[1..])
}

struct TestP<'a> {
    a: &'a [String],
    i: usize,
}

impl<'a> TestP<'a> {
    fn peek(&self) -> Option<&str> {
        self.a.get(self.i).map(|s| s.as_str())
    }
    fn or(&mut self, sh: &mut Shell) -> Result<bool, String> {
        let mut v = self.and(sh)?;
        while self.peek() == Some("-o") {
            self.i += 1;
            let r = self.and(sh)?;
            v = v || r;
        }
        Ok(v)
    }
    fn and(&mut self, sh: &mut Shell) -> Result<bool, String> {
        let mut v = self.not(sh)?;
        while self.peek() == Some("-a") {
            self.i += 1;
            let r = self.not(sh)?;
            v = v && r;
        }
        Ok(v)
    }
    fn not(&mut self, sh: &mut Shell) -> Result<bool, String> {
        if self.peek() == Some("!") && self.a.len() - self.i > 1 {
            self.i += 1;
            return Ok(!self.not(sh)?);
        }
        self.prim(sh)
    }
    fn prim(&mut self, sh: &mut Shell) -> Result<bool, String> {
        let rest = self.a.len() - self.i;
        let cur = self.peek().ok_or("argument attendu")?.to_string();
        // binaire prioritaire si le 2e est un opérateur
        if rest >= 3 && is_binop(&self.a[self.i + 1]) {
            let (x, op, y) = (cur, self.a[self.i + 1].clone(), self.a[self.i + 2].clone());
            self.i += 3;
            return test_binary(&x, &op, &y);
        }
        if cur == "(" && rest >= 3 {
            self.i += 1;
            let v = self.or(sh)?;
            if self.peek() != Some(")") {
                return Err("« ) » attendue".into());
            }
            self.i += 1;
            return Ok(v);
        }
        if is_unop(&cur) && rest >= 2 {
            let arg = self.a[self.i + 1].clone();
            self.i += 2;
            return test_unary(sh, &cur, &arg);
        }
        self.i += 1;
        Ok(!cur.is_empty())
    }
}

fn b_test(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut args = &a[1..];
    if a[0] == "[" {
        if args.last().map(|s| s.as_str()) != Some("]") {
            sh.error("[ : « ] » manquant");
            return Ok(2);
        }
        args = &args[..args.len() - 1];
    }
    if args.is_empty() {
        return Ok(1);
    }
    let mut p = TestP { a: args, i: 0 };
    match p.or(sh) {
        Ok(v) => {
            if p.i != args.len() {
                sh.error(format!("{} : trop d'arguments", a[0]));
                return Ok(2);
            }
            Ok((!v) as i32)
        }
        Err(e) => {
            sh.error(format!("{} : {e}", a[0]));
            Ok(2)
        }
    }
}

// --------------------------------------------------------------------------
// trap, getopts, umask, ulimit, times
// --------------------------------------------------------------------------

fn b_trap(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() == 1 || a[1] == "-p" {
        let mut v: Vec<(&i32, &String)> = sh.traps.iter().collect();
        v.sort();
        let s: String = v
            .iter()
            .map(|(sig, cmd)| {
                let n = if **sig == 0 { "EXIT".to_string() } else { signal_name(**sig) };
                format!("trap -- {} {n}\n", quote_if_needed(cmd))
            })
            .collect();
        out(&s);
        return Ok(0);
    }
    if a[1] == "-l" {
        return b_kill(sh, &["kill".into(), "-l".into()]);
    }
    let (action, sigs): (Option<String>, &[String]) = if a.len() == 2 || parse_signal(&a[1]).is_some() && a[1].parse::<i32>().is_ok() && a.len() == 2 {
        (None, &a[1..])
    } else if a[1] == "-" {
        (None, &a[2..])
    } else {
        (Some(a[1].clone()), &a[2..])
    };
    let mut st = 0;
    for s in sigs {
        let sig = match parse_signal(s) {
            Some(n) if (0..65).contains(&n) => n,
            _ => {
                if s == "ERR" || s == "DEBUG" || s == "RETURN" {
                    continue;
                }
                sh.error(format!("trap : {s} : signal invalide"));
                st = 1;
                continue;
            }
        };
        match &action {
            None => {
                sh.traps.remove(&sig);
                if sig > 0 {
                    if let Ok(sg) = Signal::try_from(sig) {
                        let interactive_kept = sh.interactive
                            && matches!(sg, Signal::SIGINT | Signal::SIGQUIT | Signal::SIGTSTP | Signal::SIGTTIN | Signal::SIGTTOU);
                        if interactive_kept {
                            if sg == Signal::SIGINT {
                                catch_signal(sg);
                            } else {
                                set_signal(sg, signal::SigHandler::SigIgn);
                            }
                        } else {
                            set_signal(sg, signal::SigHandler::SigDfl);
                        }
                    }
                }
            }
            Some(cmd) => {
                sh.traps.insert(sig, cmd.clone());
                if sig > 0 {
                    if let Ok(sg) = Signal::try_from(sig) {
                        if cmd.is_empty() {
                            set_signal(sg, signal::SigHandler::SigIgn);
                        } else {
                            catch_signal(sg);
                        }
                    }
                }
            }
        }
    }
    Ok(st)
}

fn b_getopts(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() < 3 {
        sh.error("getopts : usage : getopts chaîne_options nom [arguments]");
        return Ok(2);
    }
    let spec = a[1].clone();
    let var = a[2].clone();
    let args: Vec<String> = if a.len() > 3 { a[3..].to_vec() } else { sh.positional.clone() };
    let silent = spec.starts_with(':');
    let mut optind: usize = sh.get_var("OPTIND").and_then(|s| s.parse().ok()).unwrap_or(1);
    if optind < 1 {
        optind = 1;
    }
    // sous-position dans un groupe d'options (-abc)
    if optind != sh.getopts_pos_ind() {
        sh.getopts_pos = 1;
    }
    let arg = match args.get(optind - 1) {
        Some(x) if x.starts_with('-') && x.len() > 1 && x != "--" => x.clone(),
        Some(x) if x == "--" => {
            let _ = sh.set_var("OPTIND", (optind + 1).to_string());
            let _ = sh.set_var(&var, "?".into());
            return Ok(1);
        }
        _ => {
            let _ = sh.set_var(&var, "?".into());
            return Ok(1);
        }
    };
    let chars: Vec<char> = arg.chars().collect();
    let c = chars[sh.getopts_pos.min(chars.len() - 1)];
    let mut next_ind = optind;
    let mut next_pos = sh.getopts_pos + 1;
    if next_pos >= chars.len() {
        next_ind += 1;
        next_pos = 1;
    }
    let idx = spec.find(c).filter(|_| c != ':');
    let result;
    match idx {
        None => {
            if silent {
                let _ = sh.set_var("OPTARG", c.to_string());
            } else {
                sh.error(format!("option illégale -- {c}"));
                let _ = sh.unset_var("OPTARG");
            }
            result = "?".to_string();
        }
        Some(i) => {
            let needs_arg = spec[i + c.len_utf8()..].starts_with(':');
            if needs_arg {
                if sh.getopts_pos + 1 < chars.len() {
                    let v: String = chars[sh.getopts_pos + 1..].iter().collect();
                    let _ = sh.set_var("OPTARG", v);
                    next_ind = optind + 1;
                    next_pos = 1;
                    result = c.to_string();
                } else if let Some(v) = args.get(optind) {
                    let _ = sh.set_var("OPTARG", v.clone());
                    next_ind = optind + 2;
                    next_pos = 1;
                    result = c.to_string();
                } else if silent {
                    let _ = sh.set_var("OPTARG", c.to_string());
                    result = ":".to_string();
                    next_ind = optind + 1;
                    next_pos = 1;
                } else {
                    sh.error(format!("l'option requiert un argument -- {c}"));
                    result = "?".to_string();
                    next_ind = optind + 1;
                    next_pos = 1;
                }
            } else {
                let _ = sh.unset_var("OPTARG");
                result = c.to_string();
            }
        }
    }
    sh.getopts_pos = next_pos;
    let _ = sh.set_var("OPTIND", next_ind.to_string());
    sh.getopts_last_ind = next_ind;
    let _ = sh.set_var(&var, result);
    Ok(0)
}

impl Shell {
    fn getopts_pos_ind(&self) -> usize {
        self.getopts_last_ind
    }
}

fn b_umask(sh: &mut Shell, a: &[String]) -> ExecResult {
    let cur = unsafe { libc::umask(0) };
    unsafe {
        libc::umask(cur);
    }
    match a.get(1).map(|s| s.as_str()) {
        None => {
            out(&format!("{:04o}\n", cur));
            Ok(0)
        }
        Some("-S") => {
            let p = !cur & 0o777;
            let f = |s: u32| {
                format!(
                    "{}{}{}",
                    if p >> s & 4 != 0 { "r" } else { "" },
                    if p >> s & 2 != 0 { "w" } else { "" },
                    if p >> s & 1 != 0 { "x" } else { "" }
                )
            };
            out(&format!("u={},g={},o={}\n", f(6), f(3), f(0)));
            Ok(0)
        }
        Some(m) => match u32::from_str_radix(m, 8) {
            Ok(v) => {
                unsafe {
                    libc::umask(v as libc::mode_t);
                }
                Ok(0)
            }
            Err(_) => {
                sh.error(format!("umask : {m} : mode octal attendu"));
                Ok(1)
            }
        },
    }
}

fn b_ulimit(sh: &mut Shell, a: &[String]) -> ExecResult {
    use nix::sys::resource::{getrlimit, setrlimit, Resource};
    let mut res = Resource::RLIMIT_FSIZE;
    let mut val: Option<String> = None;
    let mut hard = false;
    let mut all = false;
    for x in &a[1..] {
        match x.as_str() {
            "-n" => res = Resource::RLIMIT_NOFILE,
            "-c" => res = Resource::RLIMIT_CORE,
            "-s" => res = Resource::RLIMIT_STACK,
            "-f" => res = Resource::RLIMIT_FSIZE,
            "-u" => res = Resource::RLIMIT_NPROC,
            "-v" => res = Resource::RLIMIT_AS,
            "-t" => res = Resource::RLIMIT_CPU,
            "-H" => hard = true,
            "-S" => hard = false,
            "-a" => all = true,
            v => val = Some(v.to_string()),
        }
    }
    let fmt = |v: u64| if v == libc::RLIM_INFINITY { "unlimited".to_string() } else { v.to_string() };
    if all {
        let rows = [
            ("fichiers ouverts (-n)", Resource::RLIMIT_NOFILE),
            ("taille des core (-c)", Resource::RLIMIT_CORE),
            ("pile, Ko (-s)", Resource::RLIMIT_STACK),
            ("taille fichier (-f)", Resource::RLIMIT_FSIZE),
            ("processus (-u)", Resource::RLIMIT_NPROC),
            ("temps CPU, s (-t)", Resource::RLIMIT_CPU),
        ];
        let mut s = String::new();
        for (n, r) in rows {
            if let Ok((soft, h)) = getrlimit(r) {
                s.push_str(&format!("{n:<24} {}\n", fmt(if hard { h } else { soft })));
            }
        }
        out(&s);
        return Ok(0);
    }
    let (soft, h) = match getrlimit(res) {
        Ok(x) => x,
        Err(e) => {
            sh.error(format!("ulimit : {e}"));
            return Ok(1);
        }
    };
    match val {
        None => {
            out(&format!("{}\n", fmt(if hard { h } else { soft })));
            Ok(0)
        }
        Some(v) => {
            let n = if v == "unlimited" {
                libc::RLIM_INFINITY
            } else {
                match v.parse::<u64>() {
                    Ok(n) => n,
                    Err(_) => {
                        sh.error(format!("ulimit : {v} : nombre attendu"));
                        return Ok(1);
                    }
                }
            };
            let r = if hard { setrlimit(res, soft.min(n), n) } else { setrlimit(res, n, h) };
            match r {
                Ok(()) => Ok(0),
                Err(e) => {
                    sh.error(format!("ulimit : {e}"));
                    Ok(1)
                }
            }
        }
    }
}

fn b_times(_sh: &mut Shell, _a: &[String]) -> ExecResult {
    let mut t: libc::tms = unsafe { std::mem::zeroed() };
    unsafe {
        libc::times(&mut t);
    }
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let f = |v: libc::clock_t| {
        let s = v as f64 / hz;
        format!("{}m{:.3}s", (s / 60.0) as u64, s % 60.0)
    };
    out(&format!(
        "{} {}\n{} {}\n",
        f(t.tms_utime),
        f(t.tms_stime),
        f(t.tms_cutime),
        f(t.tms_cstime)
    ));
    Ok(0)
}

// --------------------------------------------------------------------------
// history, help
// --------------------------------------------------------------------------

fn b_history(sh: &mut Shell, a: &[String]) -> ExecResult {
    let path = match &sh.history_path {
        Some(p) => p.clone(),
        None => return Ok(0),
    };
    if a.get(1).map(|s| s.as_str()) == Some("-c") {
        crate::editor::clear_history(sh);
        return Ok(0);
    }
    crate::editor::save_history_now(sh);
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let entries: Vec<String> = content
        .lines()
        .map(|l| l.replace("<\\n>", "\n"))
        .collect();
    let n: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(entries.len());
    let start = entries.len().saturating_sub(n);
    let mut s = String::new();
    for (i, e) in entries.iter().enumerate().skip(start) {
        s.push_str(&format!("\x1b[38;2;110;110;140m{:>5}\x1b[0m  {}\n", i + 1, e));
    }
    out(&s);
    Ok(0)
}

fn b_help(_sh: &mut Shell, a: &[String]) -> ExecResult {
    let v = "\x1b[1;38;2;124;108;255m";
    let p = "\x1b[38;2;255;77;141m";
    let d = "\x1b[38;2;140;140;170m";
    let r = "\x1b[0m";
    if let Some(name) = a.get(1) {
        return match NAMES.iter().find(|(n, _)| n == name) {
            Some((n, desc)) => {
                out(&format!("{v}{n}{r} — {desc}\n"));
                Ok(0)
            }
            None => {
                out(&format!("{name} : aucune aide\n"));
                Ok(1)
            }
        };
    }
    let mut s = format!(
        "\n  {v}shsh{r} {d}·{r} shadow shell {d}{}{r}\n\n",
        env!("CARGO_PKG_VERSION")
    );
    s.push_str(&format!("  {p}Édition{r}\n"));
    for (k, t) in [
        ("Tab", "complétion (commandes, fichiers, variables, options git…)"),
        ("→ / Ctrl+F", "accepte la suggestion grisée"),
        ("Alt+F", "accepte un mot de la suggestion"),
        ("Ctrl+R", "recherche dans l'historique"),
        ("Alt+Entrée", "nouvelle ligne sans exécuter"),
        ("Ctrl+Z / fg", "suspend / reprend une tâche"),
        ("!! / !$", "dernière commande / dernier argument"),
    ] {
        s.push_str(&format!("    {v}{k:<13}{r} {t}\n"));
    }
    s.push_str(&format!("\n  {p}Configuration{r}  {d}~/.shshrc{r}\n"));
    s.push_str(&format!(
        "    {d}alias, abbr gs='git status', fonctions shsh_precmd / shsh_preexec / chpwd{r}\n"
    ));
    s.push_str(&format!("\n  {p}Commandes internes{r}\n"));
    let mut line = String::from("    ");
    let mut width = 4;
    for (n, _) in NAMES {
        let w = n.chars().count() + 2;
        if width + w > 78 {
            s.push_str(&line);
            s.push('\n');
            line = String::from("    ");
            width = 4;
        }
        line.push_str(&format!("{v}{n}{r}  "));
        width += w;
    }
    s.push_str(&line);
    s.push_str(&format!("\n\n  {d}help <commande> pour le détail.{r}\n\n"));
    out(&s);
    Ok(0)
}
