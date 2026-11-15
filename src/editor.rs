//! Éditeur de ligne : coloration syntaxique, complétion, suggestions, validation.

use crate::parser::{highlight_spans, HlKind, Parser, RESERVED};
use crate::prompt::*;
use crate::shell::Shell;
use nu_ansi_term::{Color, Style};
use reedline::{
    default_emacs_keybindings, CompletionResult, Completer, DefaultHinter, EditCommand,
    Emacs, FileBackedHistory, Highlighter, ColumnarMenu, KeyCode, KeyModifiers, MenuBuilder, Reedline,
    ReedlineEvent, ReedlineMenu, Span, StyledText, Suggestion, ValidationResult, Validator,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Vue partagée de l'état du shell pour les composants de l'éditeur (Send).
#[derive(Default)]
pub struct EdData {
    pub builtins: HashSet<String>,
    pub functions: HashSet<String>,
    pub aliases: HashMap<String, String>,
    pub vars: Vec<String>,
    pub path: String,
    pub cwd: String,
    pub home: String,
    pub path_cmds: BTreeMap<String, String>,
    pub path_scanned: Option<(String, Instant)>,
    /// SHSH_HISTORY_IGNORE : les lignes qui correspondent ne vont pas dans l'historique
    pub history_ignore: Option<(String, regex::Regex)>,
    /// Options extraites des pages de manuel, par page
    pub man_opts: HashMap<String, Vec<(String, String)>>,
    /// Ctrl+R : filtre de l'historique tant que le menu est ouvert (None = menu fermé)
    pub hist_mode: Option<ModeHist>,
    pub mission: String,
    pub journal: std::path::PathBuf,
    /// journal lu une fois tant qu'il ne change pas : (date de modification, entrées)
    pub journal_cache: Option<(std::time::SystemTime, std::sync::Arc<Vec<crate::journal::Entree>>)>,
}

/// Les vues de l'historique, dans l'ordre où Ctrl+R les fait défiler.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ModeHist {
    Tout,
    Ici,
    Mission,
    Echecs,
}

impl ModeHist {
    pub fn suivant(self) -> Self {
        match self {
            ModeHist::Tout => ModeHist::Ici,
            ModeHist::Ici => ModeHist::Mission,
            ModeHist::Mission => ModeHist::Echecs,
            ModeHist::Echecs => ModeHist::Tout,
        }
    }
    pub fn libelle(self) -> &'static str {
        match self {
            ModeHist::Tout => "tout",
            ModeHist::Ici => "ce dossier",
            ModeHist::Mission => "mission",
            ModeHist::Echecs => "échecs",
        }
    }
}

impl EdData {
    /// Lignes de commande retenues par le filtre courant (None = pas de filtre).
    fn lignes_filtre(&mut self) -> Option<HashSet<String>> {
        let mode = self.hist_mode?;
        if mode == ModeHist::Tout {
            return None;
        }
        let mtime = std::fs::metadata(&self.journal).and_then(|m| m.modified()).ok();
        let a_jour = matches!((&self.journal_cache, mtime), (Some((t, _)), Some(m)) if *t == m);
        if !a_jour {
            let v = crate::journal::lire_fichier(&self.journal);
            self.journal_cache = mtime.map(|m| (m, std::sync::Arc::new(v)));
        }
        let entrees = self.journal_cache.as_ref().map(|(_, v)| v.clone()).unwrap_or_default();
        let garde = |e: &crate::journal::Entree| match mode {
            ModeHist::Ici => e.cwd == self.cwd,
            ModeHist::Mission => !self.mission.is_empty() && e.mission == self.mission,
            ModeHist::Echecs => e.code != 0 && e.code != 130,
            ModeHist::Tout => true,
        };
        Some(entrees.iter().filter(|e| garde(e)).map(|e| e.ligne.clone()).collect())
    }
}

impl EdData {
    fn refresh_path_cmds(&mut self) {
        let stale = match &self.path_scanned {
            Some((p, t)) => *p != self.path || t.elapsed().as_secs() > 30,
            None => true,
        };
        if !stale {
            return;
        }
        let mut m = BTreeMap::new();
        for dir in self.path.split(':').filter(|d| !d.is_empty()) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if m.contains_key(&name) {
                        continue;
                    }
                    let p = e.path();
                    if crate::shell::is_executable(&p.to_string_lossy()) {
                        m.insert(name, dir.to_string());
                    }
                }
            }
        }
        self.path_cmds = m;
        self.path_scanned = Some((self.path.clone(), Instant::now()));
    }

    fn command_exists(&mut self, w: &str) -> bool {
        if w.contains('/') {
            let p = expand_tilde(w, &self.home);
            let p = if p.starts_with('/') { p } else { format!("{}/{p}", self.cwd) };
            return crate::shell::is_executable(&p) || Path::new(&p).is_dir();
        }
        if self.builtins.contains(w)
            || self.functions.contains(w)
            || self.aliases.contains_key(w)
            || RESERVED.contains(&w)
        {
            return true;
        }
        self.refresh_path_cmds();
        if self.path_cmds.contains_key(w) {
            return true;
        }
        // auto-cd
        let p = if w.starts_with('/') { w.to_string() } else { format!("{}/{w}", self.cwd) };
        Path::new(&p).is_dir()
    }
}

pub type Shared = Arc<Mutex<EdData>>;

