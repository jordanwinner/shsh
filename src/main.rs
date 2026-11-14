//! shsh — le shadow shell.

mod arith;
mod ast;
mod builtins;
mod completions;
mod editor;
mod exec;
mod expand;
mod jobs;
mod parser;
mod prompt;
mod shadow;
mod shell;

use nix::sys::signal::{SigHandler, Signal};
use nix::unistd;
use reedline::Signal as RlSignal;
use shell::*;
use std::io::IsTerminal;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const DEFAULT_RC: &str = r#"# ~/.shshrc — configuration de shsh (syntaxe POSIX)
# Ce fichier est lu au démarrage de chaque shell interactif.

# --- Alias -----------------------------------------------------------------
alias ll='ls -lh --color=auto'
alias la='ls -lAh --color=auto'
alias ls='ls --color=auto'
alias grep='grep --color=auto'
alias ..='cd ..'
alias ...='cd ../..'

# --- Abréviations (développées quand on tape Espace) -----------------------
abbr gs='git status'
abbr gc='git commit'
abbr gp='git push'
abbr gl='git log --oneline --graph --decorate -20'

# --- Crochets (facultatifs) -------------------------------------------------
# shsh_precmd()  { : ; }        # avant chaque prompt
# shsh_preexec() { : "$1" ; }   # avant chaque commande ($1 = la ligne)
# chpwd()        { ls ; }       # après chaque changement de dossier
"#;

fn usage() {
    println!(
        "shsh {} — shadow shell\n\n\
         usage : shsh [options] [script [arguments…]]\n\
         \x20       shsh -c 'commande' [nom [arguments…]]\n\n\
         options :\n\
         \x20 -c CMD     exécute CMD\n\
         \x20 -i         force le mode interactif\n\
         \x20 -l         shell de connexion (lit ~/.profile)\n\
         \x20 -s         lit les commandes sur l'entrée standard\n\
         \x20 -e -u -x   comme set -e / -u / -x\n\
         \x20 --norc     ne lit pas ~/.shshrc\n\
         \x20 --version  affiche la version",
        env!("CARGO_PKG_VERSION")
    );
}

fn init_job_control(sh: &mut Shell) {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return;
    }
    // Attendre d'être au premier plan
    loop {
        match unistd::tcgetpgrp(&stdin) {
            Ok(pg) if pg == unistd::getpgrp() => break,
            Ok(_) => {
                let _ = nix::sys::signal::killpg(unistd::getpgrp(), Signal::SIGTTIN);
            }
            Err(_) => return,
        }
    }
    for s in [Signal::SIGTSTP, Signal::SIGTTIN, Signal::SIGTTOU, Signal::SIGQUIT] {
        set_signal(s, SigHandler::SigIgn);
    }
    catch_signal(Signal::SIGINT);
    let me = unistd::getpid();
    if unistd::getpgrp() != me {
        let _ = unistd::setpgid(me, me);
    }
    sh.shell_pgid = unistd::getpgrp();
    let _ = unistd::tcsetpgrp(&stdin, sh.shell_pgid);
    sh.shell_tmodes = nix::sys::termios::tcgetattr(&stdin).ok();
    sh.job_control = true;
}

fn source_if_exists(sh: &mut Shell, path: &str) -> Result<(), i32> {
    if std::path::Path::new(path).is_file() {
        match builtins::source_file(sh, path, &[]) {
            Err(Flow::Exit(n)) => return Err(n),
            _ => {}
        }
    }
    Ok(())
}

fn finish(sh: &mut Shell, code: i32) -> ! {
    sh.run_exit_trap();
    exec::flush_stdout();
    std::process::exit(code & 0xff);
}

fn result_code(sh: &Shell, r: ExecResult) -> Result<i32, i32> {
    match r {
        Ok(s) => Ok(s),
        Err(Flow::Exit(n)) => Err(n),
        Err(Flow::Return(n)) => Ok(n),
        Err(Flow::Interrupt) => Ok(130),
        Err(_) => Ok(sh.last_status),
    }
}

fn main() {
    // Grande pile : scripts très imbriqués et fonctions récursives sans débordement
    let t = std::thread::Builder::new()
        .name("shsh".into())
        .stack_size(256 << 20)
        .spawn(real_main)
        .expect("impossible de démarrer le shell");
    let _ = t.join();
}

