//! `preuve COMMANDE` : exécute la commande normalement (couleurs, interactivité)
//! et en garde la sortie, nettoyée, dans `$MISSION/preuves/`, avec date, cible,
//! code de retour et durée. Le journal de mission y renvoie et `shadow rapport`
//! l'insère sous la commande.
//!
//!   preuve nmap -sV $CIBLE
//!   preuve 'curl -s http://$CIBLE/ | grep -i version'   (tube : la ligne entière entre guillemets)
//!
//! La commande tourne dans un shsh fils (`shsh -ic`, alias et fonctions de
//! ~/.shshrc compris) à travers `script`, qui lui donne un vrai terminal.

use crate::exec::quote_if_needed;
use crate::shell::{ExecResult, Shell};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

pub const VAR_DERNIERE: &str = "SHSH_DERNIERE_PREUVE";

const AIDE: &str = "preuve COMMANDE… — exécute la commande et garde sa sortie dans $MISSION/preuves/
  preuve nmap -sV $CIBLE
  preuve 'curl -s http://$CIBLE/ | grep -i version'   (tube : la ligne entière entre guillemets)
Le journal de mission y renvoie ; « shadow rapport » insère la sortie sous la commande.
Seules les variables exportées ($CIBLE, $MISSION…) passent à la commande.";

/// `script` écrit dans son fichier, même avec -q, une ligne d'ouverture et une de fermeture
/// (« Script started … [COMMAND=…] », traduites selon la langue) : elles contiennent le chemin
/// complet du shell, donc le nom de l'utilisateur. On les retire.
pub fn sans_entete_script(brut: &str) -> String {
    brut.split_inclusive('\n')
        .filter(|l| {
            let l = l.trim_start();
            !(l.starts_with("Script ") && (l.contains("[COMMAND=") || l.contains("[COMMAND_EXIT_CODE=")))
        })
        .collect()
}

/// Retire les séquences d'échappement du terminal et rejoue les retours chariot / effacements.
pub fn nettoyer(brut: &str) -> String {
    let re = regex::Regex::new(
        r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[PX^_][^\x1b]*\x1b\\|\x1b[()][0-9A-Za-z]|\x1b[=>78DEHMNOcZ]",
    )
    .expect("regex");
    let sans = re.replace_all(brut, "");
    let mut sortie = String::with_capacity(sans.len());
    for ligne in sans.split('\n') {
        // un retour chariot réécrit la ligne (barres de progression) : on garde la dernière version
        let mut courante: Vec<char> = Vec::new();
        for segment in ligne.split('\r') {
            let seg: Vec<char> = segment.chars().collect();
            for (i, c) in seg.iter().enumerate() {
                if i < courante.len() {
                    courante[i] = *c;
                } else {
                    courante.push(*c);
                }
            }
        }
        // retour arrière : efface le caractère précédent
        let mut finale: Vec<char> = Vec::new();
        for c in courante {
            match c {
                '\u{8}' => {
                    finale.pop();
                }
                c if c.is_control() && c != '\t' => {}
                c => finale.push(c),
            }
        }
        let l: String = finale.into_iter().collect();
        sortie.push_str(l.trim_end());
        sortie.push('\n');
    }
    // pas de lignes vides en fin de fichier
    let t = sortie.trim_end_matches('\n');
    format!("{t}\n")
}

fn slug(ligne: &str) -> String {
    let mut s: String = ligne
        .split_whitespace()
        .filter(|m| !m.starts_with('$') && !m.contains('/') && m.chars().any(|c| c.is_ascii_alphanumeric()))
        .take(3)
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').chars().take(32).collect::<String>();
    if s.is_empty() { "commande".into() } else { s }
}

