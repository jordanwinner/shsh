//! Intégration native à Shadow OS : le shell connaît le mode du bureau et la
//! cible de la mission en cours (fichiers d'état écrits par `shadow modes` et
//! `shadow cible`), sans rien ajouter aux fichiers de configuration.
//!
//! `SHSH_SHADOW=0` coupe tout (segment du prompt et variables).

use crate::shell::Shell;
use std::path::PathBuf;

pub struct Cible {
    pub adresse: String,
    pub nom: String,
    pub dossier: String,
}

#[derive(Default)]
pub struct Etat {
    /// Mode du bureau, seulement s'il n'est pas le mode habituel.
    pub mode: Option<String>,
    pub cible: Option<Cible>,
}

/// Variable témoin : ce que shsh a lui-même posé dans `$CIBLE`, pour ne
/// jamais effacer une valeur choisie à la main par l'utilisateur.
const TEMOIN: &str = "SHSH_CIBLE_POSEE";

fn dossier_etat(sh: &Shell) -> PathBuf {
    let base = sh
        .get_var("XDG_STATE_HOME")
        .filter(|s| s.starts_with('/'))
        .unwrap_or_else(|| format!("{}/.local/state", sh.home()));
    PathBuf::from(base).join("shadow-os")
}

/// Valeur d'un champ texte dans un petit objet JSON plat.
pub fn champ(json: &str, cle: &str) -> Option<String> {
    let motif = format!("\"{cle}\"");
    let reste = &json[json.find(&motif)? + motif.len()..];
    let reste = reste.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut sortie = String::new();
    let mut car = reste.chars();
    while let Some(c) = car.next() {
        match c {
            '"' => return Some(sortie),
            '\\' => match car.next()? {
                'n' => sortie.push('\n'),
                't' => sortie.push('\t'),
                'u' => {
                    let hex: String = car.by_ref().take(4).collect();
                    sortie.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)?);
                }
                autre => sortie.push(autre),
            },
            c => sortie.push(c),
        }
    }
    None
}

/// Ne garde que ce qui peut s'afficher sans risque dans un prompt.
fn propre(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

pub fn actif(sh: &Shell) -> bool {
    sh.get_var("SHSH_SHADOW").as_deref() != Some("0")
}

pub fn lire(sh: &Shell) -> Etat {
    if !actif(sh) {
        return Etat::default();
    }
    let dir = dossier_etat(sh);
    let mode = std::fs::read_to_string(dir.join("modes/actuel"))
        .ok()
        .map(|s| propre(s.trim(), 24))
        .filter(|s| !s.is_empty() && s != "quotidien");
    let cible = std::fs::read_to_string(dir.join("cible")).ok().and_then(|j| {
        let adresse = propre(&champ(&j, "cible")?, 64);
        if adresse.is_empty() {
            return None;
        }
        Some(Cible {
            adresse,
            nom: propre(&champ(&j, "nom").unwrap_or_default(), 32),
            dossier: champ(&j, "dossier").unwrap_or_default(),
        })
    });
    Etat { mode, cible }
}

/// Aligne `$CIBLE`, `$TARGET` et `$MISSION` sur la mission en cours.
pub fn synchroniser(sh: &mut Shell, etat: &Etat) {
    if !actif(sh) {
        return;
    }
    let pose = sh.get_var(TEMOIN);
    match &etat.cible {
        Some(c) => {
            let courant = sh.get_var("CIBLE");
            // Une valeur posée à la main (différente du témoin) est respectée.
            if courant.is_some() && courant != pose {
                return;
            }
            if courant.as_deref() != Some(c.adresse.as_str()) {
                sh.set_exported("CIBLE", c.adresse.clone());
                sh.set_exported("TARGET", c.adresse.clone());
                sh.set_exported(TEMOIN, c.adresse.clone());
            }
            if !c.dossier.is_empty() && sh.get_var("MISSION").as_deref() != Some(c.dossier.as_str()) {
                sh.set_exported("MISSION", c.dossier.clone());
            }
        }
        None => {
            if let Some(p) = pose {
                for v in ["CIBLE", "TARGET"] {
                    if sh.get_var(v).as_deref() == Some(p.as_str()) {
                        sh.vars.remove(v);
                    }
                }
                sh.vars.remove("MISSION");
                sh.vars.remove(TEMOIN);
            }
        }
    }
}

/// Identifiants des modes installés (pour la complétion).
pub fn modes_installes(home: &str) -> Vec<String> {
    let lien = PathBuf::from(format!("{home}/.local/bin/shadow-modes"));
    let Ok(reel) = std::fs::canonicalize(&lien) else { return Vec::new() };
    let Some(dir) = reel.parent().map(|p| p.join("definitions")) else { return Vec::new() };
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from))
        .collect();
    v.sort();
    v
}