fn real_main() {
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let mut sh = Shell::new();
    let mut command: Option<String> = None;
    let mut force_interactive = false;
    let mut read_stdin = false;
    let mut norc = false;
    let mut i = 1;
    sh.login = args.first().is_some_and(|a| a.starts_with('-'));
    // $0 : « -shsh » pour un shell de connexion (comme « -bash »)
    if let Some(a0) = args.first() {
        let base = a0.rsplit('/').next().unwrap_or(a0);
        sh.arg0 = if sh.login && !base.starts_with('-') { format!("-{base}") } else { base.to_string() };
    }
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--version" | "-V" => {
                println!("shsh {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                usage();
                return;
            }
            "--norc" => norc = true,
            "--login" => sh.login = true,
            "--" => {
                i += 1;
                break;
            }
            "-c" => {
                i += 1;
                command = args.get(i).cloned();
                if command.is_none() {
                    eprintln!("shsh: -c : argument requis");
                    std::process::exit(2);
                }
                i += 1;
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => {
                for c in s[1..].chars() {
                    match c {
                        'i' => force_interactive = true,
                        'l' => sh.login = true,
                        's' => read_stdin = true,
                        'e' => sh.opts.errexit = true,
                        'u' => sh.opts.nounset = true,
                        'x' => sh.opts.xtrace = true,
                        'f' => sh.opts.noglob = true,
                        'n' => sh.noexec = true,
                        'c' => {}
                        other => {
                            eprintln!("shsh: option inconnue -{other}");
                            usage();
                            std::process::exit(2);
                        }
                    }
                }
                if s.contains('c') {
                    i += 1;
                    command = args.get(i).cloned();
                    i += 1;
                    break;
                }
            }
            _ => break,
        }
        i += 1;
    }
    let rest: Vec<String> = args.get(i..).map(|r| r.to_vec()).unwrap_or_default();

    set_signal(Signal::SIGPIPE, SigHandler::SigDfl);

    // Shell de connexion non interactif (ex. gnome-session : « -shsh -c 'exec …' ») :
    // l'environnement de /etc/profile et ~/.profile doit être chargé.
    let interactive_later = command.is_none() && (read_stdin || rest.is_empty());
    if sh.login && !interactive_later {
        let home = sh.home();
        for f in ["/etc/profile".to_string(), format!("{home}/.profile")] {
            if let Err(n) = source_if_exists(&mut sh, &f) {
                finish(&mut sh, n);
            }
        }
    }

    // --- shsh -c ---
    if let Some(cmd) = command {
        if let Some(name) = rest.first() {
            sh.arg0 = name.clone();
            sh.positional = rest[1..].to_vec();
        }
        sh.interactive = force_interactive;
        // shsh -ic '…' : comme bash et zsh, la configuration interactive est lue
        if force_interactive && !norc {
            let rc = format!("{}/.shshrc", sh.home());
            if let Err(n) = source_if_exists(&mut sh, &rc) {
                finish(&mut sh, n);
            }
        }
        let r = sh.run_source(&cmd, "");
        let code = result_code(&sh, r).unwrap_or_else(|n| n);
        finish(&mut sh, code);
    }

    // --- shsh script ---
    if !read_stdin && !rest.is_empty() {
        let path = rest[0].clone();
        sh.arg0 = path.clone();
        sh.positional = rest[1..].to_vec();
        let content = match std::fs::read(&path) {
            Ok(c) => String::from_utf8_lossy(&c).into_owned(),
            Err(e) => {
                let why = if e.kind() == std::io::ErrorKind::NotFound {
                    "fichier introuvable".to_string()
                } else {
                    e.to_string()
                };
                eprintln!("shsh: {path} : {why}");
                std::process::exit(127);
            }
        };
        let r = sh.run_source(&content, &path);
        let code = result_code(&sh, r).unwrap_or_else(|n| n);
        finish(&mut sh, code);
    }

    let interactive = force_interactive
        || (std::io::stdin().is_terminal() && std::io::stderr().is_terminal());
    sh.positional = rest.clone();

    if !interactive {
        let mut src = String::new();
        use std::io::Read;
        let _ = std::io::stdin().read_to_string(&mut src);
        let r = sh.run_source(&src, "");
        let code = result_code(&sh, r).unwrap_or_else(|n| n);
        finish(&mut sh, code);
    }

    // --- Mode interactif ---
    sh.interactive = true;
    init_job_control(&mut sh);
    let home = sh.home();
    if sh.login {
        if let Err(n) = source_if_exists(&mut sh, "/etc/profile") {
            finish(&mut sh, n);
        }
        if let Err(n) = source_if_exists(&mut sh, &format!("{home}/.profile")) {
            finish(&mut sh, n);
        }
    }
    if !norc {
        let rc = format!("{home}/.shshrc");
        if !std::path::Path::new(&rc).exists() {
            let _ = std::fs::write(&rc, DEFAULT_RC);
        }
        if let Err(n) = source_if_exists(&mut sh, &rc) {
            finish(&mut sh, n);
        }
    }
    INTERRUPTED.store(false, Ordering::SeqCst);

    let data = Arc::new(Mutex::new(editor::EdData::default()));
    editor::sync_data(&sh, &data);
    let mut ed = editor::build_editor(&mut sh, &data);
    let mut last_duration = None;
    let mut eof_warned = false;
    let mut first_prompt = true;
    // Ctrl+C au prompt : $? vaut 130 mais on ne l'affiche pas en erreur
    let mut quiet_status = false;

    let code = loop {
        // Tâches terminées en arrière-plan
        sh.jobs.reap();
        let done: Vec<(usize, String, String)> = sh
            .jobs
            .jobs
            .iter()
            .filter(|j| j.is_done() && j.background)
            .map(|j| (j.id, j.state_label(), j.text.clone()))
            .collect();
        for (id, label, text) in done {
            let m = sh.jobs.marker(id);
            eprintln!("[{id}]{m}  {label:<22} {text}");
            sh.jobs.remove(id);
        }
        INTERRUPTED.store(false, Ordering::SeqCst);
        if let Err(Flow::Exit(n)) = sh.run_pending_traps() {
            break n;
        }
        if let Some(f) = sh.functions.get("shsh_precmd").cloned() {
            let st = sh.last_status;
            if let Err(Flow::Exit(n)) = sh.call_function(&f, &["shsh_precmd".into()]) {
                break n;
            }
            sh.last_status = st;
        }
        // Shadow OS : mode du bureau et cible de mission, relus à chaque prompt
        let shadow = shadow::lire(&sh);
        shadow::synchroniser(&mut sh, &shadow);
        let cwd = sh.cwd();
        // OSC 7 (nouvel onglet dans le même dossier) + titre de la fenêtre
        let host = nix::unistd::gethostname()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        let title = builtins::tilde_path(&sh, &cwd);
        eprint!("\x1b]7;file://{host}{}\x1b\\\x1b]0;{title}\x07", url_path(&cwd));

        editor::sync_data(&sh, &data);
        if sh.abbrs_changed {
            drop(ed);
            ed = editor::build_editor(&mut sh, &data);
        }
        let user = sh.get_var("USER").unwrap_or_default();
        let info = prompt::PromptInfo {
            cwd: cwd.clone(),
            home: sh.home(),
            status: if quiet_status { 0 } else { sh.last_status },
            duration: last_duration,
            jobs: sh.jobs.jobs.len(),
            user,
            host: host.split('.').next().unwrap_or("").to_string(),
            ssh: sh.get_var("SSH_CONNECTION").is_some() || sh.get_var("SSH_TTY").is_some(),
            root: nix::unistd::geteuid().is_root(),
            venv: sh
                .get_var("VIRTUAL_ENV")
                .and_then(|v| v.rsplit('/').next().map(String::from))
                .or_else(|| sh.get_var("CONDA_DEFAULT_ENV").filter(|e| e != "base")),
            shlvl: sh.get_var("SHLVL").and_then(|s| s.parse().ok()).unwrap_or(1),
            mode: shadow.mode.clone(),
            cible: shadow.cible.as_ref().map(|c| (c.adresse.clone(), c.nom.clone())),
        };
        let p = prompt::ShshPrompt::build(&info);
        // Une ligne d'air entre la sortie précédente et le prompt
        if !first_prompt {
            eprintln!();
        }
        first_prompt = false;
        let sig = match ed.read_line(&p) {
            Ok(s) => s,
            Err(e) => {
                sh.error(format!("erreur du terminal : {e}"));
                break 1;
            }
        };
        match sig {
            RlSignal::Success(line) => {
                eof_warned = false;
                quiet_status = false;
                if line.trim().is_empty() {
                    last_duration = None;
                    continue;
                }
                // La mission a pu changer dans un autre terminal depuis l'affichage du prompt
                let shadow = shadow::lire(&sh);
                shadow::synchroniser(&mut sh, &shadow);
                if let Some(f) = sh.functions.get("shsh_preexec").cloned() {
                    let _ = sh.call_function(&f, &["shsh_preexec".into(), line.clone()]);
                }
                let first = line.split_whitespace().next().unwrap_or("").to_string();
                eprint!("\x1b]0;{first} · {title}\x07");
                let t0 = Instant::now();
                INTERRUPTED.store(false, Ordering::SeqCst);
                if first != "exit" {
                    sh.vars.remove("__SHSH_EXIT_WARNED");
                }
                let r = sh.run_source(&line, "");
                last_duration = Some(t0.elapsed());
                match result_code(&sh, r) {
                    Ok(s) => sh.last_status = s,
                    Err(n) => break n,
                }
                // Garde le terminal propre si un programme l'a laissé dans un état bizarre
                sh.reclaim_terminal(None);
            }
            RlSignal::CtrlC(_) => {
                sh.last_status = 130;
                quiet_status = true;
                last_duration = None;
            }
            RlSignal::CtrlD => {
                let stopped = sh.jobs.jobs.iter().filter(|j| j.is_stopped()).count();
                if stopped > 0 && !eof_warned {
                    sh.error(format!(
                        "il reste {stopped} tâche(s) stoppée(s) — Ctrl+D à nouveau pour quitter"
                    ));
                    eof_warned = true;
                    continue;
                }
                eprintln!("exit");
                break sh.last_status;
            }
            _ => {}
        }
    };
    drop(ed);
    // Les tâches stoppées reçoivent SIGHUP puis SIGCONT (comme bash)
    for j in sh.jobs.jobs.iter().filter(|j| j.is_stopped()) {
        let _ = nix::sys::signal::killpg(j.pgid, Signal::SIGHUP);
        let _ = nix::sys::signal::killpg(j.pgid, Signal::SIGCONT);
    }
    finish(&mut sh, code);
}

fn url_path(p: &str) -> String {
    let mut o = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}