pub fn b_preuve(sh: &mut Shell, a: &[String]) -> ExecResult {
    if a.len() < 2 || matches!(a[1].as_str(), "-h" | "--help" | "aide") {
        crate::exec::flush_stdout();
        crate::exec::write_fd(if a.len() < 2 { 2 } else { 1 }, &format!("{AIDE}\n"));
        return Ok(if a.len() < 2 { 2 } else { 0 });
    }
    let Some(mission) = sh.get_var("MISSION").filter(|m| !m.is_empty() && std::path::Path::new(m).is_dir()) else {
        sh.error("preuve : aucune mission en cours (shadow cible ADRESSE NOM)");
        return Ok(1);
    };
    // un seul argument = une ligne de commande complète (tubes, redirections) ; sinon on requote
    let ligne = if a.len() == 2 { a[1].clone() } else { a[1..].iter().map(|x| quote_if_needed(x)).collect::<Vec<_>>().join(" ") };
    let dossier = PathBuf::from(&mission).join("preuves");
    if let Err(e) = std::fs::create_dir_all(&dossier) {
        sh.error(format!("preuve : {}: {e}", dossier.display()));
        return Ok(1);
    }
    let quand = crate::journal::maintenant();
    let nom = format!("{}-{}.txt", crate::journal::date(quand, "%Y%m%d-%H%M%S"), slug(&ligne));
    let fichier = dossier.join(&nom);
    let brut = dossier.join(format!(".{nom}.brut"));
    let shsh = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "shsh".into());
    let interieur = format!("{} -ic {}", quote_if_needed(&shsh), quote_if_needed(&ligne));
    crate::exec::flush_stdout();
    let t0 = Instant::now();
    let statut = Command::new("script")
        .args(["-q", "-e", "-f", "-c", &interieur])
        .arg(&brut)
        .env("SHELL", "/bin/sh")
        .status();
    let duree = t0.elapsed();
    let code = match statut {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => {
            sh.error("preuve : la commande « script » (util-linux) est introuvable");
            return Ok(127);
        }
    };
    let sortie = std::fs::read(&brut).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let _ = std::fs::remove_file(&brut);
    let corps = nettoyer(&sans_entete_script(&sortie));
    let cible = sh.get_var("CIBLE").unwrap_or_default();
    let entete = format!(
        "# Preuve — {ligne}\n\nDate : {} · Dossier : {} · Cible : {}\nCode de retour : {code} · Durée : {}\n\n---\n\n",
        crate::journal::date(quand, "%Y-%m-%d %H:%M:%S"),
        crate::builtins::tilde_path(sh, &sh.cwd()),
        if cible.is_empty() { "—" } else { &cible },
        crate::journal::duree(duree.as_millis() as u64),
    );
    if let Err(e) = std::fs::write(&fichier, format!("{entete}{corps}")) {
        sh.error(format!("preuve : {}: {e}", fichier.display()));
        return Ok(code);
    }
    let _ = sh.set_var(VAR_DERNIERE, format!("preuves/{nom}"));
    eprintln!(
        "  \x1b[38;2;124;108;255m◐\x1b[0m \x1b[38;2;156;152;191mpreuve gardée : {}\x1b[0m",
        crate::builtins::tilde_path(sh, &fichier.to_string_lossy())
    );
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nettoyage() {
        assert_eq!(nettoyer("\x1b[1;32mOK\x1b[0m\r\n"), "OK\n");
        assert_eq!(nettoyer("10%\r50%\r100%\n"), "100%\n");
        assert_eq!(nettoyer("abc\u{8}\u{8}X\n\n\n"), "aX\n");
        assert_eq!(nettoyer("\x1b]0;titre\x07ligne\n"), "ligne\n");
    }

    #[test]
    fn entete_script_retire() {
        let b = "Script démarré sur 2026 [COMMAND=\"/home/alice/shsh -ic x\" TERM=\"x\"]\nsortie\n\nScript terminé sur 2026 [COMMAND_EXIT_CODE=\"0\"]\n";
        assert_eq!(nettoyer(&sans_entete_script(b)), "sortie\n");
        assert_eq!(sans_entete_script("Script kiddie\n"), "Script kiddie\n");
    }

    #[test]
    fn noms() {
        assert_eq!(slug("nmap -sV $CIBLE -oA scans/x"), "nmap-sv-oa");
        assert_eq!(slug("curl -s http://x/ | grep"), "curl-s-grep");
        assert_eq!(slug("$X"), "commande");
    }
}
