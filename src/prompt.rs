//! Prompt « Éclipse » : deux lignes, informations contextuelles, invite transitoire.

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Palette Éclipse (identité Shadow OS)
pub const VIOLET: (u8, u8, u8) = (124, 108, 255);
pub const VIOLET_CLAIR: (u8, u8, u8) = (169, 155, 255);
pub const BRAISE: (u8, u8, u8) = (255, 77, 141);
pub const BRUME: (u8, u8, u8) = (98, 104, 140);
pub const TEXTE: (u8, u8, u8) = (192, 202, 245);
pub const OR: (u8, u8, u8) = (224, 175, 104);
pub const VERT: (u8, u8, u8) = (158, 206, 106);
pub const CYAN: (u8, u8, u8) = (125, 207, 255);

pub fn fg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

pub struct PromptInfo {
    pub cwd: String,
    pub home: String,
    pub status: i32,
    pub duration: Option<Duration>,
    pub jobs: usize,
    pub user: String,
    pub host: String,
    pub ssh: bool,
    pub root: bool,
    pub venv: Option<String>,
    pub shlvl: i32,
    /// Mode du bureau Shadow, s'il n'est pas le mode habituel.
    pub mode: Option<String>,
    /// Mission en cours : (adresse, nom).
    pub cible: Option<(String, String)>,
    /// Ctrl+C au prompt : le code (130) est montré sans alarme
    pub discret: bool,
}

pub struct ShshPrompt {
    left: String,
    right: String,
    ok: bool,
    root: bool,
}

pub struct TransientPrompt {
    pub ok: bool,
}

/// Racine git et branche (lecture directe de .git/HEAD : instantané).
pub fn git_info(cwd: &str) -> Option<(PathBuf, String)> {
    let mut dir = Path::new(cwd);
    loop {
        let g = dir.join(".git");
        if g.exists() {
            let gitdir = if g.is_file() {
                // worktree / sous-module : "gitdir: chemin"
                let s = std::fs::read_to_string(&g).ok()?;
                let p = s.trim().strip_prefix("gitdir:")?.trim().to_string();
                let p = PathBuf::from(p);
                if p.is_absolute() {
                    p
                } else {
                    dir.join(p)
                }
            } else {
                g
            };
            let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
            let head = head.trim();
            let branch = match head.strip_prefix("ref: refs/heads/") {
                Some(b) => b.to_string(),
                None => format!("@{}", &head[..head.len().min(7)]),
            };
            let state = if gitdir.join("rebase-merge").exists() || gitdir.join("rebase-apply").exists() {
                "|rebase"
            } else if gitdir.join("MERGE_HEAD").exists() {
                "|merge"
            } else if gitdir.join("CHERRY_PICK_HEAD").exists() {
                "|cherry-pick"
            } else {
                ""
            };
            return Some((dir.to_path_buf(), format!("{branch}{state}")));
        }
        dir = dir.parent()?;
    }
}

