//! Complétions des commandes courantes : sous-commandes décrites en français
//! et valeurs dynamiques (paquets, unités systemd, processus, scripts npm).

use std::process::{Command, Stdio};

type Table = &'static [(&'static str, &'static str)];

const APT: Table = &[
    ("install", "installe des paquets"),
    ("remove", "désinstalle des paquets"),
    ("purge", "désinstalle et efface la configuration"),
    ("autoremove", "retire les dépendances devenues inutiles"),
    ("update", "rafraîchit la liste des paquets"),
    ("upgrade", "met à jour les paquets installés"),
    ("full-upgrade", "met à jour, quitte à retirer des paquets"),
    ("search", "cherche un paquet"),
    ("show", "détails d'un paquet"),
    ("list", "liste des paquets"),
    ("reinstall", "réinstalle des paquets"),
    ("policy", "versions disponibles et priorités"),
    ("depends", "dépendances d'un paquet"),
    ("rdepends", "paquets qui en dépendent"),
    ("download", "télécharge le .deb"),
    ("source", "télécharge les sources"),
    ("clean", "vide le cache des paquets"),
    ("autoclean", "retire du cache les paquets périmés"),
    ("edit-sources", "édite les sources de paquets"),
    ("changelog", "journal des modifications"),
];

const SYSTEMCTL: Table = &[
    ("status", "état d'une unité"),
    ("start", "démarre"),
    ("stop", "arrête"),
    ("restart", "redémarre"),
    ("reload", "recharge la configuration"),
    ("enable", "active au démarrage"),
    ("disable", "désactive au démarrage"),
    ("is-active", "en cours d'exécution ?"),
    ("is-enabled", "activée au démarrage ?"),
    ("mask", "interdit tout démarrage"),
    ("unmask", "lève l'interdiction"),
    ("cat", "affiche le fichier d'unité"),
    ("edit", "modifie l'unité"),
    ("show", "propriétés d'une unité"),
    ("list-units", "unités chargées"),
    ("list-unit-files", "unités installées"),
    ("list-timers", "minuteurs"),
    ("daemon-reload", "relit les fichiers d'unité"),
    ("reboot", "redémarre la machine"),
    ("poweroff", "éteint la machine"),
    ("suspend", "met en veille"),
    ("hibernate", "hiberne"),
    ("kill", "envoie un signal à une unité"),
    ("reset-failed", "efface l'état d'échec"),
];

const DOCKER: Table = &[
    ("run", "lance un conteneur"),
    ("ps", "conteneurs"),
    ("images", "images"),
    ("exec", "commande dans un conteneur"),
    ("logs", "journaux d'un conteneur"),
    ("build", "construit une image"),
    ("pull", "télécharge une image"),
    ("push", "envoie une image"),
    ("stop", "arrête un conteneur"),
    ("start", "démarre un conteneur"),
    ("restart", "redémarre un conteneur"),
    ("rm", "supprime un conteneur"),
    ("rmi", "supprime une image"),
    ("compose", "applications multi-conteneurs"),
    ("inspect", "détails d'un objet"),
    ("network", "réseaux"),
    ("volume", "volumes"),
    ("system", "gestion de Docker"),
    ("cp", "copie depuis ou vers un conteneur"),
    ("tag", "étiquette une image"),
    ("login", "connexion à un registre"),
    ("stats", "consommation en direct"),
];

const DOCKER_COMPOSE: Table = &[
    ("up", "crée et démarre"),
    ("down", "arrête et supprime"),
    ("ps", "conteneurs"),
    ("logs", "journaux"),
    ("build", "construit les images"),
    ("pull", "télécharge les images"),
    ("restart", "redémarre"),
    ("stop", "arrête"),
    ("start", "démarre"),
    ("exec", "commande dans un service"),
    ("run", "commande ponctuelle"),
    ("config", "configuration résolue"),
];