pub fn sync_data(sh: &Shell, data: &Shared) {
    let mut d = data.lock().unwrap();
    if d.builtins.is_empty() {
        d.builtins = crate::builtins::NAMES.iter().map(|(n, _)| n.to_string()).collect();
        d.builtins.insert("exec".into());
    }
    d.functions = sh.functions.keys().cloned().collect();
    d.aliases = (*sh.aliases).clone();
    d.vars = sh.vars.keys().cloned().collect();
    d.path = sh.get_var("PATH").unwrap_or_default();
    d.cwd = sh.cwd();
    d.home = sh.home();
    d.mission = sh.get_var("MISSION").unwrap_or_default();
    d.journal = crate::journal::chemin(sh);
    let pat = sh.get_var("SHSH_HISTORY_IGNORE").unwrap_or_default();
    if d.history_ignore.as_ref().map(|h| h.0.as_str()) != Some(pat.as_str()) {
        d.history_ignore = if pat.is_empty() {
            None
        } else {
            match regex::Regex::new(&pat) {
                Ok(re) => Some((pat, re)),
                Err(e) => {
                    sh.error(format!("SHSH_HISTORY_IGNORE : expression invalide : {e}"));
                    None
                }
            }
        };
    }
}

/// Historique fichier, avec filtre des lignes sensibles.
pub struct FilteredHistory {
    inner: FileBackedHistory,
    data: Shared,
}

impl reedline::History for FilteredHistory {
    fn save(&mut self, h: reedline::HistoryItem) -> reedline::Result<reedline::HistoryItem> {
        let skip = self
            .data
            .lock()
            .ok()
            .and_then(|d| d.history_ignore.as_ref().map(|(_, re)| re.is_match(&h.command_line)))
            .unwrap_or(false);
        if skip {
            return Ok(h);
        }
        self.inner.save(h)
    }
    fn load(&self, id: reedline::HistoryItemId) -> reedline::Result<reedline::HistoryItem> {
        self.inner.load(id)
    }
    fn count(&self, q: reedline::SearchQuery) -> reedline::Result<i64> {
        self.inner.count(q)
    }
    fn search(&self, q: reedline::SearchQuery) -> reedline::Result<Vec<reedline::HistoryItem>> {
        let res = self.inner.search(q)?;
        // Ctrl+R ouvert sur une vue (ce dossier, mission, échecs) : seulement ce que le journal y range
        let filtre = self.data.lock().ok().and_then(|mut d| d.lignes_filtre());
        Ok(match filtre {
            Some(f) => res.into_iter().filter(|h| f.contains(h.command_line.trim_end())).collect(),
            None => res,
        })
    }
    fn update(
        &mut self,
        id: reedline::HistoryItemId,
        f: &dyn Fn(reedline::HistoryItem) -> reedline::HistoryItem,
    ) -> reedline::Result<()> {
        self.inner.update(id, f)
    }
    fn clear(&mut self) -> reedline::Result<()> {
        self.inner.clear()
    }
    fn delete(&mut self, id: reedline::HistoryItemId) -> reedline::Result<()> {
        self.inner.delete(id)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.inner.sync()
    }
    fn session(&self) -> Option<reedline::HistorySessionId> {
        self.inner.session()
    }
}

fn expand_tilde(w: &str, home: &str) -> String {
    if w == "~" {
        home.to_string()
    } else if let Some(r) = w.strip_prefix("~/") {
        format!("{home}/{r}")
    } else {
        w.to_string()
    }
}

/// Fond de la ligne sélectionnée dans les menus : violet profond, lisible
/// avec le texte comme avec les descriptions.
const SELECTION: Color = Color::Rgb(52, 46, 110);

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

// --------------------------------------------------------------------------
// Coloration
// --------------------------------------------------------------------------

pub struct ShshHighlighter {
    pub data: Shared,
}

impl Highlighter for ShshHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styles: Vec<Style> = vec![Style::new().fg(rgb(TEXTE)); line.len()];
        let mut d = self.data.lock().unwrap();
        for (a, b, k) in highlight_spans(line) {
            if a >= b || b > line.len() {
                continue;
            }
            let word = &line[a..b];
            let st = match k {
                HlKind::Command => {
                    if d.command_exists(word) {
                        Style::new().fg(rgb(VIOLET_CLAIR)).bold()
                    } else {
                        Style::new().fg(rgb(BRAISE))
                    }
                }
                HlKind::Keyword => Style::new().fg(Color::Rgb(199, 146, 234)).italic(),
                HlKind::Arg => {
                    let p = expand_tilde(word, &d.home);
                    let full = if p.starts_with('/') { p } else { format!("{}/{p}", d.cwd) };
                    if word.len() > 1 && !word.contains(['*', '?', '$']) && Path::new(&full).exists() {
                        Style::new().fg(rgb(TEXTE)).underline()
                    } else {
                        Style::new().fg(rgb(TEXTE))
                    }
                }
                HlKind::Option => Style::new().fg(rgb(CYAN)),
                HlKind::String => Style::new().fg(rgb(VERT)),
                HlKind::Variable => Style::new().fg(rgb(OR)),
                HlKind::Operator => Style::new().fg(rgb(BRAISE)).bold(),
                HlKind::Redirect => Style::new().fg(Color::Rgb(255, 158, 100)),
                HlKind::Comment => Style::new().fg(rgb(BRUME)).italic(),
                HlKind::Assign => Style::new().fg(Color::Rgb(187, 154, 247)),
                HlKind::Error => Style::new().fg(rgb(BRAISE)).underline(),
            };
            for s in styles.iter_mut().take(b).skip(a) {
                *s = st;
            }
        }
        // Regroupe les caractères consécutifs de même style
        let mut out = StyledText::new();
        let mut cur = String::new();
        let mut cur_style: Option<Style> = None;
        for (i, ch) in line.char_indices() {
            let st = styles[i];
            if cur_style != Some(st) {
                if let Some(s) = cur_style {
                    out.push((s, std::mem::take(&mut cur)));
                }
                cur_style = Some(st);
            }
            cur.push(ch);
        }
        if let Some(s) = cur_style {
            out.push((s, cur));
        }
        out
    }
}

