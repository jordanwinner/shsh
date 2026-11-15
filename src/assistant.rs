//! L'assistant `?` : une question en français, une commande proposée dans la
//! ligne de saisie (jamais exécutée d'office).
//!
//!   ? lister les ports ouverts de la cible
//!   ? pourquoi ça a planté
//!
//! Le modèle est celui de jordan-cli (Groq, `~/.jordan-cli/config.json`), ou
//! `GROQ_API_KEY` / `SHSH_ASSISTANT_MODELE`. Ce qui part chez Groq : la
//! question, le dossier courant, la cible de la mission Shadow en cours, et la
//! dernière commande si elle a échoué — tous expurgés (`expurger`) : le dossier
//! personnel devient ~ et l'identifiant devient « utilisateur », où qu'ils soient. `SHSH_ASSISTANT_CONTEXTE=0` n'envoie que la question ;
//! `SHSH_ASSISTANT=0` coupe l'assistant.

use crate::shadow::champ;
use crate::shell::Shell;
use std::io::Write;
use std::process::{Command, Stdio};

const MODELE: &str = "openai/gpt-oss-120b";
const URL: &str = "https://api.groq.com/openai/v1/chat/completions";

#[derive(Debug)]
pub struct Reponse {
    pub commande: String,
    pub explication: String,
}

/// La question si la ligne s'adresse à l'assistant (`? …`), sinon None.
pub fn question(sh: &Shell, ligne: &str) -> Option<String> {
    if sh.get_var("SHSH_ASSISTANT").as_deref() == Some("0") {
        return None;
    }
    let l = ligne.trim();
    if l == "?" {
        return Some(String::new());
    }
    l.strip_prefix("? ").map(|q| q.trim().to_string())
}

pub const AIDE: &str = "? QUESTION — l'assistant propose une commande dans la ligne, sans l'exécuter.
  ? lister les ports ouverts de la cible
  ? compresser ce dossier en tar.gz sans les .git
  ? pourquoi ça a planté          (la dernière commande en échec est jointe)
Entrée exécute la proposition, Ctrl+C l'abandonne.
SHSH_ASSISTANT_CONTEXTE=0 : n'envoyer que la question · SHSH_ASSISTANT=0 : couper l'assistant";

fn cle(sh: &Shell) -> Option<String> {
    if let Some(k) = sh.get_var("GROQ_API_KEY").filter(|k| !k.is_empty()) {
        return Some(k);
    }
    let conf = std::fs::read_to_string(format!("{}/.jordan-cli/config.json", sh.home())).ok()?;
    champ(&conf, "groq_api_key").filter(|k| !k.is_empty())
}