const CARGO: Table = &[
    ("build", "compile"),
    ("run", "compile et lance"),
    ("test", "lance les tests"),
    ("check", "vérifie sans produire de binaire"),
    ("clippy", "analyse statique"),
    ("fmt", "met en forme"),
    ("add", "ajoute une dépendance"),
    ("remove", "retire une dépendance"),
    ("update", "met à jour les dépendances"),
    ("new", "crée un projet"),
    ("init", "crée un projet ici"),
    ("install", "installe un binaire"),
    ("uninstall", "désinstalle un binaire"),
    ("clean", "efface target/"),
    ("doc", "génère la documentation"),
    ("bench", "lance les bancs d'essai"),
    ("publish", "publie sur crates.io"),
    ("search", "cherche une crate"),
    ("tree", "arbre des dépendances"),
];

const NPM: Table = &[
    ("install", "installe les dépendances"),
    ("run", "lance un script"),
    ("start", "script start"),
    ("test", "script test"),
    ("init", "crée package.json"),
    ("uninstall", "retire un paquet"),
    ("update", "met à jour"),
    ("outdated", "paquets périmés"),
    ("list", "paquets installés"),
    ("ci", "installation propre"),
    ("audit", "failles connues"),
    ("publish", "publie le paquet"),
    ("exec", "lance un binaire"),
    ("link", "lie un paquet local"),
];

const PIP: Table = &[
    ("install", "installe des paquets"),
    ("uninstall", "désinstalle des paquets"),
    ("list", "paquets installés"),
    ("show", "détails d'un paquet"),
    ("freeze", "versions figées"),
    ("download", "télécharge sans installer"),
    ("check", "vérifie les dépendances"),
    ("cache", "gère le cache"),
    ("index", "versions disponibles"),
];

const TMUX: Table = &[
    ("new", "nouvelle session"),
    ("attach", "rejoint une session"),
    ("ls", "sessions"),
    ("kill-session", "ferme une session"),
    ("kill-server", "ferme tout"),
    ("rename-session", "renomme la session"),
    ("split-window", "coupe le panneau"),
    ("new-window", "nouvelle fenêtre"),
    ("source-file", "recharge une configuration"),
    ("list-keys", "raccourcis"),
];

const SNAP: Table = &[
    ("install", "installe"),
    ("remove", "désinstalle"),
    ("refresh", "met à jour"),
    ("list", "snaps installés"),
    ("find", "cherche"),
    ("info", "détails"),
    ("revert", "revient à la version précédente"),
    ("connections", "interfaces"),
];

const FLATPAK: Table = &[
    ("install", "installe"),
    ("uninstall", "désinstalle"),
    ("update", "met à jour"),
    ("list", "applications installées"),
    ("run", "lance une application"),
    ("search", "cherche"),
    ("info", "détails"),
    ("remote-ls", "contenu d'un dépôt"),
];

const JOURNALCTL_ABSENT: Table = &[];

fn table(cmd: &str) -> Table {
    match cmd {
        "apt" | "apt-get" => APT,
        "systemctl" => SYSTEMCTL,
        "docker" | "podman" => DOCKER,
        "cargo" => CARGO,
        "npm" | "pnpm" => NPM,
        "pip" | "pip3" | "pipx" => PIP,
        "tmux" => TMUX,
        "snap" => SNAP,
        "flatpak" => FLATPAK,
        _ => JOURNALCTL_ABSENT,
    }
}

/// Sortie d'une commande, ligne par ligne ; vide si elle échoue ou traîne.
fn lignes(prog: &str, args: &[&str]) -> Vec<String> {
    let Ok(mut enfant) = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    // Lecture dans un fil : la complétion ne doit jamais bloquer la saisie.
    let Some(mut sortie) = enfant.stdout.take() else { return Vec::new() };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut sortie, &mut s);
        let _ = tx.send(s);
    });
    let texte = match rx.recv_timeout(std::time::Duration::from_millis(900)) {
        Ok(s) => s,
        Err(_) => {
            let _ = enfant.kill();
            String::new()
        }
    };
    let _ = enfant.wait();
    texte.lines().map(|l| l.to_string()).collect()
}

