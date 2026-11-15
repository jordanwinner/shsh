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
dans bash, sans sudo sauf si c'est indispensable. Pendant une mission, utilise $CIBLE et range les sorties \
dans $MISSION/scans/ (ex. nmap -oA). « explication » : une phrase courte en français, qui signale tout effet \
destructeur. Si la demande n'appelle pas de commande, laisse « commande » vide et réponds dans « explication ».";

pub fn demander(sh: &Shell, question: &str, derniere: Option<(&str, i32)>) -> Result<Reponse, String> {
    let cle = cle(sh).ok_or("pas de clé Groq : GROQ_API_KEY ou ~/.jordan-cli/config.json")?;
    let modele = sh.get_var("SHSH_ASSISTANT_MODELE").filter(|m| !m.is_empty()).unwrap_or_else(|| MODELE.into());
    let utilisateur = format!("{}Besoin : {}", contexte(sh, derniere), expurger(sh, question));
    let corps = format!(
        "{{\"model\":{},\"temperature\":0.2,\"max_tokens\":2048,\"response_format\":{{\"type\":\"json_object\"}},\
\"messages\":[{{\"role\":\"system\",\"content\":{}}},{{\"role\":\"user\",\"content\":{}}}]}}",
        json_texte(&modele),
        json_texte(CONSIGNE),
        json_texte(&utilisateur)
    );
    // La clé passe par un fichier d'en-têtes privé, jamais par la ligne de commande (visible dans ps)
    let dir = sh.get_var("XDG_RUNTIME_DIR").filter(|d| d.starts_with('/')).unwrap_or_else(|| "/tmp".into());
    let entetes = format!("{dir}/shsh-assistant-{}.h", std::process::id());
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&entetes)
            .map_err(|e| format!("fichier temporaire : {e}"))?;
        let _ = writeln!(f, "Authorization: Bearer {cle}\nContent-Type: application/json");
    }
    let enfant = Command::new("curl")
        .args(["-sS", "--max-time", "45", "-H"])
        .arg(format!("@{entetes}"))
        .args(["--data-binary", "@-"])
        .arg(sh.get_var("SHSH_ASSISTANT_URL").filter(|u| !u.is_empty()).unwrap_or_else(|| URL.into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let resultat = (|| {
        let mut enfant = enfant.map_err(|_| "curl introuvable".to_string())?;
        enfant.stdin.take().map(|mut e| e.write_all(corps.as_bytes()));
        enfant.wait_with_output().map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&entetes);
    let sortie = resultat?;
    let texte = String::from_utf8_lossy(&sortie.stdout);
    if !sortie.status.success() {
        let err = String::from_utf8_lossy(&sortie.stderr);
        return Err(format!("réseau : {}", err.trim().trim_start_matches("curl: ")));
    }
    if texte.contains("\"error\"") && !texte.contains("\"choices\"") {
        return Err(format!("Groq : {}", champ(&texte, "message").unwrap_or_else(|| "réponse en erreur".into())));
    }
    let contenu = champ(&texte, "content").ok_or("réponse illisible")?;
    Ok(Reponse {
        commande: champ(&contenu, "commande").unwrap_or_default().lines().next().unwrap_or("").trim().to_string(),
        explication: champ(&contenu, "explication").unwrap_or_default().trim().to_string(),
    })
}

/// Commandes qu'il faut relire à deux fois avant Entrée.
pub fn destructrice(c: &str) -> bool {
    // rm récursif ou forcé sur une racine : /, ~, $HOME, *, un dossier système, ou leur contenu (/*, ~/*)
    let racine = |a: &str| {
        let a = a.replace(['"', '\''], "");
        let a = a.strip_suffix("/*").or_else(|| a.strip_suffix('*')).unwrap_or(&a);
        let a = if a.len() > 1 { a.trim_end_matches('/') } else { a };
        matches!(a, "" | "/" | "~" | "$HOME" | "${HOME}" | ".." | ".")
            || ["/home", "/etc", "/usr", "/var", "/boot", "/bin", "/lib", "/root", "/opt", "/srv"].contains(&a)
    };
    for morceau in c.split(|x| x == ';' || x == '&' || x == '|') {
        let mots: Vec<&str> = morceau.split_whitespace().collect();
        if let Some(i) = mots.iter().position(|m| *m == "rm" || m.ends_with("/rm")) {
            let args = &mots[i + 1..];
            let recursif = args.iter().any(|a| {
                a.starts_with('-') && !a.starts_with("--") && a.contains(['r', 'R', 'f'])
                    || matches!(*a, "--recursive" | "--force" | "--no-preserve-root")
            });
            if recursif && args.iter().filter(|a| !a.starts_with('-')).any(|a| racine(a)) {
                return true;
            }
        }
    }
    let re = regex::Regex::new(
        r"(?x) \bmkfs | \bdd\b.*\bof=/dev/ | >\s*/dev/(sd|nvme|mmcblk) | :\(\)\s*\{
             | \bchmod\s+(-R\s+)?[0-7]*777\s+/(\s|$) | \bchown\s+-R\s+\S+\s+/(\s|$) | \bshred\b | \bwipefs\b
             | \bfind\s+(/|~)\S*\s.*-delete",
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
    fn reponse_groq() {
        let r = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"{\"commande\":\"nmap -sV $CIBLE -oA $MISSION/scans/initial\",\"explication\":\"Scan des services.\"}","reasoning":"..."}}]}"#;
        let c = champ(r, "content").unwrap();
        assert_eq!(champ(&c, "commande").as_deref(), Some("nmap -sV $CIBLE -oA $MISSION/scans/initial"));
    }

    #[test]
    fn commandes_destructrices() {
        assert!(destructrice("rm -rf /"));
        assert!(destructrice("sudo rm -rf ~"));
        assert!(destructrice("dd if=x.iso of=/dev/sda bs=4M"));
        assert!(destructrice("rm -rf ~/*"));
        assert!(destructrice("rm -rf \"$HOME\"/*"));
        assert!(destructrice("cd /tmp && sudo rm -r --no-preserve-root /"));
        assert!(destructrice("rm -fr /etc"));
        assert!(destructrice("rm -rf *"));
        assert!(destructrice("find ~ -name '*.log' -delete"));
        assert!(!destructrice("rm -rf build/"));
        assert!(!destructrice("rm -rf ~/projet/target"));
        assert!(!destructrice("rm fichier.txt"));
        assert!(!destructrice("rm -f /tmp/x.lock"));
        assert!(!destructrice("nmap -sV $CIBLE"));
    }

    #[test]
    fn expurgation() {
        let mut sh = Shell::new();
        sh.set_exported("HOME", "/home/alice".into());
        sh.set_exported("USER", "alice".into());
        assert_eq!(expurger(&sh, "cat /home/alice/x /media/alice/y /tmp/-home-alice/z"), "cat ~/x /media/utilisateur/y /tmp/-home-utilisateur/z");
    }
}