// --------------------------------------------------------------------------
// Validation (saisie multiligne)
// --------------------------------------------------------------------------

pub struct ShshValidator;

impl Validator for ShshValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        let trimmed = line.trim_end_matches([' ', '\t']);
        if trimmed.ends_with('\\') && !trimmed.ends_with("\\\\") {
            return ValidationResult::Incomplete;
        }
        match Parser::new(line).parse_program() {
            Err(e) if e.incomplete => ValidationResult::Incomplete,
            _ => ValidationResult::Complete,
        }
    }
}

// --------------------------------------------------------------------------
// Complétion
// --------------------------------------------------------------------------

pub struct ShshCompleter {
    pub data: Shared,
}

const GIT_CMDS: &[(&str, &str)] = &[
    ("add", "ajoute au prochain commit"),
    ("bisect", "recherche dichotomique d'un bug"),
    ("blame", "auteur de chaque ligne"),
    ("branch", "liste/crée/supprime des branches"),
    ("checkout", "change de branche ou restaure"),
    ("cherry-pick", "applique un commit"),
    ("clone", "clone un dépôt"),
    ("commit", "enregistre les changements"),
    ("diff", "différences"),
    ("fetch", "récupère depuis le distant"),
    ("grep", "cherche dans le dépôt"),
    ("init", "crée un dépôt"),
    ("log", "historique"),
    ("merge", "fusionne une branche"),
    ("mv", "déplace un fichier suivi"),
    ("pull", "récupère et fusionne"),
    ("push", "envoie vers le distant"),
    ("rebase", "rejoue des commits"),
    ("reflog", "journal des références"),
    ("remote", "gère les distants"),
    ("reset", "réinitialise HEAD"),
    ("restore", "restaure des fichiers"),
    ("revert", "annule un commit"),
    ("rm", "retire un fichier suivi"),
    ("show", "affiche un objet"),
    ("stash", "met de côté les changements"),
    ("status", "état de l'arbre de travail"),
    ("switch", "change de branche"),
    ("tag", "étiquettes"),
    ("worktree", "arbres de travail multiples"),
];

const PREFIX_CMDS: &[&str] = &[
    "sudo", "doas", "time", "exec", "command", "builtin", "nohup", "env", "watch", "nice", "strace",
    "ltrace", "gdb", "proxychains", "proxychains4", "torsocks", "xargs", "timeout", "unbuffer",
    "run0",
];