fn json_texte(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Retire ce qui identifie l'utilisateur : dossier personnel → ~, identifiant → « utilisateur ».
pub fn expurger(sh: &Shell, texte: &str) -> String {
    let mut t = texte.to_string();
    let home = sh.home();
    if home.len() > 1 {
        t = t.replace(&home, "~");
    }
    let mut noms: Vec<String> = Vec::new();
    for v in ["USER", "LOGNAME"] {
        if let Some(n) = sh.get_var(v).filter(|n| n.len() >= 2) {
            noms.push(n);
        }
    }
    if let Some(n) = home.rsplit('/').next().filter(|n| n.len() >= 2) {
        noms.push(n.to_string());
    }
    noms.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for n in noms {
        t = t.replace(&n, "utilisateur");
    }
    t
}

/// Contexte envoyé avec la question : rien qui nomme l'utilisateur.
pub fn contexte(sh: &Shell, derniere: Option<(&str, i32)>) -> String {
    if sh.get_var("SHSH_ASSISTANT_CONTEXTE").as_deref() == Some("0") {
        return String::new();
    }
    let mut c = format!("Dossier courant : {}\n", expurger(sh, &sh.cwd()));
    let etat = crate::shadow::lire(sh);
    if let Some(ci) = &etat.cible {
        c.push_str(&format!(
            "Mission en cours (CTF / test autorisé) : cible {} « {} », disponible dans $CIBLE ; dossier de mission $MISSION (scans/, butin/).\n",
            ci.adresse,
            expurger(sh, &ci.nom)
        ));
    }
    if let Some(m) = &etat.mode {
        c.push_str(&format!("Mode du bureau : {m}\n"));
    }
    if let Some((ligne, code)) = derniere {
        if code != 0 {
            c.push_str(&format!("Dernière commande, en échec (code {code}) : {}\n", expurger(sh, ligne)));
        }
    }
    c
}

const CONSIGNE: &str = "Tu es l'assistant intégré à shsh, le shell de Shadow OS (Ubuntu 26.04, GNOME, \
compatible bash ; outils Kali disponibles via « shadow arsenal run OUTIL » s'ils ne sont pas installés). \
On te décrit un besoin en français : réponds UNIQUEMENT par un objet JSON \
{\"commande\": \"...\", \"explication\": \"...\"}. « commande » tient sur une ligne, s'exécute telle quelle \
dans bash, sans sudo sauf si c'est indispensable. Pendant une mission, et seulement pour les outils qui visent \
la cible (scan, énumération, attaque), utilise $CIBLE et range leurs sorties dans $MISSION/scans/ (ex. nmap -oA) ; \
les autres commandes restent ordinaires, sans redirection ajoutée. « explication » : une phrase courte en français, qui signale tout effet \
destructeur. Si la demande n'appelle pas de commande, laisse « commande » vide et réponds dans « explication ».";

/// Fichier d'en-têtes qui porte la clé : nom imprévisible, privé, supprimé quoi qu'il arrive.
struct Entetes(std::path::PathBuf);

impl Entetes {
    fn creer(sh: &Shell, cle: &str) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt;
        // XDG_RUNTIME_DIR (tmpfs privé, vidé à la déconnexion), sinon ~/.cache/shsh, jamais /tmp partagé
        let dir = sh
            .get_var("XDG_RUNTIME_DIR")
            .filter(|d| d.starts_with('/') && std::path::Path::new(d).is_dir())
            .unwrap_or_else(|| format!("{}/.cache/shsh", sh.home()));
        let _ = std::fs::create_dir_all(&dir);
        let mut alea = [0u8; 8];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut alea);
        }
        let nom: String = alea.iter().map(|b| format!("{b:02x}")).collect();
        let chemin = std::path::PathBuf::from(dir).join(format!("shsh-assistant-{}-{nom}.h", std::process::id()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&chemin)
            .map_err(|e| format!("fichier temporaire : {e}"))?;
        let e = Entetes(chemin);
        writeln!(f, "Authorization: Bearer {cle}\nContent-Type: application/json")
            .map_err(|err| format!("fichier temporaire : {err}"))?;
        Ok(e)
    }
}