/// Noms de tous les paquets connus d'apt, gardés dans ~/.cache/shsh/paquets :
/// `apt-cache pkgnames` met plusieurs secondes, trop pour une touche Tab.
fn cache_paquets() -> Option<String> {
    use std::time::{Duration, Instant, SystemTime};
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| s.starts_with('/'))
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.cache")))?;
    let dir = format!("{base}/shsh");
    let fichier = format!("{dir}/paquets");
    let date = |p: &str| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let frais = match (date(&fichier), date("/var/lib/apt/lists")) {
        (Some(c), Some(l)) => c >= l,
        (Some(c), None) => SystemTime::now().duration_since(c).map(|d| d < Duration::from_secs(7 * 86400)).unwrap_or(false),
        (None, _) => false,
    };
    if !frais {
        let _ = std::fs::create_dir_all(&dir);
        // Double fork : la génération continue même si on n'attend pas la fin.
        let script = format!(
            "(apt-cache pkgnames 2>/dev/null | sort -u > '{f}.tmp.$$' && mv -f '{f}.tmp.$$' '{f}') >/dev/null 2>&1 &",
            f = fichier.replace('\'', "")
        );
        let _ = Command::new("sh").arg("-c").arg(script).stdin(Stdio::null()).status();
        let ancien = date(&fichier);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(4000) && date(&fichier) == ancien {
            std::thread::sleep(Duration::from_millis(60));
        }
    }
    std::fs::read_to_string(&fichier).ok()
}

fn paquets(prefixe: &str, installes: bool) -> Vec<(String, String)> {
    let noms: Vec<String> = if installes {
        lignes("dpkg-query", &["-W", "-f", "${Package}\n", &format!("{prefixe}*")])
    } else {
        if prefixe.len() < 2 {
            return Vec::new();
        }
        cache_paquets().map(|t| t.lines().filter(|n| n.starts_with(prefixe)).map(String::from).collect()).unwrap_or_default()
    };
    let mut v: Vec<String> = noms.into_iter().filter(|n| n.starts_with(prefixe)).collect();
    v.sort();
    v.dedup();
    v.truncate(400);
    v.into_iter().map(|n| (n, "paquet".to_string())).collect()
}

/// Unités systemd, lues dans les dossiers d'unités (instantané, là où
/// `systemctl list-unit-files` peut prendre plusieurs secondes).
fn unites(prefixe: &str, utilisateur: bool) -> Vec<(String, String)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dossiers: Vec<String> = if utilisateur {
        vec![
            format!("{home}/.config/systemd/user"),
            "/etc/systemd/user".into(),
            "/usr/lib/systemd/user".into(),
            format!("{home}/.local/share/systemd/user"),
        ]
    } else {
        vec![
            "/etc/systemd/system".into(),
            "/run/systemd/system".into(),
            "/usr/lib/systemd/system".into(),
            "/usr/local/lib/systemd/system".into(),
        ]
    };
    let mut v: Vec<(String, String)> = Vec::new();
    for d in dossiers {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Some(nom) = e.file_name().to_str().map(String::from) else { continue };
            let Some((_, genre)) = nom.rsplit_once('.') else { continue };
            let libelle = match genre {
                "service" => "service",
                "timer" => "minuteur",
                "socket" => "socket",
                "target" => "cible",
                "mount" => "montage",
                "path" => "chemin surveillé",
                _ => continue,
            };
            if nom.starts_with(prefixe) && !nom.contains('@') {
                v.push((nom, libelle.to_string()));
            }
        }
    }
    // Les services d'abord : c'est ce qu'on manipule le plus
    v.sort_by_key(|(n, _)| (!n.ends_with(".service"), n.clone()));
    v.dedup();
    v.truncate(400);
    v
}

/// Processus de l'utilisateur : (pid, nom).
fn processus() -> Vec<(String, String)> {
    let moi = unsafe { libc::getuid() };
    let mut v = Vec::new();
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let nom = e.file_name();
        let Some(pid) = nom.to_str().filter(|s| s.bytes().all(|b| b.is_ascii_digit())) else { continue };
        use std::os::unix::fs::MetadataExt;
        if e.metadata().map(|m| m.uid()).unwrap_or(u32::MAX) != moi {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) {
            v.push((pid.to_string(), comm.trim().to_string()));
        }
    }
    v
}