fn escape_path(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if " \t'\"\\$`!&|;<>()[]{}*?#~".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Correspondance floue (sous-séquence) : renvoie les indices trouvés.
fn fuzzy(pat: &str, s: &str) -> Option<Vec<usize>> {
    let mut idx = Vec::new();
    let mut it = s.char_indices();
    for pc in pat.chars() {
        let pl = pc.to_lowercase().next().unwrap_or(pc);
        loop {
            match it.next() {
                Some((i, c)) if c.to_lowercase().next().unwrap_or(c) == pl => {
                    idx.push(i);
                    break;
                }
                Some(_) => continue,
                None => return None,
            }
        }
    }
    Some(idx)
}

fn make(value: String, desc: Option<String>, span: Span, space: bool, style: Option<Style>) -> Suggestion {
    Suggestion {
        value,
        description: desc,
        style,
        span,
        append_whitespace: space,
        ..Default::default()
    }
}

/// Début du mot sous le curseur (en octets), en tenant compte des \ et guillemets.
fn word_start(line: &str, pos: usize) -> usize {
    let before = &line[..pos];
    let chars: Vec<(usize, char)> = before.char_indices().collect();
    let mut start = 0;
    let mut in_s = false;
    let mut in_d = false;
    let mut k = 0;
    while k < chars.len() {
        let (i, c) = chars[k];
        match c {
            '\\' if !in_s => {
                k += 2;
                continue;
            }
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            ' ' | '\t' | '\n' | ';' | '|' | '&' | '(' | ')' | '<' | '>' if !in_s && !in_d => {
                start = i + c.len_utf8();
            }
            _ => {}
        }
        k += 1;
    }
    start
}

/// Mots déjà tapés dans la commande courante (avant le mot en cours).
fn command_words(prefix: &str) -> Vec<String> {
    let mut seg_start = 0;
    let spans = highlight_spans(prefix);
    for (a, b, k) in &spans {
        if *k == HlKind::Operator {
            let op = &prefix[*a..*b];
            if op != ")" {
                seg_start = *b;
            }
        }
    }
    highlight_spans(&prefix[seg_start..])
        .into_iter()
        .filter(|(_, _, k)| matches!(k, HlKind::Command | HlKind::Arg | HlKind::Option | HlKind::Keyword | HlKind::Assign))
        .map(|(a, b, _)| prefix[seg_start + a..seg_start + b].to_string())
        .filter(|w| !w.contains('=') || w.starts_with('-'))
        .collect()
}

impl ShshCompleter {
    fn files(&self, word: &str, span: Span, dirs_only: bool, home: &str, cwd: &str) -> Vec<Suggestion> {
        let quoted = word.starts_with('\'') || word.starts_with('"');
        let raw = word.trim_start_matches(['\'', '"']).replace('\\', "");
        let (dir_part, file_part) = match raw.rfind('/') {
            Some(i) => (&raw[..=i], &raw[i + 1..]),
            None => ("", raw.as_str()),
        };
        let real_dir = {
            let d = expand_tilde(dir_part, home);
            let d = if d.is_empty() {
                cwd.to_string()
            } else if d.starts_with('/') {
                d
            } else {
                format!("{cwd}/{d}")
            };
            PathBuf::from(d)
        };
        let rd = match std::fs::read_dir(&real_dir) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        let mut entries: Vec<(String, bool, bool, bool)> = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && !file_part.starts_with('.') {
                continue;
            }
            let p = e.path();
            let is_dir = p.is_dir();
            if dirs_only && !is_dir {
                continue;
            }
            let is_link = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            let is_exec = !is_dir && crate::shell::is_executable(&p.to_string_lossy());
            entries.push((name, is_dir, is_link, is_exec));
        }
        let mut pick: Vec<&(String, bool, bool, bool)> =
            entries.iter().filter(|e| e.0.starts_with(file_part)).collect();
        if pick.is_empty() {
            let low = file_part.to_lowercase();
            pick = entries.iter().filter(|e| e.0.to_lowercase().starts_with(&low)).collect();
        }
        if pick.is_empty() && !file_part.is_empty() {
            pick = entries.iter().filter(|e| fuzzy(file_part, &e.0).is_some()).collect();
        }
        pick.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        pick.into_iter()
            .take(500)
            .map(|(name, is_dir, is_link, is_exec)| {
                let shown = if quoted {
                    format!("{}{dir_part}{name}", &word[..1])
                } else {
                    format!("{}{}", escape_path_dir(dir_part), escape_path(name))
                };
                let (val, desc, style) = if *is_dir {
                    (format!("{shown}/"), "dossier", Style::new().fg(rgb(VIOLET_CLAIR)).bold())
                } else if *is_link {
                    (shown, "lien", Style::new().fg(rgb(CYAN)))
                } else if *is_exec {
                    (shown, "exécutable", Style::new().fg(rgb(VERT)))
                } else {
                    (shown, "fichier", Style::new().fg(rgb(TEXTE)))
                };
                let _ = desc;
                let mut sug = make(val, None, span, !*is_dir, Some(style));
                sug.display_override = Some(if *is_dir { format!("{name}/") } else { name.clone() });
                sug
            })
            .collect()
    }
}

fn escape_path_dir(d: &str) -> String {
    // Ne pas échapper le ~ initial
    if let Some(r) = d.strip_prefix('~') {
        format!("~{}", escape_path(r))
    } else {
        escape_path(d)
    }
}

fn git_branches(cwd: &str) -> Vec<String> {
    std::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes", "refs/tags"])
        .current_dir(cwd)
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default()
}

fn ssh_hosts(home: &str) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(s) = std::fs::read_to_string(format!("{home}/.ssh/config")) {
        for l in s.lines() {
            let l = l.trim();
            if let Some(h) = l.strip_prefix("Host ").or_else(|| l.strip_prefix("host ")) {
                for x in h.split_whitespace() {
                    if !x.contains(['*', '?']) {
                        v.push(x.to_string());
                    }
                }
            }
        }
    }
    if let Ok(s) = std::fs::read_to_string(format!("{home}/.ssh/known_hosts")) {
        for l in s.lines() {
            if let Some(h) = l.split_whitespace().next() {
                if h.starts_with('|') {
                    continue;
                }
                for x in h.split(',') {
                    let x = x.trim_start_matches('[');
                    let x = x.split(']').next().unwrap_or(x);
                    v.push(x.to_string());
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Options d'une commande, lues dans sa page de manuel (comme fish).
fn man_options(page: &str) -> Vec<(String, String)> {
    if page.is_empty() || page.contains('/') {
        return vec![];
    }
    let child = std::process::Command::new("man")
        .args(["-P", "cat", page])
        .env("MANWIDTH", "200")
        .env("MAN_KEEP_FORMATTING", "")
        .env("GROFF_NO_SGR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return vec![] };
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed().as_millis() < 1500 => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return vec![];
            }
        }
    }
    let Ok(out) = child.wait_with_output() else { return vec![] };
    // retire le surlignage par surfrappe (x\bx) et les séquences ANSI
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut text = String::new();
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if it.peek() == Some(&'\x08') {
            it.next();
            continue;
        }
        text.push(c);
    }
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let text = ansi.replace_all(&text, "");
    let opt_re = regex::Regex::new(r"^\s{2,}(-{1,2}[A-Za-z0-9?@][\w\-]*(?:[=\[ ][^,]*?)?(?:,\s+-{1,2}[A-Za-z0-9?@][\w\-]*(?:[=\[ ][^,]*?)?)*)(?:\s{2,}(.*))?$").unwrap();
    let tok_re = regex::Regex::new(r"(?:^|[,\s])(-{1,2}[A-Za-z0-9?@][\w\-]*)").unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let mut res: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(c) = opt_re.captures(l) else { continue };
        let spec = c.get(1).unwrap().as_str();
        let mut desc = c.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        if desc.is_empty() {
            desc = lines.get(i + 1).map(|x| x.trim().to_string()).unwrap_or_default();
        }
        if desc.starts_with('-') {
            desc.clear();
        }
        let desc = shorten(&desc, 64);
        for t in tok_re.captures_iter(spec) {
            let o = t[1].to_string();
            if o.len() > 1 && seen.insert(o.clone()) {
                res.push((o, desc.clone()));
            }
        }
    }
    res
}

