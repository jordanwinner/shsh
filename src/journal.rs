//! Journal des commandes : chaque ligne tapée au prompt est notée avec son
//! heure, son dossier, son code de retour et sa durée.
//!
//! - `~/.local/share/shsh/journal.tsv` : toutes les commandes (lu par `journal`) ;
//! - `$MISSION/journal.md` : pendant une mission Shadow OS (`shadow cible …`),
//!   les mêmes lignes en Markdown, prêtes pour un rapport de test d'intrusion.
//!
//! Une commande est notée quand elle se termine : celle qui ne rend jamais la
//! main (`exec …`, terminal fermé en plein scan) n'apparaît pas.
//!
//! Ce qui est exclu de l'historique (espace en tête, `SHSH_HISTORY_IGNORE`)
//! n'est jamais écrit. `SHSH_JOURNAL=0` coupe tout.

use crate::exec::{flush_stdout, write_fd};
use crate::shell::{ExecResult, Shell};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

pub fn chemin(sh: &Shell) -> PathBuf {
    let base = sh
        .get_var("XDG_DATA_HOME")
        .filter(|s| s.starts_with('/'))
        .unwrap_or_else(|| format!("{}/.local/share", sh.home()));
    PathBuf::from(base).join("shsh/journal.tsv")
}

fn actif(sh: &Shell) -> bool {
    sh.get_var("SHSH_JOURNAL").as_deref() != Some("0")
}

/// Heure locale formatée avec strftime.
pub fn date(quand: i64, modele: &str) -> String {
    let t = quand as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let mut tampon = [0u8; 64];
    let Ok(c) = std::ffi::CString::new(modele) else { return String::new() };
    let n = unsafe { libc::strftime(tampon.as_mut_ptr() as *mut libc::c_char, tampon.len(), c.as_ptr(), &tm) };
    String::from_utf8_lossy(&tampon[..n]).into_owned()
}

pub fn maintenant() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Durée lisible à la française : « 0,4 s », « 2 min 05 s », « 1 h 12 min ».
pub fn duree(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    // 59,96 s s'arrondirait en « 60,0 s » : on passe aux minutes avant
    if ms < 59_950 {
        format!("{s:.1} s").replace('.', ",")
    } else if s < 3600.0 {
        let sec = (ms + 500) / 1000;
        format!("{} min {:02} s", sec / 60, sec % 60)
    } else {
        format!("{} h {:02} min", ms / 3_600_000, (ms / 60_000) % 60)
    }
}

fn echapper(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn desechapper(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut c = s.chars();
    while let Some(x) = c.next() {
        if x == '\\' {
            match c.next() {
                Some('t') => o.push('\t'),
                Some('n') => o.push('\n'),
                Some(y) => o.push(y),
                None => o.push('\\'),
            }
        } else {
            o.push(x);
        }
    }
    o
}

/// Code Markdown en ligne qui résiste aux accents graves dans la commande.
fn code_md(s: &str) -> String {
    let une = s.replace('\n', " ⏎ ");
    let mut n = 1;
    while une.contains(&"`".repeat(n)) {
        n += 1;
    }
    let f = "`".repeat(n);
    let pad = if une.starts_with('`') || une.ends_with('`') { " " } else { "" };
    format!("{f}{pad}{une}{pad}{f}")
}

/// Note une commande exécutée au prompt. `mission` = dossier de la mission en cours.
pub fn noter(sh: &Shell, ligne: &str, code: i32, d: Duration, cwd: &str, mission: Option<(&str, &str, &str)>) {
    if !actif(sh) || ligne.trim().is_empty() {
        return;
    }
    let quand = maintenant();
    let ms = d.as_millis() as u64;
    let p = chemin(sh);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{quand}\t{ms}\t{code}\t{}\t{}", echapper(cwd), echapper(ligne.trim_end()));
    }
    let Some((dossier, adresse, nom)) = mission else { return };
    if dossier.is_empty() || !std::path::Path::new(dossier).is_dir() {
        return;
    }
    let jp = PathBuf::from(dossier).join("journal.md");
    let neuf = !jp.exists();
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&jp) else { return };
    if neuf {
        let titre = if nom.is_empty() { adresse.to_string() } else { format!("{nom} ({adresse})") };
        let _ = writeln!(
            f,
            "# Journal de mission — {titre}\n\nÉcrit automatiquement par shsh : chaque commande tapée pendant la mission.\n"
        );
    }
    let marque = if code == 0 { "✔".to_string() } else { format!("✘ {code}") };
    // « preuve … » : lien vers la sortie gardée dans $MISSION/preuves/
    // (où que « preuve » soit dans la ligne : « cd x && preuve … », « preuve a; preuve b »)
    let preuve: String = sh
        .get_var(crate::preuve::VAR_DERNIERE)
        .unwrap_or_default()
        .split_whitespace()
        .map(|p| format!(" · preuve : [{p}]({p})"))
        .collect();
    let _ = writeln!(
        f,
        "- {} · {marque} · {} · {} — {}{preuve}",
        date(quand, "%Y-%m-%d %H:%M:%S"),
        duree(ms),
        code_md(&crate::builtins::tilde_path(sh, cwd)),
        code_md(ligne.trim())
    );
}

struct Entree {
    quand: i64,
    ms: u64,
    code: i32,
    cwd: String,
    ligne: String,
}