/// Sous-commandes de `shadow` et de ses outils, avec leur description.
pub fn sous_commandes(cmd: &str, avant: &[String]) -> &'static [(&'static str, &'static str)] {
    let sous = avant.first().map(|s| s.as_str());
    match (cmd, sous) {
        ("shadow", None) => &[
            ("modes", "ambiances du bureau : pentest, focus, jeu, nuit"),
            ("mode", "fenêtres flottantes ou en tuiles"),
            ("energie", "mode d'énergie du portable"),
            ("cible", "cible de la mission en cours"),
            ("arsenal", "arsenal Kali isolé"),
            ("theme", "thème Shadow"),
            ("wall", "fonds d'écran"),
            ("keycast", "touches à l'écran dans l'îlot"),
            ("todo", "to-do de l'îlot"),
            ("keys", "raccourcis clavier"),
            ("dub", "traduction du son en direct"),
            ("fetch", "infos système"),
            ("status", "état de tout"),
            ("maj", "met tout Shadow à jour : système, apps, arsenal, shsh"),
            ("docteur", "vérifie que Shadow est entier et le répare"),
            ("rapport", "rapport de la mission en cours (Markdown)"),
        ],
        ("shadow", Some("modes")) if avant.len() == 1 => MODES,
        ("shadow-modes", None) => MODES,
        ("shadow", Some("mode")) if avant.len() == 1 => FENETRES,
        ("shadow-mode", None) => FENETRES,
        ("shadow", Some("energie")) if avant.len() == 1 => &[
            ("frais", "silencieux, sans turbo"),
            ("equilibre", "le compromis"),
            ("performance", "toute la puissance"),
            ("etat", "profil en cours"),
        ],
        ("shadow", Some("cible")) if avant.len() == 1 => CIBLE,
        ("shadow-cible", None) => CIBLE,
        ("shadow", Some("theme")) if avant.len() == 1 => {
            &[("apply", "appliquer"), ("revert", "revenir en arrière"), ("status", "état")]
        }
        ("shadow", Some("wall")) if avant.len() == 1 => {
            &[("next", "fond suivant"), ("dl", "télécharger des fonds"), ("slideshow", "diaporama")]
        }
        ("shadow", Some("docteur")) if avant.len() == 1 => &[
            ("reparer", "applique les réparations proposées"),
            ("photo", "retient les extensions actives comme référence"),
            ("minuteur", "passage automatique toutes les 15 min"),
            ("json", "diagnostic en JSON"),
        ],
        ("shadow", Some("maj")) if avant.len() == 1 => &[
            ("verifier", "ce qui attend, sans rien changer"),
            ("--sans", "sauter des étapes"),
            ("--seulement", "seulement certaines étapes"),
        ],
        ("shadow", Some("keycast")) if avant.len() == 1 => &[("on", ""), ("off", ""), ("toggle", "")],
        ("shadow", Some("todo")) if avant.len() == 1 => &[
            ("list", "voir la liste"),
            ("add", "ajouter une tâche"),
            ("done", "cocher"),
            ("rm", "retirer"),
            ("clear", "tout vider"),
        ],
        ("shadow", Some("keys")) if avant.len() == 1 => &[("install", ""), ("remove", "")],
        ("shadow", Some("arsenal")) if avant.len() == 1 => &[
            ("setup", "créer l'arsenal"),
            ("packs", "lots d'outils"),
            ("install", "installer un lot"),
            ("update", "mettre à jour"),
            ("enter", "entrer dans l'arsenal"),
            ("export", "exporter un outil"),
            ("status", "état"),
        ],
        _ => &[],
    }
}

const MODES: &[(&str, &str)] = &[
    ("liste", "modes disponibles"),
    ("activer", "passer dans un mode"),
    ("quotidien", "revenir au bureau habituel"),
    ("etat", "mode en cours"),
];
const FENETRES: &[(&str, &str)] = &[
    ("flottant", "fenêtres libres"),
    ("tuiles", "fenêtres rangées en tuiles"),
    ("bascule", "passer de l'un à l'autre"),
    ("etat", "mode en cours"),
    ("restaurer", "réglages d'origine"),
];
const CIBLE: &[(&str, &str)] = &[("fin", "terminer la mission"), ("rapport", "rapport Markdown de la mission"), ("terminaux", "brancher les autres shells")];

/// Vrai quand l'argument attendu est un identifiant de mode.
pub fn attend_mode(cmd: &str, avant: &[String]) -> bool {
    let a: Vec<&str> = avant.iter().map(|s| s.as_str()).collect();
    matches!((cmd, a.as_slice()), ("shadow", ["modes", "activer"]) | ("shadow-modes", ["activer"]))
}

#[cfg(test)]
mod tests {
    use super::champ;

    #[test]
    fn champ_json() {
        let j = "{\n \"cible\": \"10.10.11.5\",\n \"nom\": \"a\\\"b\\u00e9\",\n \"dossier\": \"/h/ctf/x\"\n}";
        assert_eq!(champ(j, "cible").as_deref(), Some("10.10.11.5"));
        assert_eq!(champ(j, "nom").as_deref(), Some("a\"bé"));
        assert_eq!(champ(j, "absent"), None);
    }
}