/// Coupe proprement sur un mot, avec « … ».
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => cut[..i].to_string(),
        _ => cut,
    };
    format!("{}…", cut.trim_end_matches([',', ';', '.', ' ']))
}

fn make_targets(cwd: &str) -> Vec<String> {
    let s = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .find_map(|f| std::fs::read_to_string(format!("{cwd}/{f}")).ok())
        .unwrap_or_default();
    let mut v: Vec<String> = s
        .lines()
        .filter_map(|l| {
            if l.starts_with(['\t', ' ', '#', '.']) {
                return None;
            }
            let (t, rest) = l.split_once(':')?;
            if rest.starts_with('=') || t.contains(['$', '%', '=']) {
                return None;
            }
            Some(t.split_whitespace().map(String::from).collect::<Vec<_>>())
        })
        .flatten()
        .collect();
    v.sort();
    v.dedup();
    v
}

impl Completer for ShshCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let r = self.complete_inner(line, pos);
        if r.suggestions().is_empty() {
            // Jamais de menu vide (« NO RECORDS FOUND ») : on garde le mot tel quel
            let start = word_start(line, pos);
            if start < pos {
                return CompletionResult::fresh(vec![make(
                    line[start..pos].to_string(),
                    None,
                    Span { start, end: pos },
                    false,
                    None,
                )]);
            }
        }
        r
    }
}

impl ShshCompleter {
    fn complete_inner(&mut self, line: &str, pos: usize) -> CompletionResult {
        let start = word_start(line, pos);
        let word = &line[start..pos];
        let span = Span { start, end: pos };
        let prefix = &line[..start];
        let mut words = command_words(prefix);
        // Les préfixes (sudo, time…) laissent la position de commande ouverte
        while words.first().is_some_and(|w| PREFIX_CMDS.contains(&w.as_str())) {
            words.remove(0);
            while words.first().is_some_and(|w| w.starts_with('-')) {
                words.remove(0);
            }
        }
        let mut d = self.data.lock().unwrap();
        let home = d.home.clone();
        let cwd = d.cwd.clone();
        let mut out: Vec<Suggestion> = Vec::new();

        // $VARIABLE
        if let Some(vp) = word.rfind('$') {
            let after = &word[vp + 1..];
            let brace = after.starts_with('{');
            let name_part = after.trim_start_matches('{');
            if name_part.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let mut names: Vec<String> = d.vars.iter().filter(|v| v.starts_with(name_part)).cloned().collect();
                names.sort();
                let base = &word[..vp];
                for n in names {
                    let v = if brace { format!("{base}${{{n}}}") } else { format!("{base}${n}") };
                    out.push(make(v, Some("variable".into()), span, false, Some(Style::new().fg(rgb(OR)))));
                }
                return CompletionResult::fresh(out);
            }
        }

        let cmd_pos = words.is_empty() && !word.contains('/') && !word.starts_with('~') && !word.starts_with('.');
        if cmd_pos {
            d.refresh_path_cmds();
            let mut seen = HashSet::new();
            let mut add = |name: &str, desc: String, style: Style, out: &mut Vec<Suggestion>| {
                if seen.insert(name.to_string()) {
                    out.push(make(name.to_string(), Some(desc), span, true, Some(style)));
                }
            };
            let mut cands: Vec<(String, String, Style)> = Vec::new();
            for (a, v) in &d.aliases {
                cands.push((a.clone(), format!("alias → {v}"), Style::new().fg(Color::Rgb(187, 154, 247))));
            }
            for f in &d.functions {
                cands.push((f.clone(), "fonction".into(), Style::new().fg(rgb(OR))));
            }
            for (n, desc) in crate::builtins::NAMES {
                cands.push((n.to_string(), format!("interne · {desc}"), Style::new().fg(rgb(VIOLET_CLAIR))));
            }
            for k in RESERVED {
                cands.push((k.to_string(), "mot-clé".into(), Style::new().fg(Color::Rgb(199, 146, 234))));
            }
            for (c, dir) in &d.path_cmds {
                cands.push((c.clone(), dir.clone(), Style::new().fg(rgb(TEXTE))));
            }
            let mut exact: Vec<&(String, String, Style)> = cands.iter().filter(|c| c.0.starts_with(word)).collect();
            if exact.is_empty() && !word.is_empty() {
                exact = cands.iter().filter(|c| fuzzy(word, &c.0).is_some()).collect();
            }
            exact.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
            for (n, desc, st) in exact.into_iter().take(300) {
                add(n, desc.clone(), *st, &mut out);
            }
            drop(d);
            // dossiers (auto-cd)
            if !word.is_empty() {
                out.extend(self.files(word, span, true, &home, &cwd));
            }
            return CompletionResult::fresh(out);
        }

        let cmd = words.first().cloned().unwrap_or_default();
        let argn = words.len();