/// État de l'arbre de travail, borné dans le temps (pas de prompt lent).
fn git_dirty(root: &Path) -> Option<GitEtat> {
    let mut child = Command::new("git")
        .args(["--no-optional-locks", "status", "--porcelain=v1", "-b", "--ignore-submodules=dirty", "-unormal"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if t0.elapsed() > Duration::from_millis(250) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut staged = false;
    let mut changed = false;
    let (mut avance, mut retard) = (0u32, 0u32);
    for l in s.lines() {
        let b = l.as_bytes();
        if b.len() < 2 {
            continue;
        }
        // « ## main...origin/main [ahead 1, behind 2] »
        if let Some(entete) = l.strip_prefix("## ") {
            if let Some(i) = entete.rfind('[') {
                for morceau in entete[i + 1..].trim_end_matches(']').split(", ") {
                    if let Some(n) = morceau.strip_prefix("ahead ") {
                        avance = n.parse().unwrap_or(0);
                    } else if let Some(n) = morceau.strip_prefix("behind ") {
                        retard = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if b[0] == b'?' {
            changed = true;
            continue;
        }
        if b[0] != b' ' {
            staged = true;
        }
        if b[1] != b' ' {
            changed = true;
        }
    }
    Some(GitEtat { staged, changed, avance, retard })
}

/// État du dépôt affiché dans le prompt.
struct GitEtat {
    staged: bool,
    changed: bool,
    /// Commits locaux pas encore poussés / commits distants pas encore récupérés.
    avance: u32,
    retard: u32,
}

/// Chemin affiché dans un dépôt git : à partir du dossier du dépôt
/// (« shsh/src » plutôt que « ~/Dev/shsh/src »).
fn chemin_depot(cwd: &str, racine: &Path) -> Option<(String, String)> {
    let racine_s = racine.to_str()?;
    let nom = racine.file_name()?.to_str()?;
    let reste = cwd.strip_prefix(racine_s)?.trim_start_matches('/');
    if reste.is_empty() {
        return Some((String::new(), nom.to_string()));
    }
    let comps: Vec<&str> = reste.split('/').collect();
    let (dernier, parents) = comps.split_last()?;
    let milieu = if parents.len() > 3 {
        format!("…/{}/", parents[parents.len() - 2..].join("/"))
    } else if parents.is_empty() {
        String::new()
    } else {
        format!("{}/", parents.join("/"))
    };
    Some((format!("{nom}/{milieu}"), dernier.to_string()))
}

pub fn short_path(cwd: &str, home: &str) -> (String, String) {
    let p = if home.len() > 1 && (cwd == home || cwd.starts_with(&format!("{home}/"))) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd.to_string()
    };
    // Raccourcit les dossiers intermédiaires si le chemin est long
    let comps: Vec<&str> = p.split('/').collect();
    let (head, last) = match p.rfind('/') {
        Some(i) if p.len() > 1 => (&p[..=i], &p[i + 1..]),
        _ => ("", p.as_str()),
    };
    if comps.len() > 5 {
        let keep = &comps[comps.len() - 3..comps.len() - 1];
        let first = comps[0];
        let head = format!("{}/…/{}/", first, keep.join("/"));
        return (head, last.to_string());
    }
    (head.to_string(), last.to_string())
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 {
        format!("{s:.1}s")
    } else if s < 3600.0 {
        format!("{}m{:02}s", s as u64 / 60, s as u64 % 60)
    } else {
        format!("{}h{:02}m", s as u64 / 3600, (s as u64 % 3600) / 60)
    }
}

fn clock() -> String {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

impl ShshPrompt {
    pub fn build(info: &PromptInfo) -> Self {
        let v = fg(VIOLET);
        let vc = fg(VIOLET_CLAIR);
        let b = fg(BRAISE);
        let m = fg(BRUME);
        let mut left = String::new();

        if info.ssh || info.root {
            let col = if info.root { fg(BRAISE) } else { fg(OR) };
            left.push_str(&format!("{col}{BOLD}{}{RESET}{m}@{RESET}{col}{}{RESET} ", info.user, info.host));
        }
        // Dossier : parents en violet, dossier courant en gras clair
        let git = git_info(&info.cwd);
        let (head, last) = git
            .as_ref()
            .filter(|(racine, _)| racine.to_str() != Some(info.home.as_str()))
            .and_then(|(racine, _)| chemin_depot(&info.cwd, racine))
            .unwrap_or_else(|| short_path(&info.cwd, &info.home));
        let inscriptible = std::ffi::CString::new(info.cwd.as_str())
            .map(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
            .unwrap_or(true);
        let icon = if !inscriptible {
            "\u{f023}" // cadenas : on ne peut pas écrire ici
        } else if info.cwd == info.home {
            "\u{f015}" // maison
        } else {
            "\u{f07c}" // dossier ouvert
        };
        left.push_str(&format!("{v}{icon} {RESET}{v}{head}{RESET}{vc}{BOLD}{last}{RESET}"));

        // Git
        if let Some((root, branch)) = git {
            left.push_str(&format!(" {m}on{RESET} {b}\u{e725} {branch}{RESET}"));
            if let Some(e) = git_dirty(&root) {
                let mut marks = String::new();
                if e.staged {
                    marks.push_str(&format!("{}+", fg(VERT)));
                }
                if e.changed {
                    marks.push_str(&format!("{}●", fg(OR)));
                }
                if e.avance > 0 {
                    marks.push_str(&format!("{}{}⇡{}", if marks.is_empty() { "" } else { " " }, fg(CYAN), e.avance));
                }
                if e.retard > 0 {
                    marks.push_str(&format!("{}{}⇣{}", if marks.is_empty() { "" } else { " " }, fg(CYAN), e.retard));
                }
                if !marks.is_empty() {
                    left.push_str(&format!(" {marks}{RESET}"));
                }
            }
        }
        if let Some(venv) = &info.venv {
            left.push_str(&format!(" {m}via{RESET} {}\u{e73c} {venv}{RESET}", fg(OR)));
        }
        // Mission Shadow : la cible reste sous les yeux
        if let Some((adresse, nom)) = &info.cible {
            left.push_str(&format!(" {m}→{RESET} {b}{BOLD}\u{f05b} {adresse}{RESET}"));
            if !nom.is_empty() {
                left.push_str(&format!(" {m}{nom}{RESET}"));
            }
        }
        if info.jobs > 0 {
            left.push_str(&format!(" {}\u{f013} {}{RESET}", fg(CYAN), info.jobs));
        }

        // Droite : code de retour, durée, heure
        let mut right = String::new();
        if let Some(mode) = &info.mode {
            let (icone, couleur) = match mode.as_str() {
                "pentest" => ("\u{f188}", BRAISE),
                "focus" => ("\u{f06e}", CYAN),
                "jeu" => ("\u{f11b}", VERT),
                "nuit" => ("\u{f186}", OR),
                _ => ("\u{f111}", VIOLET_CLAIR),
            };
            let mut nom = mode.clone();
            if let Some(c) = nom.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            right.push_str(&format!("{}{icone} {nom}{RESET}  ", fg(couleur)));
        }
        // Code de retour de la dernière commande : toujours affiché
        if info.status == 0 {
            right.push_str(&format!("{}✔ 0{RESET}  ", fg(VERT)));
        } else {
            // 130 → « 130 INT » : le nombre exact, plus le nom du signal quand c'en est un
            let signal = match info.status {
                s if s > 128 && s < 193 => format!(" {}", crate::shell::signal_name(s - 128)),
                _ => String::new(),
            };
            if info.discret {
                right.push_str(&format!("{m}{}{signal}{RESET}  ", info.status));
            } else {
                right.push_str(&format!("{b}{BOLD}✘ {}{RESET}{b}{signal}{RESET}  ", info.status));
            }
        }
        if let Some(d) = info.duration {
            if d >= Duration::from_secs(2) {
                right.push_str(&format!("{}\u{f252} {}{RESET}  ", fg(OR), fmt_duration(d)));
            }
        }
        right.push_str(&format!("{m}{}{RESET}", clock()));

        ShshPrompt {
            left: format!("{left}\n"),
            right,
            ok: info.status == 0 || info.discret,
            root: info.root,
        }
    }
}

impl Prompt for ShshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.right)
    }
    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        let col = if self.ok { fg(VIOLET) } else { fg(BRAISE) };
        let sym = match mode {
            PromptEditMode::Vi(reedline::PromptViMode::Normal) => "❮",
            _ if self.root => "#",
            _ => "❯",
        };
        Cow::Owned(format!("{col}{BOLD}{sym}{RESET} "))
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{}·{RESET} ", fg(BRUME)))
    }
    fn render_prompt_history_search_indicator(&self, hs: PromptHistorySearch) -> Cow<'_, str> {
        let (col, prefix) = match hs.status {
            PromptHistorySearchStatus::Passing => (fg(VIOLET), ""),
            PromptHistorySearchStatus::Failing => (fg(BRAISE), "aucun résultat · "),
        };
        Cow::Owned(format!("{col}\u{f002} {prefix}{}{RESET} {}❯{RESET} ", hs.term, fg(BRUME)))
    }
    fn right_prompt_on_last_line(&self) -> bool {
        false
    }
    fn get_indicator_color(&self) -> reedline::Color {
        reedline::Color::Rgb(VIOLET.0, VIOLET.1, VIOLET.2)
    }
    fn get_prompt_color(&self) -> reedline::Color {
        reedline::Color::Rgb(VIOLET.0, VIOLET.1, VIOLET.2)
    }
}

impl Prompt for TransientPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        let col = if self.ok { fg(VIOLET) } else { fg(BRAISE) };
        Cow::Owned(format!("{col}❯{RESET} "))
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{}·{RESET} ", fg(BRUME)))
    }
    fn render_prompt_history_search_indicator(&self, _hs: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Owned(format!("{}❯{RESET} ", fg(VIOLET)))
    }
}
