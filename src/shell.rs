//! État global du shell : variables, fonctions, alias, options, signaux.

use crate::ast::FuncBody;
use crate::jobs::JobTable;
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::termios::Termios;
use nix::unistd::Pid;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);
pub static PENDING: [AtomicBool; 65] = [const { AtomicBool::new(false) }; 65];

extern "C" fn on_signal(sig: libc::c_int) {
    if sig == libc::SIGINT {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    if (0..65).contains(&sig) {
        PENDING[sig as usize].store(true, Ordering::SeqCst);
    }
}

pub fn catch_signal(sig: Signal) {
    let act = signal::SigAction::new(
        SigHandler::Handler(on_signal),
        signal::SaFlags::SA_RESTART,
        signal::SigSet::empty(),
    );
    unsafe {
        let _ = signal::sigaction(sig, &act);
    }
}

pub fn set_signal(sig: Signal, h: SigHandler) {
    unsafe {
        let _ = signal::signal(sig, h);
    }
}

#[derive(Debug, Clone)]
pub struct Var {
    pub value: String,
    pub exported: bool,
    pub readonly: bool,
    /// Tableau indexé (value == élément 0)
    pub array: Option<BTreeMap<usize, String>>,
    /// Tableau associatif : clés, parallèles aux valeurs de `array` (positions 0..n)
    pub keys: Option<Vec<String>>,
    /// declare -i : toute valeur affectée est évaluée comme une expression
    pub integer: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub errexit: bool,
    pub nounset: bool,
    pub xtrace: bool,
    pub pipefail: bool,
    pub noglob: bool,
    pub noclobber: bool,
    pub verbose: bool,
    pub allexport: bool,
    pub nullglob: bool,
    pub dotglob: bool,
    pub nocaseglob: bool,
    pub nocasematch: bool,
}

/// Contrôle de flux non local (break, continue, return, exit, Ctrl+C).
#[derive(Debug, Clone, Copy)]
pub enum Flow {
    Break(usize),
    Continue(usize),
    Return(i32),
    Exit(i32),
    Interrupt,
}

pub type ExecResult = Result<i32, Flow>;

pub struct Shell {
    pub vars: HashMap<String, Var>,
    pub local_stack: Vec<HashMap<String, Option<Var>>>,
    pub positional: Vec<String>,
    pub arg0: String,
    pub functions: HashMap<String, Rc<FuncBody>>,
    pub aliases: Rc<HashMap<String, String>>,
    pub abbrs: HashMap<String, String>,
    pub abbrs_changed: bool,
    pub last_status: i32,
    pub last_bg_pid: Option<i32>,
    pub opts: Options,
    pub interactive: bool,
    pub login: bool,
    pub job_control: bool,
    pub jobs: JobTable,
    pub shell_pgid: Pid,
    pub shell_tmodes: Option<Termios>,
    pub is_subshell: bool,
    pub main_pid: i32,
    pub traps: HashMap<i32, String>,
    /// Pièges du shell parent, gardés dans un sous-shell pour `trap -p`
    pub traps_parent: Option<HashMap<i32, String>>,
    pub cmd_cache: HashMap<String, String>,
    pub loop_depth: usize,
    pub func_depth: usize,
    /// > 0 quand on évalue une condition (if, while, &&, ||, !) : pas de set -e.
    pub cond_depth: usize,
    pub start: Instant,
    /// Ligne de la commande en cours ($LINENO)
    pub lineno: usize,
    pub cmdsub_status: Option<i32>,
    pub dir_stack: Vec<String>,
    pub history_path: Option<std::path::PathBuf>,
    pub getopts_pos: usize,
    pub getopts_last_ind: usize,
    pub source_depth: usize,
    pub procsub_fds: Vec<i32>,
    pub rand_state: u64,
    pub in_trap: bool,
    pub pending_arrays: HashMap<String, Vec<String>>,
    pub noexec: bool,
    pub shopts: std::collections::HashSet<String>,
    pub func_names: Vec<String>,
}

impl Shell {
    pub fn new() -> Self {
        let mut vars = HashMap::new();
        for (k, v) in std::env::vars_os() {
            if let (Ok(k), Ok(v)) = (k.into_string(), v.into_string()) {
                vars.insert(
                    k,
                    Var {
                        value: v,
                        exported: true,
                        readonly: false,
                        array: None,
                        keys: None,
                        integer: false,
                    },
                );
            }
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42)
            ^ (std::process::id() as u64) << 17;
        let mut sh = Shell {
            vars,
            local_stack: Vec::new(),
            positional: Vec::new(),
            arg0: "shsh".into(),
            functions: HashMap::new(),
            aliases: Rc::new(HashMap::new()),
            abbrs: HashMap::new(),
            abbrs_changed: false,
            last_status: 0,
            last_bg_pid: None,
            opts: Options::default(),
            interactive: false,
            login: false,
            job_control: false,
            jobs: JobTable::default(),
            shell_pgid: nix::unistd::getpgrp(),
            shell_tmodes: None,
            is_subshell: false,
            main_pid: std::process::id() as i32,
            traps: HashMap::new(),
            traps_parent: None,
            cmd_cache: HashMap::new(),
            loop_depth: 0,
            func_depth: 0,
            cond_depth: 0,
            start: Instant::now(),
            lineno: 0,
            cmdsub_status: None,
            dir_stack: Vec::new(),
            history_path: None,
            getopts_pos: 1,
            getopts_last_ind: 1,
            source_depth: 0,
            procsub_fds: Vec::new(),
            rand_state: seed | 1,
            in_trap: false,
            pending_arrays: HashMap::new(),
            noexec: false,
            shopts: Default::default(),
            func_names: Vec::new(),
        };
        // Variables de base
        let lvl = sh
            .get_var("SHLVL")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0);
        sh.set_exported("SHLVL", (lvl + 1).to_string());
        if let Ok(cwd) = std::env::current_dir() {
            let cwd = cwd.to_string_lossy().to_string();
            // Garder le PWD hérité s'il désigne bien le même dossier (chemins logiques)
            let keep = sh
                .get_var("PWD")
                .is_some_and(|p| same_file(&p, &cwd));
            if !keep {
                sh.set_exported("PWD", cwd);
            }
        }
        // Comme bash : PATH par défaut si l'environnement n'en fournit pas
        if sh.get_var("PATH").is_none_or(|p| p.is_empty()) {
            sh.set_exported("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
        }
        if sh.get_var("IFS").is_none() {
            sh.set_var_raw("IFS", " \t\n".into());
        }
        sh.set_var_raw("SHSH_VERSION", env!("CARGO_PKG_VERSION").into());
        sh.set_var_raw("OPTIND", "1".into());
        if sh.get_var("PS4").is_none() {
            sh.set_var_raw("PS4", "+ ".into());
        }
        sh.set_var_raw("PPID", nix::unistd::getppid().to_string());
        if sh.get_var("HOME").is_none() {
            if let Some(h) = home_of("") {
                sh.set_exported("HOME", h);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            sh.set_var_raw("SHSH", exe.to_string_lossy().into());
        }
        sh
    }

    // ------------------------------------------------------------------
    // Variables
    // ------------------------------------------------------------------

    pub fn get_var(&self, name: &str) -> Option<String> {
        self.vars.get(name).map(|v| v.value.clone())
    }

    /// Paramètres spéciaux et variables dynamiques compris.
    pub fn lookup(&mut self, name: &str) -> Option<String> {
        match name {
            "?" => return Some(self.last_status.to_string()),
            "$" => return Some(self.main_pid.to_string()),
            "!" => return self.last_bg_pid.map(|p| p.to_string()),
            "#" => return Some(self.positional.len().to_string()),
            "0" => return Some(self.arg0.clone()),
            "-" => return Some(self.flags_string()),
            "@" | "*" => {
                return Some(self.positional.join(" "));
            }
            "RANDOM" => {
                // xorshift
                let mut x = self.rand_state;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.rand_state = x;
                return Some(((x >> 33) % 32768).to_string());
            }
            "SECONDS" => return Some(self.start.elapsed().as_secs().to_string()),
            "LINENO" => return Some(self.lineno.to_string()),
            "UID" if !self.vars.contains_key("UID") => return Some(nix::unistd::getuid().to_string()),
            "EUID" if !self.vars.contains_key("EUID") => return Some(nix::unistd::geteuid().to_string()),
            "HOSTNAME" if !self.vars.contains_key("HOSTNAME") => {
                return nix::unistd::gethostname().ok().map(|h| h.to_string_lossy().into_owned());
            }
            "OSTYPE" if !self.vars.contains_key("OSTYPE") => return Some("linux-gnu".into()),
            "HOSTTYPE" if !self.vars.contains_key("HOSTTYPE") => return Some(std::env::consts::ARCH.into()),
            "MACHTYPE" if !self.vars.contains_key("MACHTYPE") => {
                return Some(format!("{}-pc-linux-gnu", std::env::consts::ARCH));
            }
            "EPOCHSECONDS" => {
                return Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                        .to_string(),
                )
            }
            "EPOCHREALTIME" => {
                let d = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                return Some(format!("{}.{:06}", d.as_secs(), d.subsec_micros()));
            }
            "BASHPID" | "SHSHPID" => return Some(std::process::id().to_string()),
            "FUNCNAME" => return self.func_names.last().cloned(),
            _ => {}
        }
        if let Ok(n) = name.parse::<usize>() {
            if n == 0 {
                return Some(self.arg0.clone());
            }
            return self.positional.get(n - 1).cloned();
        }
        self.get_var(name)
    }

    pub fn flags_string(&self) -> String {
        let mut s = String::new();
        if self.opts.errexit {
            s.push('e');
        }
        if self.opts.nounset {
            s.push('u');
        }
        if self.opts.xtrace {
            s.push('x');
        }
        if self.opts.noglob {
            s.push('f');
        }
        if self.opts.noclobber {
            s.push('C');
        }
        if self.interactive {
            s.push('i');
        }
        if self.job_control {
            s.push('m');
        }
        s
    }

    fn set_var_raw(&mut self, name: &str, value: String) {
        match self.vars.get_mut(name) {
            Some(v) => {
                if let Some(a) = v.array.as_mut() {
                    a.insert(0, value.clone());
                }
                v.value = value
            }
            None => {
                self.vars.insert(
                    name.to_string(),
                    Var {
                        value,
                        exported: false,
                        readonly: false,
                        array: None,
                        keys: None,
                        integer: false,
                    },
                );
            }
        }
    }

    pub fn set_var(&mut self, name: &str, value: String) -> Result<(), String> {
        if let Some(v) = self.vars.get(name) {
            if v.readonly {
                return Err(format!("{name} : variable en lecture seule"));
            }
        }
        if name == "PATH" {
            self.cmd_cache.clear();
        }
        if self.is_assoc(name) {
            return self.assoc_set(name, "0", value);
        }
        let value = if self.vars.get(name).is_some_and(|v| v.integer) {
            crate::arith::eval(&value, self)?.to_string()
        } else {
            value
        };
        let allexport = self.opts.allexport;
        match self.vars.get_mut(name) {
            Some(v) => {
                if let Some(a) = v.array.as_mut() {
                    a.insert(0, value.clone());
                }
                v.value = value;
                if allexport {
                    v.exported = true;
                }
            }
            None => {
                self.vars.insert(
                    name.to_string(),
                    Var {
                        value,
                        exported: allexport,
                        readonly: false,
                        array: None,
                        keys: None,
                        integer: false,
                    },
                );
            }
        }
        Ok(())
    }

    /// Valeurs d'une variable vue comme tableau (dans l'ordre des indices).
    pub fn get_array(&mut self, name: &str) -> Vec<String> {
        if name == "@" || name == "*" {
            return self.positional.clone();
        }
        match self.vars.get(name) {
            Some(v) => match &v.array {
                Some(a) => a.values().cloned().collect(),
                None => vec![v.value.clone()],
            },
            None => match self.lookup(name) {
                Some(v) => vec![v],
                None => vec![],
            },
        }
    }

    /// Indices (tableau indexé) ou clés (tableau associatif), pour ${!a[@]}.
    pub fn get_array_keys(&mut self, name: &str) -> Vec<String> {
        if name == "@" || name == "*" {
            return (1..=self.positional.len()).map(|i| i.to_string()).collect();
        }
        match self.vars.get(name) {
            Some(v) => match (&v.keys, &v.array) {
                (Some(k), _) => k.clone(),
                (None, Some(a)) => a.keys().map(|i| i.to_string()).collect(),
                (None, None) => vec!["0".into()],
            },
            None => vec![],
        }
    }

    // ----- Tableaux associatifs (declare -A) -----

    pub fn is_assoc(&self, name: &str) -> bool {
        self.vars.get(name).is_some_and(|v| v.keys.is_some())
    }

    fn assoc_pairs(&self, name: &str) -> Vec<(String, String)> {
        match self.vars.get(name) {
            Some(Var { keys: Some(k), array: Some(a), .. }) => {
                k.iter().cloned().zip(a.values().cloned()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Remplace tout le contenu d'un tableau associatif (le crée au besoin).
    pub fn assoc_store(&mut self, name: &str, pairs: Vec<(String, String)>) -> Result<(), String> {
        let (exported, readonly) = match self.vars.get(name) {
            Some(v) if v.readonly => return Err(format!("{name} : variable en lecture seule")),
            Some(v) => (v.exported, v.readonly),
            None => (false, false),
        };
        let value = pairs.iter().find(|(k, _)| k == "0").map(|(_, v)| v.clone()).unwrap_or_default();
        let (keys, vals): (Vec<String>, Vec<String>) = pairs.into_iter().unzip();
        self.vars.insert(
            name.to_string(),
            Var {
                value,
                exported,
                readonly,
                array: Some(vals.into_iter().enumerate().collect()),
                keys: Some(keys),
                integer: false,
            },
        );
        Ok(())
    }

    /// declare -A nom : sans effet si la variable est déjà associative.
    pub fn declare_assoc(&mut self, name: &str) -> Result<(), String> {
        if self.is_assoc(name) {
            return Ok(());
        }
        self.assoc_store(name, Vec::new())
    }

    pub fn assoc_get(&self, name: &str, key: &str) -> Option<String> {
        self.assoc_pairs(name).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn assoc_set(&mut self, name: &str, key: &str, val: String) -> Result<(), String> {
        let mut pairs = self.assoc_pairs(name);
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some(p) => p.1 = val,
            None => pairs.push((key.to_string(), val)),
        }
        self.assoc_store(name, pairs)
    }

    pub fn assoc_unset(&mut self, name: &str, key: &str) -> Result<(), String> {
        let mut pairs = self.assoc_pairs(name);
        pairs.retain(|(k, _)| k != key);
        self.assoc_store(name, pairs)
    }

    /// Affectation composée `nom=(…)` : mots simples, ou formes `[clé]=valeur`
    /// (obligatoires pour un tableau associatif, admises pour un tableau indexé).
    pub fn assign_compound(&mut self, name: &str, append: bool, vals: Vec<String>) -> Result<(), String> {
        fn cle_valeur(w: &str) -> Option<(&str, &str)> {
            let r = w.strip_prefix('[')?;
            let f = r.find("]=")?;
            Some((&r[..f], &r[f + 2..]))
        }
        if self.is_assoc(name) {
            // Une clé avec espaces non protégée ([y z]=2) arrive découpée : on la recolle
            let mut recolle: Vec<String> = Vec::new();
            let mut ouvert = false;
            for w in vals {
                if ouvert {
                    let dernier = recolle.last_mut().unwrap();
                    dernier.push(' ');
                    dernier.push_str(&w);
                    ouvert = !w.contains("]=");
                } else {
                    ouvert = w.starts_with('[') && !w.contains("]=");
                    recolle.push(w);
                }
            }
            let vals = recolle;
            let mut pairs = if append { self.assoc_pairs(name) } else { Vec::new() };
            for w in &vals {
                let (k, v) = cle_valeur(w)
                    .ok_or_else(|| format!("{name} : « {w} » — un tableau associatif attend [clé]=valeur"))?;
                match pairs.iter_mut().find(|(pk, _)| pk == k) {
                    Some(p) => p.1 = v.to_string(),
                    None => pairs.push((k.to_string(), v.to_string())),
                }
            }
            return self.assoc_store(name, pairs);
        }
        let indexes: Option<Vec<(usize, &str)>> = vals
            .iter()
            .map(|w| cle_valeur(w).and_then(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v))))
            .collect();
        if let Some(ix) = indexes.filter(|ix| !ix.is_empty()) {
            let mut m = if append { self.array_map(name) } else { BTreeMap::new() };
            for (i, v) in ix {
                m.insert(i, v.to_string());
            }
            return self.put_array(name, m);
        }
        if append {
            return self.append_array(name, vals);
        }
        self.set_array(name, vals)
    }

    fn array_index(&self, name: &str, idx: i64) -> Option<usize> {
        if idx >= 0 {
            return Some(idx as usize);
        }
        let next = self
            .vars
            .get(name)
            .and_then(|v| v.array.as_ref())
            .and_then(|a| a.keys().next_back().map(|k| k + 1))
            .unwrap_or(if self.vars.contains_key(name) { 1 } else { 0 });
        let j = next as i64 + idx;
        if j < 0 { None } else { Some(j as usize) }
    }

    pub fn get_elem(&mut self, name: &str, idx: i64) -> Option<String> {
        let i = self.array_index(name, idx)?;
        match self.vars.get(name) {
            Some(v) => match &v.array {
                Some(a) => a.get(&i).cloned(),
                None if i == 0 => Some(v.value.clone()),
                None => None,
            },
            None if i == 0 => self.lookup(name),
            None => None,
        }
    }

    pub fn is_array(&self, name: &str) -> bool {
        self.vars.get(name).is_some_and(|v| v.array.is_some())
    }

    fn put_array(&mut self, name: &str, map: BTreeMap<usize, String>) -> Result<(), String> {
        if let Some(v) = self.vars.get(name) {
            if v.readonly {
                return Err(format!("{name} : variable en lecture seule"));
            }
        }
        let first = map.get(&0).cloned().unwrap_or_default();
        let exported = self.vars.get(name).map(|v| v.exported).unwrap_or(false);
        self.vars.insert(
            name.to_string(),
            Var {
                value: first,
                exported,
                readonly: false,
                array: Some(map),
                keys: None,
                integer: false,
            },
        );
        Ok(())
    }

    fn array_map(&self, name: &str) -> BTreeMap<usize, String> {
        match self.vars.get(name) {
            Some(v) => match &v.array {
                Some(a) => a.clone(),
                None => BTreeMap::from([(0, v.value.clone())]),
            },
            None => BTreeMap::new(),
        }
    }

    pub fn set_array(&mut self, name: &str, vals: Vec<String>) -> Result<(), String> {
        self.put_array(name, vals.into_iter().enumerate().collect())
    }

    pub fn append_array(&mut self, name: &str, vals: Vec<String>) -> Result<(), String> {
        let mut m = self.array_map(name);
        let mut next = m.keys().next_back().map(|k| k + 1).unwrap_or(0);
        for v in vals {
            m.insert(next, v);
            next += 1;
        }
        self.put_array(name, m)
    }

    pub fn set_elem(&mut self, name: &str, idx: i64, val: String) -> Result<(), String> {
        let i = self
            .array_index(name, idx)
            .ok_or_else(|| format!("{name}[{idx}] : indice invalide"))?;
        let mut m = self.array_map(name);
        m.insert(i, val);
        self.put_array(name, m)
    }

    pub fn unset_elem(&mut self, name: &str, idx: i64) -> Result<(), String> {
        let Some(i) = self.array_index(name, idx) else { return Ok(()) };
        let mut m = self.array_map(name);
        m.remove(&i);
        self.put_array(name, m)
    }

    pub fn set_exported(&mut self, name: &str, value: String) {
        let _ = self.set_var(name, value);
        if let Some(v) = self.vars.get_mut(name) {
            v.exported = true;
        }
    }

    pub fn unset_var(&mut self, name: &str) -> Result<(), String> {
        if let Some(v) = self.vars.get(name) {
            if v.readonly {
                return Err(format!("{name} : variable en lecture seule"));
            }
        }
        if name == "PATH" {
            self.cmd_cache.clear();
        }
        self.vars.remove(name);
        Ok(())
    }

    /// `local` : mémorise l'ancienne valeur pour la restaurer au retour.
    pub fn make_local(&mut self, name: &str) {
        if let Some(scope) = self.local_stack.last_mut() {
            if !scope.contains_key(name) {
                scope.insert(name.to_string(), self.vars.get(name).cloned());
            }
        }
    }

    pub fn pop_locals(&mut self) {
        if let Some(scope) = self.local_stack.pop() {
            for (name, old) in scope {
                match old {
                    Some(v) => {
                        self.vars.insert(name, v);
                    }
                    None => {
                        self.vars.remove(&name);
                    }
                }
            }
        }
    }

    pub fn env_list(&self, extra: &[(String, String)]) -> Vec<std::ffi::CString> {
        let mut map: HashMap<&str, &str> = HashMap::new();
        for (k, v) in &self.vars {
            if v.exported {
                map.insert(k, &v.value);
            }
        }
        for (k, v) in extra {
            map.insert(k, v);
        }
        map.into_iter()
            .filter_map(|(k, v)| std::ffi::CString::new(format!("{k}={v}")).ok())
            .collect()
    }

    pub fn home(&self) -> String {
        self.get_var("HOME").unwrap_or_else(|| "/".into())
    }

    pub fn cwd(&self) -> String {
        self.get_var("PWD").unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/".into())
        })
    }

    pub fn error(&self, msg: impl AsRef<str>) {
        crate::exec::flush_stdout();
        // En interactif, le nom du shell en couleur braise repère l'erreur d'un coup d'œil
        if self.interactive && std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprintln!("\x1b[1;38;2;255;77;141mshsh\x1b[0m\x1b[38;2;98;104;140m:\x1b[0m {}", msg.as_ref());
        } else {
            eprintln!("shsh: {}", msg.as_ref());
        }
    }

    /// Recherche d'un exécutable dans PATH (avec cache).
    pub fn find_in_path(&mut self, name: &str) -> Option<String> {
        if name.contains('/') {
            return if is_executable(name) {
                Some(name.to_string())
            } else {
                None
            };
        }
        if let Some(p) = self.cmd_cache.get(name) {
            if is_executable(p) {
                return Some(p.clone());
            }
            self.cmd_cache.remove(name);
        }
        let path = self.get_var("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into());
        for dir in path.split(':') {
            let dir = if dir.is_empty() { "." } else { dir };
            let full = format!("{dir}/{name}");
            if is_executable(&full) && !std::path::Path::new(&full).is_dir() {
                self.cmd_cache.insert(name.to_string(), full.clone());
                return Some(full);
            }
        }
        None
    }

    pub fn next_random(&mut self) -> u64 {
        let mut x = self.rand_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rand_state = x;
        x
    }
}

impl crate::arith::Vars for Shell {
    fn get_var(&mut self, name: &str) -> Option<String> {
        if self.opts.nounset && self.lookup(name).is_none() {
            return None;
        }
        self.lookup(name)
    }
    fn set_var(&mut self, name: &str, val: String) -> Result<(), String> {
        Shell::set_var(self, name, val)
    }
}

pub fn is_executable(p: &str) -> bool {
    nix::unistd::access(p, nix::unistd::AccessFlags::X_OK).is_ok()
        && std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

pub fn same_file(a: &str, b: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Dossier personnel d'un utilisateur ("" = utilisateur courant).
pub fn home_of(user: &str) -> Option<String> {
    let u = if user.is_empty() {
        nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten()
    } else {
        nix::unistd::User::from_name(user).ok().flatten()
    };
    u.map(|u| u.dir.to_string_lossy().to_string())
}

pub fn signal_name(sig: i32) -> String {
    Signal::try_from(sig)
        .map(|s| s.as_str().trim_start_matches("SIG").to_string())
        .unwrap_or_else(|_| sig.to_string())
}

/// Clé de `traps` pour le pseudo-signal ERR (trap '…' ERR).
pub const TRAP_ERR: i32 = -1;

pub fn parse_signal(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return Some(n);
    }
    let up = s.to_ascii_uppercase();
    let name = if up.starts_with("SIG") { up } else { format!("SIG{up}") };
    if name == "SIGEXIT" {
        return Some(0);
    }
    name.parse::<Signal>().ok().map(|s| s as i32)
}

/// Description française d'un signal fatal (comme bash : « Erreur de segmentation »).
pub fn signal_message(sig: i32) -> &'static str {
    match sig {
        libc::SIGSEGV => "Erreur de segmentation",
        libc::SIGKILL => "Processus tué",
        libc::SIGTERM => "Terminé",
        libc::SIGABRT => "Abandon",
        libc::SIGBUS => "Erreur de bus",
        libc::SIGFPE => "Exception en virgule flottante",
        libc::SIGILL => "Instruction illégale",
        libc::SIGHUP => "Raccrochage",
        libc::SIGQUIT => "Quitter",
        libc::SIGALRM => "Minuterie expirée",
        libc::SIGUSR1 => "Signal utilisateur 1",
        libc::SIGUSR2 => "Signal utilisateur 2",
        libc::SIGTRAP => "Trappe de trace",
        libc::SIGSYS => "Appel système erroné",
        libc::SIGXCPU => "Limite de temps CPU dépassée",
        _ => "Tué par un signal",
    }
}