        // --options depuis la page de manuel
        if word.starts_with('-') && !cmd.is_empty() {
            let page = if matches!(cmd.as_str(), "git" | "systemctl" | "docker" | "cargo" | "apt" | "podman") && argn >= 2 && !words[1].starts_with('-') {
                format!("{cmd}-{}", words[1])
            } else {
                cmd.rsplit('/').next().unwrap_or(&cmd).to_string()
            };
            if !d.man_opts.contains_key(&page) {
                let mut opts = man_options(&page);
                if opts.is_empty() && page.contains('-') && page != cmd {
                    opts = man_options(&cmd);
                }
                d.man_opts.insert(page.clone(), opts);
            }
            let opts = d.man_opts.get(&page).cloned().unwrap_or_default();
            for (o, desc) in opts.into_iter().filter(|(o, _)| o.starts_with(word)) {
                let space = !o.ends_with('=');
                out.push(make(o, if desc.is_empty() { None } else { Some(desc) }, span, space, Some(Style::new().fg(rgb(CYAN)))));
            }
            if !out.is_empty() {
                return CompletionResult::fresh(out);
            }
        }
        match cmd.as_str() {
            "cd" | "pushd" | "z" | "rmdir" => {
                drop(d);
                return CompletionResult::fresh(self.files(word, span, true, &home, &cwd));
            }
            _ if !word.starts_with('-') && crate::completions::connue(&cmd) => {
                drop(d);
                let style = Some(Style::new().fg(rgb(CYAN)));
                if let Some(v) = crate::completions::candidats(&cmd, &words[1..], word, &cwd) {
                    for (val, desc) in v {
                        out.push(make(val, if desc.is_empty() { None } else { Some(desc) }, span, true, style));
                    }
                    if !out.is_empty() {
                        return CompletionResult::fresh(out);
                    }
                }
                return CompletionResult::fresh(self.files(word, span, false, &home, &cwd));
            }
            "shadow" | "shadow-modes" | "shadow-mode" | "shadow-cible" if !word.starts_with('-') => {
                let violet = Some(Style::new().fg(rgb(VIOLET_CLAIR)));
                if crate::shadow::attend_mode(&cmd, &words[1..]) {
                    for m in crate::shadow::modes_installes(&home).into_iter().filter(|m| m.starts_with(word)) {
                        out.push(make(m, Some("mode".into()), span, true, violet));
                    }
                }
                for (c, desc) in crate::shadow::sous_commandes(&cmd, &words[1..]) {
                    if c.starts_with(word) {
                        let desc = if desc.is_empty() { None } else { Some(desc.to_string()) };
                        out.push(make(c.to_string(), desc, span, true, violet));
                    }
                }
                if !out.is_empty() {
                    return CompletionResult::fresh(out);
                }
            }
            "git" if argn == 1 => {
                for (c, desc) in GIT_CMDS {
                    if c.starts_with(word) {
                        out.push(make(c.to_string(), Some(desc.to_string()), span, true, Some(Style::new().fg(rgb(BRAISE)))));
                    }
                }
                return CompletionResult::fresh(out);
            }
            "git" if matches!(words.get(1).map(|s| s.as_str()), Some("checkout" | "switch" | "merge" | "rebase" | "branch" | "diff" | "log" | "cherry-pick" | "reset" | "push" | "pull")) && !word.starts_with('-') => {
                let bs = git_branches(&cwd);
                for b in bs.into_iter().filter(|b| b.starts_with(word)) {
                    out.push(make(b, Some("branche".into()), span, true, Some(Style::new().fg(rgb(BRAISE)))));
                }
                if !out.is_empty() {
                    return CompletionResult::fresh(out);
                }
            }
            "ssh" | "scp" | "sftp" | "mosh" | "ssh-copy-id" if !word.starts_with('-') && !word.contains('/') => {
                let (user, host_pfx) = match word.split_once('@') {
                    Some((u, h)) => (format!("{u}@"), h.to_string()),
                    None => (String::new(), word.to_string()),
                };
                for h in ssh_hosts(&home).into_iter().filter(|h| h.starts_with(&host_pfx)) {
                    out.push(make(format!("{user}{h}"), Some("hôte".into()), span, cmd != "scp", Some(Style::new().fg(rgb(CYAN)))));
                }
                if cmd != "scp" || !out.is_empty() {
                    return CompletionResult::fresh(out);
                }
            }
            "make" if !word.starts_with('-') => {
                for t in make_targets(&cwd).into_iter().filter(|t| t.starts_with(word)) {
                    out.push(make(t, Some("cible".into()), span, true, Some(Style::new().fg(rgb(OR)))));
                }
                if !out.is_empty() {
                    return CompletionResult::fresh(out);
                }
            }
            "man" | "which" | "type" | "whereis" | "help" if argn >= 1 => {
                d.refresh_path_cmds();
                let mut v: Vec<String> = d.path_cmds.keys().filter(|c| c.starts_with(word)).cloned().collect();
                if cmd == "type" || cmd == "help" {
                    v.extend(crate::builtins::NAMES.iter().map(|(n, _)| n.to_string()).filter(|n| n.starts_with(word)));
                }
                v.sort();
                v.dedup();
                for c in v.into_iter().take(300) {
                    out.push(make(c, None, span, true, None));
                }
                return CompletionResult::fresh(out);
            }
            "unalias" => {
                for a in d.aliases.keys().filter(|a| a.starts_with(word)) {
                    out.push(make(a.clone(), Some("alias".into()), span, true, None));
                }
                return CompletionResult::fresh(out);
            }
            "unset" | "export" | "local" | "readonly" => {
                let mut v: Vec<&String> = d.vars.iter().filter(|a| a.starts_with(word)).collect();
                v.sort();
                for a in v {
                    out.push(make(a.clone(), Some("variable".into()), span, true, Some(Style::new().fg(rgb(OR)))));
                }
                return CompletionResult::fresh(out);
            }
            "kill" if word.starts_with('-') => {
                for n in 1..32 {
                    let s = format!("-{}", crate::shell::signal_name(n));
                    if s.starts_with(word) {
                        out.push(make(s, Some(format!("signal {n}")), span, true, None));
                    }
                }
                return CompletionResult::fresh(out);
            }
            _ => {}
        }
        drop(d);
        CompletionResult::fresh(self.files(word, span, false, &home, &cwd))
    }
}