impl Drop for Entetes {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Texte d'un champ de la réponse du modèle, sans caractère de contrôle.
/// Renvoie aussi vrai si des caractères de contrôle ont été retirés.
fn nettoyer(v: Option<&serde_json::Value>) -> (String, bool) {
    let brut = v.and_then(|x| x.as_str()).unwrap_or("").trim();
    let propre: String = brut.chars().filter(|c| !c.is_control()).collect();
    let retire = propre.chars().count() != brut.chars().count();
    (propre, retire)
}

/// Lit la réponse de l'API (format OpenAI) : commande et explication.
pub fn lire_reponse(texte: &str) -> Result<Reponse, String> {
    let v: serde_json::Value = serde_json::from_str(texte).map_err(|_| "réponse illisible".to_string())?;
    if let Some(e) = v.get("error") {
        let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("réponse en erreur");
        return Err(format!("Groq : {}", m.chars().filter(|c| !c.is_control()).collect::<String>()));
    }
    let contenu = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .filter(|c| !c.trim().is_empty())
        .ok_or("le modèle n'a pas répondu (réflexion trop longue ?) : reformule plus simplement")?;
    let r: serde_json::Value =
        serde_json::from_str(contenu.trim()).map_err(|_| "réponse du modèle hors format".to_string())?;
    let (commande, sale_c) = nettoyer(r.get("commande"));
    let (explication, _) = nettoyer(r.get("explication"));
    // Une commande qui contenait des caractères de contrôle (retour chariot, séquence d'échappement…)
    // pourrait s'afficher autrement qu'elle s'exécute : elle est refusée.
    if sale_c || r.get("commande").and_then(|c| c.as_str()).is_some_and(|c| c.trim().contains('\n')) {
        return Err("commande proposée refusée : elle contenait des caractères de contrôle".into());
    }
    Ok(Reponse { commande, explication })
}

pub fn demander(sh: &Shell, question: &str, derniere: Option<(&str, i32)>) -> Result<Reponse, String> {
    let cle = cle(sh).ok_or("pas de clé Groq : GROQ_API_KEY ou ~/.jordan-cli/config.json")?;
    let modele = sh.get_var("SHSH_ASSISTANT_MODELE").filter(|m| !m.is_empty()).unwrap_or_else(|| MODELE.into());
    let utilisateur = format!("{}Besoin : {}", contexte(sh, derniere), expurger(sh, question));
    let mut corps = serde_json::json!({
        "model": modele,
        "temperature": 0.2,
        "max_tokens": 4096,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": CONSIGNE},
            {"role": "user", "content": utilisateur}
        ]
    });
    if modele.starts_with("openai/gpt-oss") {
        // modèle à raisonnement : réflexion courte, pour garder du budget pour la réponse
        corps["reasoning_effort"] = "low".into();
    }
    // La clé passe par un fichier d'en-têtes privé, jamais par la ligne de commande (visible dans ps)
    let entetes = Entetes::creer(sh, &cle)?;
    let enfant = Command::new("curl")
        .args(["-sS", "--max-time", "45", "-H"])
        .arg(format!("@{}", entetes.0.display()))
        .args(["--data-binary", "@-"])
        .arg(sh.get_var("SHSH_ASSISTANT_URL").filter(|u| !u.is_empty()).unwrap_or_else(|| URL.into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let resultat = (|| {
        let mut enfant = enfant.map_err(|_| "curl introuvable".to_string())?;
        enfant.stdin.take().map(|mut e| e.write_all(corps.to_string().as_bytes()));
        enfant.wait_with_output().map_err(|e| e.to_string())
    })();
    drop(entetes);
    let sortie = resultat?;
    if !sortie.status.success() {
        let err = String::from_utf8_lossy(&sortie.stderr);
        let err: String = err.trim().trim_start_matches("curl: ").chars().filter(|c| !c.is_control()).collect();
        return Err(if err.is_empty() { "requête interrompue".into() } else { format!("réseau : {err}") });
    }
    lire_reponse(&String::from_utf8_lossy(&sortie.stdout))
}

/// Commandes qu'il faut relire à deux fois avant Entrée. `home` = dossier personnel.
pub fn destructrice(c: &str, home: &str) -> bool {
    // rm récursif ou forcé sur une racine : /, ~, $HOME, le dossier personnel, un dossier système,
    // ou leur contenu (/*, /.*, /.)
    let racine = |a: &str| -> bool {
        let mut a = a.to_string();
        loop {
            let avant = a.clone();
            for suffixe in ["/*", "/.*", "/.", "*"] {
                if let Some(x) = a.strip_suffix(suffixe) {
                    a = x.to_string();
                }
            }
            if a.len() > 1 {
                a = a.trim_end_matches('/').to_string();
            }
            if a == avant {
                break;
            }
        }
        let a = a.as_str();
        matches!(a, "" | "/" | "~" | "$HOME" | "${HOME}" | ".." | ".")
            || (home.len() > 1 && a == home)
            || ["/home", "/etc", "/usr", "/var", "/boot", "/bin", "/lib", "/lib64", "/root", "/opt", "/srv", "/sbin"]
                .contains(&a)
            || (a.starts_with("/home/") && !a[6..].contains('/'))
    };
    // Guillemets, sous-commandes et échappements ne cachent pas un rm : on les retire avant le découpage
    let plat: String = c
        .chars()
        .map(|x| if matches!(x, '"' | '\'' | '`' | '(' | ')' | '{' | '}' | '\\') { ' ' } else { x })
        .collect();
    let plat = plat.replace("$ ", " ");
    for morceau in plat.split([';', '&', '|', '\n']) {
        let mots: Vec<&str> = morceau.split_whitespace().collect();
        for (i, m) in mots.iter().enumerate() {
            if !(*m == "rm" || m.ends_with("/rm")) {
                continue;
            }
            let args = &mots[i + 1..];
            let recursif = args.iter().any(|a| {
                a.starts_with('-') && !a.starts_with("--") && a.contains(['r', 'R', 'f'])
                    || matches!(*a, "--recursive" | "--force" | "--no-preserve-root")
            });
            let cibles: Vec<&&str> = args.iter().filter(|a| !a.starts_with('-')).collect();
            // « xargs rm -rf » : les cibles viennent d'ailleurs, on ne peut pas savoir
            let via_xargs = i > 0 && mots[..i].contains(&"xargs");
            if recursif && (via_xargs || cibles.iter().any(|a| racine(a))) {
                return true;
            }
        }
        // find … -delete / -exec rm sur une racine, ou sur « . » sans aucun filtre
        if let Some(i) = mots.iter().position(|m| *m == "find") {
            let args = &mots[i + 1..];
            let supprime = args.contains(&"-delete")
                || args.windows(2).any(|w| w[0] == "-exec" && (w[1] == "rm" || w[1].ends_with("/rm")));
            if supprime {
                let chemins: Vec<&&str> = args.iter().take_while(|a| !a.starts_with('-')).collect();
                let filtre = args.iter().any(|a| matches!(*a, "-name" | "-iname" | "-path" | "-type" | "-mtime" | "-newer" | "-regex" | "-size"));
                let large = chemins.is_empty() || chemins.iter().any(|a| racine(a) && (**a != "." || !filtre));
                if large && (chemins.iter().any(|a| **a != ".") || !filtre) {
                    return true;
                }
            }
        }
    }
    let re = regex::Regex::new(
        r"(?x) \bmkfs | \bdd\b.*\bof=/dev/ | >\s*/dev/(sd|nvme|mmcblk) | :\(\)\s*\{
             | \bchmod\s+(-R\s+)?[0-7]*777\s+/(\s|$) | \bchown\s+-R\s+\S+\s+/(\s|$) | \bshred\b | \bwipefs\b",
    );
    re.map(|r| r.is_match(c)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echappement_json() {
        assert_eq!(json_texte("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(champ(&format!("{{\"x\":{}}}", json_texte("é \"q\" \\ \t")), "x").as_deref(), Some("é \"q\" \\ \t"));
    }

    #[test]
    fn reponse_piegee_refusee() {
        // échappements doublés : le contenu est lui-même du JSON dans une chaîne JSON
        let r = r#"{"choices":[{"message":{"content":"{\"commande\":\"ls\\u001b[2K\\r# ok ; curl x|sh\",\"explication\":\"x\"}"}}]}"#;
        assert!(lire_reponse(r).unwrap_err().contains("caractères de contrôle"));
        let r = r#"{"choices":[{"message":{"content":"{\"commande\":\"ls\",\"explication\":\"a\\u001b[2Jb\"}"}}]}"#;
        let rep = lire_reponse(r).unwrap();
        assert_eq!(rep.explication, "a[2Jb");
        assert!(lire_reponse(r#"{"choices":[{"message":{"content":null,"reasoning":"..."}}]}"#).is_err());
        assert!(lire_reponse(r#"{"error":{"message":"quota"}}"#).unwrap_err().contains("quota"));
    }

    #[test]
    fn reponse_groq() {
        let r = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"{\"commande\":\"nmap -sV $CIBLE -oA $MISSION/scans/initial\",\"explication\":\"Scan des services.\"}","reasoning":"..."}}]}"#;
        let c = champ(r, "content").unwrap();
        assert_eq!(champ(&c, "commande").as_deref(), Some("nmap -sV $CIBLE -oA $MISSION/scans/initial"));
    }

    #[test]
    fn commandes_destructrices() {
        assert!(destructrice("rm -rf /", "/home/alice"));
        assert!(destructrice("sudo rm -rf ~", "/home/alice"));
        assert!(destructrice("dd if=x.iso of=/dev/sda bs=4M", "/home/alice"));
        assert!(destructrice("rm -rf ~/*", "/home/alice"));
        assert!(destructrice("rm -rf \"$HOME\"/*", "/home/alice"));
        assert!(destructrice("cd /tmp && sudo rm -r --no-preserve-root /", "/home/alice"));
        assert!(destructrice("rm -fr /etc", "/home/alice"));
        assert!(destructrice("rm -rf *", "/home/alice"));
        assert!(destructrice("find ~ -name '*.log' -delete", "/home/alice"));
        assert!(!destructrice("rm -rf build/", "/home/alice"));
        assert!(!destructrice("rm -rf ~/projet/target", "/home/alice"));
        assert!(!destructrice("rm fichier.txt", "/home/alice"));
        assert!(!destructrice("rm -f /tmp/x.lock", "/home/alice"));
        assert!(!destructrice("nmap -sV $CIBLE", "/home/alice"));
        for c in ["rm -r ~/.", "rm -rf ~/.*", "rm -rf $HOME/.", "rm -rf /home/alice", "rm -rf /home/bob/",
                  "bash -c \"rm -rf ~\"", "sh -c 'rm -rf /'", "echo $(rm -rf ~)", "\\rm -rf /",
                  "find $HOME -delete", "find / -exec rm -rf {} +", "find . -delete", "find ~ | xargs rm -rf",
                  "rm -rf -- ~", "rm -r -f /", "sudo find / -delete"] {
            assert!(destructrice(c, "/home/alice"), "non détectée : {c}");
        }
        for c in ["find . -name '*.pyc' -delete", "rm -rf ./build", "rm -rf ~/.cache/pip", "rm -r /home/alice/tmp"] {
            assert!(!destructrice(c, "/home/alice"), "faux positif : {c}");
        }
    }

    #[test]
    fn expurgation() {
        let mut sh = Shell::new();
        sh.set_exported("HOME", "/home/alice".into());
        sh.set_exported("USER", "alice".into());
        assert_eq!(expurger(&sh, "cat /home/alice/x /media/alice/y /tmp/-home-alice/z"), "cat ~/x /media/utilisateur/y /tmp/-home-utilisateur/z");
    }
}