fn lire(sh: &Shell) -> Vec<Entree> {
    let Ok(s) = std::fs::read_to_string(chemin(sh)) else { return Vec::new() };
    s.lines()
        .filter_map(|l| {
            let mut c = l.splitn(5, '\t');
            Some(Entree {
                quand: c.next()?.parse().ok()?,
                ms: c.next()?.parse().ok()?,
                code: c.next()?.parse().ok()?,
                cwd: desechapper(c.next()?),
                ligne: desechapper(c.next()?),
            })
        })
        .collect()
}

const AIDE: &str = "journal — les commandes tapées, avec heure, dossier, code de retour et durée

  journal                  les 20 dernières
  journal ici              celles lancées dans ce dossier
  journal echecs           celles qui ont échoué
  journal lentes           les plus longues
  journal cherche MOTIF    celles qui contiennent MOTIF
  journal aujourdhui       celles d'aujourd'hui
  journal mission          le journal de la mission Shadow en cours ($MISSION/journal.md)
  journal -n N …           en montrer N (défaut 20)
  journal chemin           où le journal est rangé

SHSH_JOURNAL=0 coupe le journal ; une ligne qui commence par une espace ou
qui correspond à SHSH_HISTORY_IGNORE n'y est jamais écrite.
";

pub fn b_journal(sh: &mut Shell, a: &[String]) -> ExecResult {
    let mut n = 20usize;
    let mut args: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < a.len() {
        match a[i].as_str() {
            "-n" => {
                i += 1;
                match a.get(i).and_then(|x| x.parse().ok()) {
                    Some(v) => n = v,
                    None => {
                        sh.error("journal : -n attend un nombre");
                        return Ok(2);
                    }
                }
            }
            "-h" | "--help" | "aide" => {
                sortir(AIDE);
                return Ok(0);
            }
            x => args.push(x),
        }
        i += 1;
    }
    match args.first().copied() {
        Some("chemin") => {
            sortir(&format!("{}\n", chemin(sh).display()));
            return Ok(0);
        }
        Some("mission") => {
            let Some(m) = sh.get_var("MISSION").filter(|m| !m.is_empty()) else {
                sh.error("journal : aucune mission en cours (shadow cible ADRESSE NOM)");
                return Ok(1);
            };
            let p = PathBuf::from(&m).join("journal.md");
            return match std::fs::read_to_string(&p) {
                Ok(s) => {
                    let lignes: Vec<&str> = s.lines().collect();
                    let debut = lignes.len().saturating_sub(n);
                    sortir(&format!("{}\n", lignes[debut..].join("\n")));
                    Ok(0)
                }
                Err(_) => {
                    sh.error(format!("journal : rien encore dans {}", p.display()));
                    Ok(1)
                }
            };
        }
        _ => {}
    }
    let tout = lire(sh);
    let cwd = sh.cwd();
    let jour = date(maintenant(), "%Y-%m-%d");
    let mut choisis: Vec<&Entree> = match args.first().copied() {
        None => tout.iter().collect(),
        Some("ici") => tout.iter().filter(|e| e.cwd == cwd).collect(),
        Some("echecs" | "échecs") => tout.iter().filter(|e| e.code != 0 && e.code != 130).collect(),
        Some("lentes" | "lents") => {
            let mut v: Vec<&Entree> = tout.iter().collect();
            v.sort_by_key(|e| e.ms);
            v
        }
        Some("aujourdhui" | "aujourd'hui") => tout.iter().filter(|e| date(e.quand, "%Y-%m-%d") == jour).collect(),
        Some("cherche") => {
            let Some(m) = args.get(1) else {
                sh.error("journal : cherche MOTIF");
                return Ok(2);
            };
            let m = m.to_lowercase();
            tout.iter().filter(|e| e.ligne.to_lowercase().contains(&m)).collect()
        }
        Some(x) => {
            sh.error(format!("journal : « {x} » inconnu (journal aide)"));
            return Ok(2);
        }
    };
    let debut = choisis.len().saturating_sub(n);
    choisis.drain(..debut);
    let couleur = sh.interactive && unsafe { libc::isatty(1) } == 1;
    let (gris, vert, rose, violet, fin) = if couleur {
        ("\x1b[38;2;110;106;150m", "\x1b[38;2;120;220;160m", "\x1b[38;2;255;77;141m", "\x1b[38;2;124;108;255m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    let mut o = String::new();
    for e in choisis {
        let quand = if date(e.quand, "%Y-%m-%d") == jour {
            date(e.quand, "%H:%M:%S")
        } else {
            date(e.quand, "%d/%m %H:%M")
        };
        let code = if e.code == 0 {
            format!("{vert}✔ {:<3}{fin}", 0)
        } else {
            format!("{rose}✘ {:<3}{fin}", e.code)
        };
        o.push_str(&format!(
            "{gris}{quand:>11}{fin}  {code}  {gris}{:>10}{fin}  {violet}{}{fin}  {}\n",
            duree(e.ms),
            crate::builtins::tilde_path(sh, &e.cwd),
            e.ligne.replace('\n', " ⏎ ")
        ));
    }
    sortir(&o);
    Ok(0)
}

fn sortir(s: &str) {
    flush_stdout();
    write_fd(1, s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour_echappement() {
        let s = "a\tb\\c\nd";
        assert_eq!(desechapper(&echapper(s)), s);
    }

    #[test]
    fn markdown_accents_graves() {
        assert_eq!(code_md("ls"), "`ls`");
        assert_eq!(code_md("echo `id`"), "`` echo `id` ``");
    }

    #[test]
    fn durees() {
        assert_eq!(duree(400), "0,4 s");
        assert_eq!(duree(59_960), "1 min 00 s");
        assert_eq!(duree(125_000), "2 min 05 s");
        assert_eq!(duree(4_320_000), "1 h 12 min");
    }
}