// --------------------------------------------------------------------------
// Construction de l'éditeur
// --------------------------------------------------------------------------

pub fn history_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".local/share")
        });
    base.join("shsh/history")
}

/// Premier lancement : reprend l'historique zsh puis bash (les suggestions servent tout de suite).
fn import_history(dest: &Path, home: &str) {
    if dest.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dest.parent().unwrap());
    let mut entries: Vec<String> = Vec::new();
    if let Ok(bytes) = std::fs::read(format!("{home}/.zsh_history")) {
        let s = String::from_utf8_lossy(&bytes);
        let mut cur = String::new();
        for l in s.lines() {
            let l = if cur.is_empty() {
                match l.strip_prefix(": ") {
                    Some(r) => r.split_once(';').map(|x| x.1).unwrap_or(r),
                    None => l,
                }
            } else {
                l
            };
            if let Some(cont) = l.strip_suffix('\\') {
                cur.push_str(cont);
                cur.push('\n');
                continue;
            }
            cur.push_str(l);
            entries.push(std::mem::take(&mut cur));
        }
    }
    if let Ok(s) = std::fs::read_to_string(format!("{home}/.bash_history")) {
        entries.extend(s.lines().filter(|l| !l.starts_with('#')).map(String::from));
    }
    // dédoublonne en gardant la dernière occurrence
    let mut seen = HashSet::new();
    let mut uniq: Vec<String> = Vec::new();
    for e in entries.into_iter().rev() {
        let t = e.trim().to_string();
        if t.is_empty() || t.len() > 2000 {
            continue;
        }
        if seen.insert(t.clone()) {
            uniq.push(t);
        }
    }
    uniq.truncate(8000);
    uniq.reverse();
    let body: String = uniq.iter().map(|e| format!("{}\n", e.replace('\n', "<\\n>"))).collect();
    let _ = std::fs::write(dest, body);
}

pub fn build_editor(sh: &mut Shell, data: &Shared) -> Reedline {
    let hp = history_path();
    import_history(&hp, &sh.home());
    sh.history_path = Some(hp.clone());
    let inner = FileBackedHistory::with_file(50_000, hp).unwrap_or_else(|_| FileBackedHistory::new(5000).unwrap());
    let history: Box<dyn reedline::History> = Box::new(FilteredHistory { inner, data: data.clone() });

    let mut kb = default_emacs_keybindings();
    kb.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion".into()),
            ReedlineEvent::MenuNext,
        ]),
    );
    kb.add_binding(KeyModifiers::SHIFT, KeyCode::BackTab, ReedlineEvent::MenuPrevious);
    kb.add_binding(
        KeyModifiers::ALT,
        KeyCode::Enter,
        ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
    );
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('f'),
        ReedlineEvent::UntilFound(vec![ReedlineEvent::HistoryHintComplete, ReedlineEvent::Right]),
    );
    kb.add_binding(
        KeyModifiers::ALT,
        KeyCode::Char('f'),
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::HistoryHintWordComplete,
            ReedlineEvent::Edit(vec![EditCommand::MoveWordRight { select: false }]),
        ]),
    );
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('l'),
        ReedlineEvent::ClearScreen,
    );
    // Alt+S : ajoute « sudo » devant la ligne (ou devant la commande précédente si la ligne est vide)
    kb.add_binding(
        KeyModifiers::ALT,
        KeyCode::Char('s'),
        ReedlineEvent::Edit(vec![
            EditCommand::MoveToStart { select: false },
            EditCommand::InsertString("sudo ".into()),
            EditCommand::MoveToEnd { select: false },
        ]),
    );
    // Alt+E : retravailler la ligne dans $VISUAL / $EDITOR
    kb.add_binding(KeyModifiers::ALT, KeyCode::Char('e'), ReedlineEvent::OpenEditor);

    // Menu de complétion façon liste : valeur à gauche, description alignée à droite ;
    // sans description (fichiers), les candidats se rangent en colonnes.
    let menu = ColumnarMenu::default()
        .with_name("completion")
        .with_marker("\x1b[1;38;2;124;108;255m❯\x1b[0m ")
        .with_columns(5)
        .with_column_padding(3)
        .with_text_style(Style::new().fg(rgb(TEXTE)))
        .with_selected_text_style(Style::new().fg(Color::Rgb(236, 235, 250)).on(SELECTION).bold())
        .with_description_text_style(Style::new().fg(Color::Rgb(128, 134, 172)))
        .with_match_text_style(Style::new().fg(rgb(VIOLET_CLAIR)).bold())
        .with_selected_match_text_style(Style::new().fg(Color::Rgb(236, 235, 250)).on(SELECTION).bold());

    // Ctrl+R : l'historique en liste, filtré à mesure qu'on tape
    let historique = ColumnarMenu::default()
        .with_name("historique")
        .with_marker("\x1b[1;38;2;224;175;104m≡\x1b[0m ")
        .with_columns(1)
        .with_text_style(Style::new().fg(rgb(TEXTE)))
        .with_selected_text_style(Style::new().fg(Color::Rgb(236, 235, 250)).on(SELECTION).bold())
        .with_match_text_style(Style::new().fg(rgb(VIOLET_CLAIR)).bold())
        .with_selected_match_text_style(Style::new().fg(Color::Rgb(236, 235, 250)).on(SELECTION).bold());
    // Ctrl+R ouvre l'historique ; menu ouvert, Ctrl+R change de vue : tout → ce dossier → mission → échecs
    kb.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('r'),
        ReedlineEvent::UntilFound(vec![ReedlineEvent::Menu("historique".into()), ReedlineEvent::MenuPageNext]),
    );

    let mut ed = Reedline::create()
        .with_history(history)
        .with_history_exclusion_prefix(Some(" ".into()))
        .with_highlighter(Box::new(ShshHighlighter { data: data.clone() }))
        .with_hinter(Box::new(DefaultHinter::default().with_style(Style::new().fg(rgb(BRUME)))))
        .with_completer(Box::new(ShshCompleter { data: data.clone() }))
        .with_partial_completions(true)
        .with_quick_completions(true)
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(crate::menu::MenuFr::new(menu, 14))))
        .with_menu(ReedlineMenu::HistoryMenu(Box::new(crate::menu::MenuFr::historique(historique, 12, data.clone()))))
        .with_validator(Box::new(ShshValidator))
        .with_edit_mode(Box::new(Emacs::new(kb)))
        .with_transient_prompt(Box::new(TransientPrompt { ok: true }))
        .with_ansi_colors(true);
    let editeur = sh
        .get_var("VISUAL")
        .or_else(|| sh.get_var("EDITOR"))
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "nano".into());
    let mut mots = editeur.split_whitespace();
    if let Some(prog) = mots.next() {
        let mut c = std::process::Command::new(prog);
        c.args(mots);
        let tampon = std::env::temp_dir().join(format!("shsh-ligne-{}.sh", std::process::id()));
        ed = ed.with_buffer_editor(c, tampon);
    }
    if !sh.abbrs.is_empty() {
        ed = ed.with_abbreviations(sh.abbrs.clone());
    }
    sh.abbrs_changed = false;
    ed
}