fn scripts_npm(cwd: &str) -> Vec<(String, String)> {
    let Ok(txt) = std::fs::read_to_string(format!("{cwd}/package.json")) else { return Vec::new() };
    let Some(debut) = txt.find("\"scripts\"") else { return Vec::new() };
    let Some(ouvre) = txt[debut..].find('{') else { return Vec::new() };
    let corps = &txt[debut + ouvre + 1..];
    let corps = &corps[..corps.find('}').unwrap_or(corps.len())];
    let re = regex::Regex::new(r#""([^"]+)"\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap();
    re.captures_iter(corps).map(|c| (c[1].to_string(), c[2].to_string())).collect()
}

/// Vrai si `candidats` sait répondre pour cette commande.
pub fn connue(cmd: &str) -> bool {
    matches!(cmd, "kill" | "killall" | "pkill" | "pgrep") || !table(cmd).is_empty()
}

/// Candidats (valeur, description) pour l'argument en cours, ou `None` si la
/// commande n'est pas connue ici (on retombe alors sur les fichiers).
/// `avant` : les arguments déjà saisis après le nom de la commande.
pub fn candidats(cmd: &str, avant: &[String], mot: &str, cwd: &str) -> Option<Vec<(String, String)>> {
    let positionnels: Vec<&str> = avant.iter().map(|s| s.as_str()).filter(|s| !s.starts_with('-')).collect();
    let sous = positionnels.first().copied();
    let garde = |t: Table| -> Vec<(String, String)> {
        t.iter().filter(|(n, _)| n.starts_with(mot)).map(|(n, d)| (n.to_string(), d.to_string())).collect()
    };
    match cmd {
        "kill" => {
            let v = processus()
                .into_iter()
                .filter(|(pid, _)| pid.starts_with(mot))
                .collect::<Vec<_>>();
            return Some(v);
        }
        "killall" | "pkill" | "pgrep" => {
            let mut noms: Vec<String> = processus().into_iter().map(|(_, n)| n).filter(|n| n.starts_with(mot)).collect();
            noms.sort();
            noms.dedup();
            return Some(noms.into_iter().map(|n| (n, "processus".to_string())).collect());
        }
        _ => {}
    }
    let t = table(cmd);
    if t.is_empty() {
        return None;
    }
    let Some(sous) = sous else { return Some(garde(t)) };
    let v = match (cmd, sous) {
        ("apt" | "apt-get", "install" | "show" | "policy" | "depends" | "rdepends" | "download" | "source" | "changelog") => {
            // un chemin (./paquet.deb) reste une complétion de fichier
            if mot.starts_with('.') || mot.starts_with('/') || mot.starts_with('~') {
                return None;
            }
            paquets(mot, false)
        }
        ("apt" | "apt-get", "remove" | "purge" | "reinstall") => paquets(mot, true),
        ("systemctl", s) if SYSTEMCTL.iter().any(|(n, _)| *n == s) && !s.starts_with("list-") && !matches!(s, "daemon-reload" | "reboot" | "poweroff" | "suspend" | "hibernate") => {
            unites(mot, avant.iter().any(|a| a == "--user"))
        }
        ("docker" | "podman", "compose") if positionnels.len() == 1 => garde(DOCKER_COMPOSE),
        ("npm" | "pnpm", "run") if positionnels.len() == 1 => {
            scripts_npm(cwd).into_iter().filter(|(n, _)| n.starts_with(mot)).collect()
        }
        _ => return None,
    };
    Some(v)
}

/// Paquet apt qui fournit une commande absente (base de command-not-found).
pub fn paquet_fournissant(commande: &str) -> Option<String> {
    if !commande.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b)) {
        return None;
    }
    if !std::path::Path::new("/var/lib/command-not-found/commands.db").exists() {
        return None;
    }
    let script = "import sqlite3,sys\n\
c=sqlite3.connect('file:/var/lib/command-not-found/commands.db?mode=ro',uri=True)\n\
r=c.execute('select p.name from commands c join packages p on p.pkgID=c.pkgID where c.command=? order by p.priority desc limit 1',(sys.argv[1],)).fetchone()\n\
print(r[0] if r else '')";
    lignes("python3", &["-I", "-c", script, commande]).into_iter().next().filter(|p| !p.is_empty())
}