pub fn save_history_now(_sh: &Shell) {
    // FileBackedHistory écrit à chaque entrée validée ; rien à faire ici.
}

pub fn clear_history(sh: &Shell) {
    if let Some(p) = &sh.history_path {
        let _ = std::fs::write(p, "");
    }
}

/// « Vouliez-vous dire … ? » : commande la plus proche (distance d'édition ≤ 2).
/// Commandes très courantes : à distance égale, on les propose en premier.
const COURANTES: &[&str] = &[
    "git", "ls", "cd", "cat", "grep", "make", "sudo", "apt", "docker", "python3", "ssh", "vim", "nano", "curl",
    "wget", "tar", "find", "echo", "mkdir", "rm", "cp", "mv", "man", "kill", "top", "htop", "systemctl", "cargo",
    "npm", "pip", "node", "code", "tmux", "less", "head", "tail", "chmod", "chown", "ping", "nmap", "gcc", "clear",
    "exit", "history", "which", "touch", "diff", "sed", "awk", "sort", "unzip", "zip", "df", "du", "ps", "ip",
];

/// Commande la plus proche d'un nom mal tapé, avec sa distance d'édition.
pub fn suggest_command(sh: &Shell, name: &str) -> Option<(String, usize)> {
    if name.len() < 2 {
        return None;
    }
    let mut cands: Vec<String> = crate::builtins::NAMES.iter().map(|(n, _)| n.to_string()).collect();
    cands.extend(sh.aliases.keys().cloned());
    cands.extend(sh.functions.keys().cloned());
    let path = sh.get_var("PATH").unwrap_or_default();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                cands.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    let lettres = |x: &str| {
        let mut v: Vec<char> = x.chars().collect();
        v.sort_unstable();
        v
    };
    let tri_nom = lettres(name);
    let max = if name.len() <= 3 { 1 } else { 2 };
    // (score, longueur, nom, distance) : le plus petit score gagne
    let mut best: Option<(i32, usize, String, usize)> = None;
    for c in cands {
        if (c.len() as isize - name.len() as isize).abs() > max as isize {
            continue;
        }
        let dist = damerau(name, &c);
        if dist == 0 || dist > max {
            continue;
        }
        let mut score = dist as i32 * 10;
        if lettres(&c) == tri_nom {
            score -= 4; // mêmes lettres dans le désordre : la faute la plus fréquente
        }
        if c.chars().next() == name.chars().next() {
            score -= 2;
        }
        if COURANTES.contains(&c.as_str()) {
            score -= 3;
        }
        let cle = (score, c.len());
        if best.as_ref().is_none_or(|(s, l, _, _)| cle < (*s, *l)) {
            best = Some((score, c.len(), c, dist));
        }
    }
    best.map(|b| (b.2, b.3))
}

fn damerau(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}
