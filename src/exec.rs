//! Exécution de l'AST : listes, pipelines, commandes, redirections, tâches.

use crate::ast::*;
use crate::builtins;
use crate::parser::{ParseError, Parser};
use crate::shell::*;
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{self, ForkResult, Pid};
use std::ffi::CString;
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::Ordering;

pub fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

/// Écriture directe sur un descripteur (respecte les redirections dup2).
pub fn write_fd(fd: i32, s: &str) -> bool {
    let mut b = s.as_bytes();
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const libc::c_void, b.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        b = &b[n as usize..];
    }
    true
}

fn close_fd(fd: i32) {
    unsafe {
        libc::close(fd);
    }
}

fn dup2(a: i32, b: i32) -> bool {
    unsafe { libc::dup2(a, b) >= 0 }
}

fn make_pipe() -> Result<(i32, i32), String> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(format!("pipe : {}", std::io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

/// Quitte un processus enfant sans lancer les destructeurs du parent.
pub fn child_exit(code: i32) -> ! {
    flush_stdout();
    let _ = std::io::stderr().flush();
    unsafe { libc::_exit(code & 0xff) }
}

impl Shell {
    // ------------------------------------------------------------------
    // Points d'entrée
    // ------------------------------------------------------------------

    /// Analyse et exécute un texte source, commande par commande.
    pub fn run_source(&mut self, src: &str, origin: &str) -> ExecResult {
        let mut p = Parser::with_aliases(src, self.aliases.clone());
        let mut status = 0;
        loop {
            p.set_aliases(self.aliases.clone());
            let item = match p.parse_one() {
                Ok(Some(list)) => list,
                Ok(None) => break,
                Err(e) => {
                    self.report_parse_error(&e, src, origin);
                    self.last_status = 2;
                    if !self.interactive || self.source_depth > 0 {
                        return Ok(2);
                    }
                    return Ok(2);
                }
            };
            if self.noexec {
                continue;
            }
            status = self.exec_list(&item)?;
        }
        Ok(status)
    }

    pub fn report_parse_error(&self, e: &ParseError, src: &str, origin: &str) {
        let line = src
            .char_indices()
            .take_while(|(i, _)| *i < byte_of(src, e.pos))
            .filter(|(_, c)| *c == '\n')
            .count()
            + 1;
        if origin.is_empty() {
            self.error(format!("erreur de syntaxe : {}", e.msg));
        } else {
            self.error(format!("{origin}:{line} : erreur de syntaxe : {}", e.msg));
        }
    }

    pub fn exec_list(&mut self, list: &List) -> ExecResult {
        let mut status = self.last_status;
        for item in list {
            self.check_interrupt()?;
            if item.background {
                status = self.exec_background(&item.and_or)?;
            } else {
                status = self.exec_and_or(&item.and_or)?;
            }
            self.last_status = status;
            self.run_pending_traps()?;
        }
        Ok(status)
    }

    pub fn check_interrupt(&mut self) -> Result<(), Flow> {
        if INTERRUPTED.load(Ordering::SeqCst) && !self.traps.contains_key(&libc::SIGINT) {
            return Err(Flow::Interrupt);
        }
        Ok(())
    }

    fn exec_and_or(&mut self, ao: &AndOr) -> ExecResult {
        let has_rest = !ao.rest.is_empty();
        if has_rest {
            self.cond_depth += 1;
        }
        let mut status = self.exec_pipeline(&ao.first, has_rest);
        if has_rest {
            self.cond_depth -= 1;
        }
        let mut st = status?;
        for (i, (is_and, p)) in ao.rest.iter().enumerate() {
            self.last_status = st;
            if (*is_and && st != 0) || (!*is_and && st == 0) {
                continue;
            }
            let last = i == ao.rest.len() - 1;
            if !last {
                self.cond_depth += 1;
            }
            status = self.exec_pipeline(p, !last);
            if !last {
                self.cond_depth -= 1;
            }
            st = status?;
        }
        Ok(st)
    }

    /// trap '…' ERR : lancé dans les mêmes conditions que set -e.
    fn run_err_trap(&mut self, status: i32) -> Result<(), Flow> {
        if status == 0 || self.cond_depth != 0 || self.in_trap {
            return Ok(());
        }
        let Some(cmd) = self.traps.get(&crate::shell::TRAP_ERR).cloned() else { return Ok(()) };
        if cmd.is_empty() {
            return Ok(());
        }
        self.in_trap = true;
        let r = self.run_source(&cmd, "");
        self.in_trap = false;
        self.last_status = status;
        if let Err(Flow::Exit(n)) = r {
            return Err(Flow::Exit(n));
        }
        Ok(())
    }

    fn errexit_check(&mut self, status: i32) -> Result<(), Flow> {
        if self.opts.errexit && status != 0 && self.cond_depth == 0 {
            return Err(Flow::Exit(status));
        }
        Ok(())
    }

    fn exec_pipeline(&mut self, p: &Pipeline, in_cond: bool) -> ExecResult {
        if p.timed {
            let t0 = std::time::Instant::now();
            let r0 = rusage_total();
            let mut q = p.clone();
            q.timed = false;
            let r = self.exec_pipeline(&q, in_cond);
            let r1 = rusage_total();
            let fmt = |s: f64| format!("{}m{:.3}s", (s / 60.0) as u64, s % 60.0);
            flush_stdout();
            eprintln!(
                "\nreal\t{}\nuser\t{}\nsys\t{}",
                fmt(t0.elapsed().as_secs_f64()),
                fmt(r1.0 - r0.0),
                fmt(r1.1 - r0.1)
            );
            return r;
        }
        if p.negate {
            self.cond_depth += 1;
        }
        let r = if p.cmds.len() == 1 {
            self.exec_command(&p.cmds[0], false, &p.text)
        } else {
            self.exec_multi(p, false)
        };
        if p.negate {
            self.cond_depth -= 1;
        }
        let st = r?;
        let st = if p.negate { (st == 0) as i32 } else { st };
        self.last_status = st;
        if !in_cond && !p.negate {
            self.run_err_trap(st)?;
            self.errexit_check(st)?;
        }
        Ok(st)
    }

    fn exec_background(&mut self, ao: &AndOr) -> ExecResult {
        let text = and_or_text(ao);
        let ao2 = ao.clone();
        let pid = self.fork_child(None, false, move |sh| {
            if !sh.job_control {
                set_signal(Signal::SIGINT, SigHandler::SigIgn);
                set_signal(Signal::SIGQUIT, SigHandler::SigIgn);
            }
            match sh.exec_and_or(&ao2) {
                Ok(s) => s,
                Err(Flow::Exit(s)) | Err(Flow::Return(s)) => s,
                Err(_) => sh.last_status,
            }
        })?;
        let id = self.jobs.add(pid, vec![pid], format!("{text} &"), true);
        self.last_bg_pid = Some(pid.as_raw());
        if self.interactive {
            eprintln!("[{id}] {}", pid.as_raw());
        }
        Ok(0)
    }

    /// Pipeline à plusieurs commandes : chaque étage dans un processus.
    fn exec_multi(&mut self, p: &Pipeline, background: bool) -> ExecResult {
        let n = p.cmds.len();
        let mut pids: Vec<Pid> = Vec::new();
        let mut pgid: Option<Pid> = None;
        let mut prev_read: i32 = -1;
        for (i, cmd) in p.cmds.iter().enumerate() {
            let (r, w) = if i + 1 < n {
                match make_pipe() {
                    Ok(x) => x,
                    Err(e) => {
                        self.error(e);
                        return Ok(1);
                    }
                }
            } else {
                (-1, -1)
            };
            let cmd2 = cmd.clone();
            let text = p.text.clone();
            let pr = prev_read;
            let pid = self.fork_child(pgid, !background, move |sh| {
                if pr >= 0 {
                    dup2(pr, 0);
                    close_fd(pr);
                }
                if w >= 0 {
                    dup2(w, 1);
                    close_fd(w);
                }
                if r >= 0 {
                    close_fd(r);
                }
                match sh.exec_command(&cmd2, true, &text) {
                    Ok(s) => s,
                    Err(Flow::Exit(s)) | Err(Flow::Return(s)) => s,
                    Err(_) => sh.last_status,
                }
            });
            if prev_read >= 0 {
                close_fd(prev_read);
            }
            if w >= 0 {
                close_fd(w);
            }
            let pid = match pid {
                Ok(pid) => pid,
                Err(e) => {
                    if r >= 0 {
                        close_fd(r);
                    }
                    let _ = e;
                    return Ok(1);
                }
            };
            if pgid.is_none() {
                pgid = Some(pid);
            }
            pids.push(pid);
            prev_read = r;
        }
        let pg = pgid.unwrap();
        let id = self.jobs.add(pg, pids, p.text.clone(), background);
        if background {
            return Ok(0);
        }
        Ok(self.wait_fg(id))
    }

    /// Fork : l'enfant exécute `f` puis se termine. Gère groupes de processus et terminal.
    pub fn fork_child<F>(&mut self, pgid: Option<Pid>, foreground: bool, f: F) -> Result<Pid, Flow>
    where
        F: FnOnce(&mut Shell) -> i32,
    {
        flush_stdout();
        match unsafe { unistd::fork() } {
            Err(e) => {
                self.error(format!("fork : {e}"));
                Err(Flow::Exit(1))
            }
            Ok(ForkResult::Child) => {
                self.child_setup(pgid, foreground);
                let st = f(self);
                self.run_exit_trap();
                child_exit(st);
            }
            Ok(ForkResult::Parent { child }) => {
                if self.job_control {
                    let pg = pgid.unwrap_or(child);
                    let _ = unistd::setpgid(child, pg);
                }
                Ok(child)
            }
        }
    }

    fn child_setup(&mut self, pgid: Option<Pid>, foreground: bool) {
        if self.job_control {
            let me = unistd::getpid();
            let pg = pgid.unwrap_or(me);
            let _ = unistd::setpgid(me, pg);
            if foreground {
                let _ = unistd::tcsetpgrp(std::io::stdin(), pg);
            }
            for s in [
                Signal::SIGTSTP,
                Signal::SIGTTIN,
                Signal::SIGTTOU,
                Signal::SIGQUIT,
            ] {
                set_signal(s, SigHandler::SigDfl);
            }
        }
        if self.interactive {
            set_signal(Signal::SIGINT, SigHandler::SigDfl);
        }
        set_signal(Signal::SIGPIPE, SigHandler::SigDfl);
        set_signal(Signal::SIGCHLD, SigHandler::SigDfl);
        // Pièges : réinitialisés dans un sous-shell (sauf les signaux ignorés)
        self.traps.retain(|_, v| v.is_empty());
        for sig in 1..65 {
            PENDING[sig].store(false, Ordering::SeqCst);
        }
        INTERRUPTED.store(false, Ordering::SeqCst);
        self.job_control = false;
        self.interactive = false;
        self.is_subshell = true;
        self.jobs.clear();
    }

    /// Attend une tâche au premier plan ; renvoie son statut.
    pub fn wait_fg(&mut self, id: usize) -> i32 {
        loop {
            let (done, stopped) = match self.jobs.get(id) {
                Some(j) => (j.is_done(), j.is_stopped()),
                None => (true, false),
            };
            if done || stopped {
                break;
            }
            // Sans contrôle des tâches, un enfant suspendu n'est pas une fin d'attente
            let flags = if self.job_control { Some(WaitPidFlag::WUNTRACED) } else { None };
            match waitpid(Pid::from_raw(-1), flags) {
                Ok(st) => {
                    self.jobs.update(st);
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => {
                    // Plus d'enfants : tout est terminé
                    if let Some(j) = self.jobs.get_mut(id) {
                        for p in j.procs.iter_mut() {
                            if !matches!(p.state, crate::jobs::ProcState::Done(..)) {
                                p.state = crate::jobs::ProcState::Done(0, None);
                            }
                        }
                    }
                    break;
                }
            }
        }
        self.reclaim_terminal(Some(id));
        let job = match self.jobs.get(id) {
            Some(j) => j.clone(),
            None => return 0,
        };
        if job.is_stopped() {
            if let Some(j) = self.jobs.get_mut(id) {
                j.background = true;
            }
            self.jobs.touch(id);
            eprintln!("\n[{id}]+  Stoppé                  {}", job.text);
            return 128 + libc::SIGTSTP;
        }
        let st = job.status(self.opts.pipefail);
        self.jobs.remove(id);
        if let Some(sig) = job.signal() {
            if sig == libc::SIGINT {
                INTERRUPTED.store(true, Ordering::SeqCst);
                if self.interactive {
                    eprintln!();
                }
            } else if sig != libc::SIGPIPE && (self.interactive || !self.is_subshell) {
                let core = if matches!(sig, libc::SIGSEGV | libc::SIGABRT | libc::SIGBUS | libc::SIGFPE | libc::SIGQUIT) {
                    " (core dumped)"
                } else {
                    ""
                };
                eprintln!("{}{core}", signal_message(sig));
            }
        }
        st
    }

    /// Reprend le terminal pour le shell après une tâche au premier plan.
    pub fn reclaim_terminal(&mut self, id: Option<usize>) {
        if !self.job_control {
            return;
        }
        let _ = unistd::tcsetpgrp(std::io::stdin(), self.shell_pgid);
        if let Some(id) = id {
            if let Ok(t) = nix::sys::termios::tcgetattr(std::io::stdin()) {
                if let Some(j) = self.jobs.get_mut(id) {
                    j.tmodes = Some(t);
                }
            }
        }
        if let Some(t) = &self.shell_tmodes {
            let _ = nix::sys::termios::tcsetattr(
                std::io::stdin(),
                nix::sys::termios::SetArg::TCSADRAIN,
                t,
            );
        }
    }

    // ------------------------------------------------------------------
    // Commandes
    // ------------------------------------------------------------------

    /// `forked` : déjà dans un processus enfant (étage de pipeline) — exec direct.
    pub fn exec_command(&mut self, cmd: &Command, forked: bool, text: &str) -> ExecResult {
        match cmd {
            Command::Simple(s) => self.exec_simple(s, forked, text),
            Command::FuncDef(name, body) => {
                self.functions.insert(name.clone(), body.clone());
                Ok(0)
            }
            Command::Compound(c, redirs) => {
                if let Compound::Subshell(list) = &**c {
                    if !forked {
                        let list = list.clone();
                        let redirs = redirs.clone();
                        let pid = self.fork_child(None, true, move |sh| {
                            if let Err(e) = sh.apply_redirs(&redirs, false) {
                                sh.error(e);
                                return 1;
                            }
                            match sh.exec_list(&list) {
                                Ok(s) => s,
                                Err(Flow::Exit(s)) | Err(Flow::Return(s)) => s,
                                Err(_) => sh.last_status,
                            }
                        })?;
                        let id = self.jobs.add(pid, vec![pid], text.to_string(), false);
                        return Ok(self.wait_fg(id));
                    }
                }
                let saved = match self.apply_redirs(redirs, true) {
                    Ok(s) => s,
                    Err(e) => {
                        self.error(e);
                        return Ok(1);
                    }
                };
                let r = self.exec_compound(c);
                self.restore_redirs(saved);
                r
            }
        }
    }

    pub fn exec_compound(&mut self, c: &Compound) -> ExecResult {
        match c {
            Compound::Brace(list) | Compound::Subshell(list) => self.exec_list(list),
            Compound::If {
                branches,
                else_body,
            } => {
                for (cond, body) in branches {
                    self.cond_depth += 1;
                    let r = self.exec_list(cond);
                    self.cond_depth -= 1;
                    if r? == 0 {
                        return self.exec_list(body);
                    }
                }
                match else_body {
                    Some(b) => self.exec_list(b),
                    None => Ok(0),
                }
            }
            Compound::Loop { until, cond, body } => {
                let mut status = 0;
                self.loop_depth += 1;
                let res = loop {
                    if let Err(f) = self.check_interrupt() {
                        break Err(f);
                    }
                    self.cond_depth += 1;
                    let r = self.exec_list(cond);
                    self.cond_depth -= 1;
                    let c = match r {
                        Ok(c) => c,
                        Err(f) => break Err(f),
                    };
                    if (c == 0) == *until {
                        break Ok(status);
                    }
                    match self.exec_list(body) {
                        Ok(s) => status = s,
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                break Err(Flow::Break(n - 1));
                            }
                            break Ok(0);
                        }
                        Err(Flow::Continue(n)) => {
                            if n > 1 {
                                break Err(Flow::Continue(n - 1));
                            }
                        }
                        Err(f) => break Err(f),
                    }
                };
                self.loop_depth -= 1;
                res
            }
            Compound::For { var, items, body } => {
                let values = match items {
                    Some(ws) => match self.expand_words(ws) {
                        Ok(v) => v,
                        Err(e) => {
                            self.error(e);
                            return Ok(1);
                        }
                    },
                    None => self.positional.clone(),
                };
                let mut status = 0;
                self.loop_depth += 1;
                let mut res = Ok(0);
                for v in values {
                    if let Err(f) = self.check_interrupt() {
                        res = Err(f);
                        break;
                    }
                    if let Err(e) = self.set_var(var, v) {
                        self.error(e);
                        res = Ok(1);
                        break;
                    }
                    match self.exec_list(body) {
                        Ok(s) => status = s,
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                res = Err(Flow::Break(n - 1));
                            }
                            break;
                        }
                        Err(Flow::Continue(n)) => {
                            if n > 1 {
                                res = Err(Flow::Continue(n - 1));
                                break;
                            }
                        }
                        Err(f) => {
                            res = Err(f);
                            break;
                        }
                    }
                    res = Ok(status);
                }
                self.loop_depth -= 1;
                res
            }
            Compound::ArithFor {
                init,
                cond,
                step,
                body,
            } => {
                if let Err(e) = self.eval_arith_parts(init) {
                    self.error(e);
                    return Ok(1);
                }
                let mut status = 0;
                self.loop_depth += 1;
                let res = loop {
                    if let Err(f) = self.check_interrupt() {
                        break Err(f);
                    }
                    let c = if cond.is_empty() {
                        1
                    } else {
                        match self.eval_arith_parts(cond) {
                            Ok(v) => v,
                            Err(e) => {
                                self.error(e);
                                break Ok(1);
                            }
                        }
                    };
                    if c == 0 {
                        break Ok(status);
                    }
                    match self.exec_list(body) {
                        Ok(s) => status = s,
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                break Err(Flow::Break(n - 1));
                            }
                            break Ok(0);
                        }
                        Err(Flow::Continue(n)) => {
                            if n > 1 {
                                break Err(Flow::Continue(n - 1));
                            }
                        }
                        Err(f) => break Err(f),
                    }
                    if let Err(e) = self.eval_arith_parts(step) {
                        self.error(e);
                        break Ok(1);
                    }
                };
                self.loop_depth -= 1;
                res
            }
            Compound::Case { word, arms } => {
                let w = match self.expand_str(word) {
                    Ok(w) => w,
                    Err(e) => {
                        self.error(e);
                        return Ok(1);
                    }
                };
                let mut i = 0;
                let mut matched = false;
                let mut dernier = 0;
                while i < arms.len() {
                    let arm = &arms[i];
                    if !matched {
                        for p in &arm.patterns {
                            let pat = match self.expand_pattern(p) {
                                Ok(p) => p,
                                Err(e) => {
                                    self.error(e);
                                    return Ok(1);
                                }
                            };
                            if self.pattern_match(&pat, &w) {
                                matched = true;
                                break;
                            }
                        }
                    }
                    if matched {
                        dernier = self.exec_list(&arm.body)?;
                        if arm.retest {
                            // ;;& : on reprend la comparaison aux branches suivantes
                            matched = false;
                        } else if !arm.fallthrough {
                            return Ok(dernier);
                        }
                    }
                    i += 1;
                }
                Ok(dernier)
            }
            Compound::Arith(parts) => match self.eval_arith_parts(parts) {
                Ok(v) => Ok((v == 0) as i32),
                Err(e) => {
                    self.error(e);
                    Ok(1)
                }
            },
            Compound::Cond(e) => match self.eval_cond(e) {
                Ok(b) => Ok((!b) as i32),
                Err(e) => {
                    self.error(e);
                    Ok(2)
                }
            },
        }
    }

    pub fn eval_arith_parts(&mut self, parts: &[Part]) -> Result<i64, String> {
        let s = self.expand_str(&vec![Part::DQuoted(parts.to_vec())])?;
        crate::arith::eval(&s, self)
    }

    fn eval_cond(&mut self, e: &CondExpr) -> Result<bool, String> {
        match e {
            CondExpr::And(a, b) => Ok(self.eval_cond(a)? && self.eval_cond(b)?),
            CondExpr::Or(a, b) => Ok(self.eval_cond(a)? || self.eval_cond(b)?),
            CondExpr::Not(a) => Ok(!self.eval_cond(a)?),
            CondExpr::Single(w) => Ok(!self.expand_str(w)?.is_empty()),
            CondExpr::Unary(op, w) => {
                let a = self.expand_str(w)?;
                builtins::test_unary(self, op, &a)
            }
            CondExpr::Binary(l, op, r) => {
                let a = self.expand_str(l)?;
                match op.as_str() {
                    "==" | "=" | "!=" => {
                        let pat = self.expand_pattern(r)?;
                        let m = self.pattern_match(&pat, &a);
                        Ok(if op == "!=" { !m } else { m })
                    }
                    "=~" => {
                        let re = self.expand_regex(r)?;
                        let rx = regex::Regex::new(&re)
                            .map_err(|e| format!("expression régulière invalide : {e}"))?;
                        match rx.captures(&a) {
                            Some(caps) => {
                                // SHSH_REMATCH0, SHSH_REMATCH1… (pas de tableaux)
                                for (i, c) in caps.iter().enumerate() {
                                    let _ = self.set_var(
                                        &format!("SHSH_REMATCH{i}"),
                                        c.map(|m| m.as_str().to_string()).unwrap_or_default(),
                                    );
                                }
                                let groupes: Vec<String> = caps
                                    .iter()
                                    .map(|c| c.map(|m| m.as_str().to_string()).unwrap_or_default())
                                    .collect();
                                let _ = self.set_array("BASH_REMATCH", groupes);
                                Ok(true)
                            }
                            None => Ok(false),
                        }
                    }
                    "<" => Ok(a < self.expand_str(r)?),
                    ">" => Ok(a > self.expand_str(r)?),
                    _ => {
                        let b = self.expand_str(r)?;
                        let (x, y) = if op.starts_with("-e") || op.starts_with("-n") || op.starts_with("-l") || op.starts_with("-g") {
                            if matches!(op.as_str(), "-ef" | "-nt") {
                                (0, 0)
                            } else {
                                (
                                    crate::arith::eval(&a, self)?,
                                    crate::arith::eval(&b, self)?,
                                )
                            }
                        } else {
                            (0, 0)
                        };
                        match op.as_str() {
                            "-eq" => Ok(x == y),
                            "-ne" => Ok(x != y),
                            "-lt" => Ok(x < y),
                            "-le" => Ok(x <= y),
                            "-gt" => Ok(x > y),
                            "-ge" => Ok(x >= y),
                            _ => builtins::test_binary(&a, op, &b),
                        }
                    }
                }
            }
        }
    }

    fn exec_simple(&mut self, cmd: &SimpleCommand, forked: bool, text: &str) -> ExecResult {
        self.cmdsub_status = None;
        let argv = match self.expand_words(&cmd.words) {
            Ok(v) => v,
            Err(e) => {
                self.error(e);
                self.close_procsubs();
                if !self.interactive {
                    return Err(Flow::Exit(1));
                }
                return Ok(1);
            }
        };
        // Affectations seules
        if argv.is_empty() {
            let mut st = 0;
            for a in &cmd.assigns {
                if let Err(e) = self.do_assign(a) {
                    self.error(e);
                    st = 1;
                    if !self.interactive {
                        return Err(Flow::Exit(1));
                    }
                }
            }
            if !cmd.redirs.is_empty() {
                match self.apply_redirs(&cmd.redirs, true) {
                    Ok(saved) => self.restore_redirs(saved),
                    Err(e) => {
                        self.error(e);
                        return Ok(1);
                    }
                }
            }
            return Ok(if st != 0 { st } else { self.cmdsub_status.unwrap_or(0) });
        }

        let mut assigns: Vec<(String, String)> = Vec::new();
        for a in &cmd.assigns {
            if a.array.is_some() || a.index.is_some() {
                if let Err(e) = self.do_assign(a) {
                    self.error(e);
                    return Ok(1);
                }
                continue;
            }
            match self.expand_str(&a.value) {
                Ok(mut v) => {
                    if a.append {
                        v = match self.append_value(&a.name, v) {
                            Ok(v) => v,
                            Err(e) => {
                                self.error(e);
                                return Ok(1);
                            }
                        };
                    }
                    assigns.push((a.name.clone(), v));
                }
                Err(e) => {
                    self.error(e);
                    return Ok(1);
                }
            }
        }
        if self.opts.xtrace {
            let mut t: Vec<String> = assigns.iter().map(|(k, v)| format!("{k}={v}")).collect();
            t.extend(argv.iter().cloned());
            self.xtrace(&t);
        }
        let r = self.dispatch(argv, assigns, &cmd.redirs, forked, text);
        self.close_procsubs();
        r
    }

    /// Affectation : scalaire, élément (a[i]=v) ou tableau (a=(…)), avec +=.
    pub fn do_assign(&mut self, a: &Assign) -> Result<(), String> {
        if let Some(items) = &a.array {
            let mut vals = self.expand_words(items)?;
            if self.opts.xtrace {
                self.xtrace(&[format!("{}=({})", a.name, vals.join(" "))]);
            }
            return self.assign_compound(&a.name, a.append, std::mem::take(&mut vals));
        }
        let mut v = self.expand_str(&a.value)?;
        if let Some(idx) = &a.index {
            let e = self.expand_str(&vec![Part::DQuoted(idx.clone())])?;
            if self.is_assoc(&a.name) {
                if a.append {
                    v = self.assoc_get(&a.name, &e).unwrap_or_default() + &v;
                }
                if self.opts.xtrace {
                    self.xtrace(&[format!("{}[{e}]={v}", a.name)]);
                }
                return self.assoc_set(&a.name, &e, v);
            }
            let i = crate::arith::eval(&e, self)?;
            if a.append {
                if let Some(old) = self.get_elem(&a.name, i) {
                    v = format!("{old}{v}");
                }
            }
            if self.opts.xtrace {
                self.xtrace(&[format!("{}[{i}]={v}", a.name)]);
            }
            return self.set_elem(&a.name, i, v);
        }
        if a.append {
            v = self.append_value(&a.name, v)?;
        }
        if self.opts.xtrace {
            self.xtrace(&[format!("{}={}", a.name, v)]);
        }
        self.set_var(&a.name, v)
    }

    /// Valeur après `nom+=v` : somme pour un entier (declare -i), concaténation sinon.
    fn append_value(&mut self, name: &str, v: String) -> Result<String, String> {
        let old = self.get_var(name).unwrap_or_default();
        if self.vars.get(name).is_some_and(|x| x.integer) {
            let a = crate::arith::eval(&old, self)?;
            let b = crate::arith::eval(&v, self)?;
            return Ok((a + b).to_string());
        }
        Ok(old + &v)
    }

    fn close_procsubs(&mut self) {
        for fd in std::mem::take(&mut self.procsub_fds) {
            close_fd(fd);
        }
    }

    fn xtrace(&self, words: &[String]) {
        let ps4 = self.get_var("PS4").unwrap_or_else(|| "+ ".into());
        let parts: Vec<String> = words.iter().map(|w| quote_if_needed(w)).collect();
        eprintln!("{ps4}{}", parts.join(" "));
    }

    /// Fonction, builtin ou programme externe.
    pub fn dispatch(
        &mut self,
        mut argv: Vec<String>,
        assigns: Vec<(String, String)>,
        redirs: &[Redir],
        forked: bool,
        text: &str,
    ) -> ExecResult {
        let mut skip_funcs = false;
        let mut only_builtin = false;
        // command / builtin : contournent fonctions (et alias, déjà faits)
        loop {
            match argv[0].as_str() {
                "command" if argv.len() > 1 && !argv[1].starts_with('-') => {
                    argv.remove(0);
                    skip_funcs = true;
                }
                "builtin" if argv.len() > 1 => {
                    argv.remove(0);
                    only_builtin = true;
                    skip_funcs = true;
                }
                _ => break,
            }
        }
        let name = argv[0].clone();

        if !skip_funcs {
            if let Some(f) = self.functions.get(&name).cloned() {
                let saved_vars = self.push_temp_assigns(&assigns);
                let saved = match self.apply_redirs(redirs, true) {
                    Ok(s) => s,
                    Err(e) => {
                        self.pop_temp_assigns(saved_vars);
                        self.error(e);
                        return Ok(1);
                    }
                };
                let r = self.call_function(&f, &argv);
                self.restore_redirs(saved);
                self.pop_temp_assigns(saved_vars);
                return r;
            }
        }
        if name == "exec" {
            return self.builtin_exec(&argv[1..], assigns, redirs);
        }
        if let Some(b) = builtins::lookup(&name) {
            let special = builtins::is_special(&name);
            let saved_vars = if special {
                for (k, v) in &assigns {
                    let _ = self.set_var(k, v.clone());
                }
                Vec::new()
            } else {
                self.push_temp_assigns(&assigns)
            };
            let saved = match self.apply_redirs(redirs, true) {
                Ok(s) => s,
                Err(e) => {
                    self.pop_temp_assigns(saved_vars);
                    self.error(e);
                    return Ok(1);
                }
            };
            let r = b(self, &argv);
            flush_stdout();
            self.restore_redirs(saved);
            self.pop_temp_assigns(saved_vars);
            return r;
        }
        if only_builtin {
            self.error(format!("builtin : {name} n'est pas une commande interne"));
            return Ok(1);
        }

        // Programme externe
        let path = match self.find_in_path(&name) {
            Some(p) => p,
            None => {
                // Les redirections s'appliquent aussi au message d'erreur (cmd 2>/dev/null)
                let saved = match self.apply_redirs(redirs, true) {
                    Ok(s) => s,
                    Err(e) => {
                        self.error(e);
                        return Ok(1);
                    }
                };
                let r = self.command_not_found(&argv, redirs, forked);
                self.restore_redirs(saved);
                return r;
            }
        };
        let env = self.env_list(&assigns);
        if forked {
            if let Err(e) = self.apply_redirs(redirs, false) {
                self.error(e);
                return Ok(1);
            }
            self.do_exec(&path, &argv, &env);
        }
        let redirs2 = redirs.to_vec();
        let pid = self.fork_child(None, true, move |sh| {
            if let Err(e) = sh.apply_redirs(&redirs2, false) {
                sh.error(e);
                return 1;
            }
            sh.do_exec(&path, &argv, &env);
        })?;
        let id = self.jobs.add(pid, vec![pid], text.to_string(), false);
        Ok(self.wait_fg(id))
    }

    fn command_not_found(&mut self, argv: &[String], redirs: &[Redir], _forked: bool) -> ExecResult {
        let name = &argv[0];
        // auto-cd : taper un nom de dossier suffit pour y aller
        if self.interactive && argv.len() == 1 && redirs.is_empty() && !self.is_subshell {
            let p = std::path::Path::new(name);
            if p.is_dir() {
                return builtins::run(self, &["cd".into(), name.clone()]);
            }
        }
        if name.contains('/') {
            let p = std::path::Path::new(name);
            if p.is_dir() {
                self.error(format!("{name} : est un dossier"));
                return Ok(126);
            }
            if p.exists() {
                self.error(format!("{name} : permission refusée"));
                return Ok(126);
            }
            self.error(format!("{name} : fichier ou dossier introuvable"));
            return Ok(127);
        }
        // Fonction de rattrapage définie par l'utilisateur
        if let Some(f) = self.functions.get("command_not_found_handle").cloned() {
            return self.call_function(&f, argv);
        }
        let mut msg = format!("commande introuvable : {name}");
        if self.interactive {
            if let Some(s) = crate::editor::suggest_command(self, name) {
                msg.push_str(&format!("\n      vouliez-vous dire \x1b[1;38;2;124;108;255m{s}\x1b[0m ?"));
            }
        }
        self.error(msg);
        Ok(127)
    }

    pub fn do_exec(&self, path: &str, argv: &[String], env: &[CString]) -> ! {
        let cpath = CString::new(path).unwrap_or_default();
        let cargs: Vec<CString> = argv
            .iter()
            .map(|a| CString::new(a.as_str()).unwrap_or_default())
            .collect();
        flush_stdout();
        let e = unistd::execve(&cpath, &cargs, env).unwrap_err();
        if e == nix::errno::Errno::ENOEXEC {
            // Script sans #! : exécuté par shsh
            let me = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/bin/sh".into());
            let mut a = vec![CString::new(me.clone()).unwrap(), cpath.clone()];
            a.extend(cargs.into_iter().skip(1));
            let _ = unistd::execve(&CString::new(me).unwrap(), &a, env);
        }
        let msg = match e {
            nix::errno::Errno::EACCES => "permission refusée".to_string(),
            nix::errno::Errno::ENOENT => "fichier ou dossier introuvable (interpréteur ?)".to_string(),
            other => other.desc().to_string(),
        };
        eprintln!("shsh: {} : {msg}", argv[0]);
        child_exit(if e == nix::errno::Errno::ENOENT { 127 } else { 126 });
    }

    fn builtin_exec(
        &mut self,
        args: &[String],
        assigns: Vec<(String, String)>,
        redirs: &[Redir],
    ) -> ExecResult {
        if let Err(e) = self.apply_redirs(redirs, false) {
            self.error(e);
            return Ok(1);
        }
        if args.is_empty() {
            for (k, v) in assigns {
                let _ = self.set_var(&k, v);
            }
            return Ok(0);
        }
        let path = match self.find_in_path(&args[0]) {
            Some(p) => p,
            None => {
                self.error(format!("exec : {} : commande introuvable", args[0]));
                return if self.interactive { Ok(127) } else { Err(Flow::Exit(127)) };
            }
        };
        let env = self.env_list(&assigns);
        self.reclaim_terminal(None);
        crate::editor::save_history_now(self);
        self.do_exec(&path, args, &env);
    }

    fn push_temp_assigns(&mut self, assigns: &[(String, String)]) -> Vec<(String, Option<Var>)> {
        let mut saved = Vec::new();
        for (k, v) in assigns {
            saved.push((k.clone(), self.vars.get(k).cloned()));
            let _ = self.set_var(k, v.clone());
            if let Some(var) = self.vars.get_mut(k) {
                var.exported = true;
            }
        }
        saved
    }

    fn pop_temp_assigns(&mut self, saved: Vec<(String, Option<Var>)>) {
        for (k, old) in saved.into_iter().rev() {
            match old {
                Some(v) => {
                    self.vars.insert(k, v);
                }
                None => {
                    self.vars.remove(&k);
                }
            }
        }
    }

    pub fn call_function(&mut self, f: &Rc<FuncBody>, argv: &[String]) -> ExecResult {
        if self.func_depth > 1000 {
            self.error(format!("{} : récursion trop profonde", argv[0]));
            return Ok(1);
        }
        self.func_names.push(argv[0].clone());
        let saved_pos = std::mem::replace(&mut self.positional, argv[1..].to_vec());
        let saved_loop = std::mem::replace(&mut self.loop_depth, 0);
        self.local_stack.push(Default::default());
        self.func_depth += 1;
        let r = match self.apply_redirs(&f.redirs, true) {
            Ok(saved) => {
                let r = self.exec_compound(&f.body);
                self.restore_redirs(saved);
                r
            }
            Err(e) => {
                self.error(e);
                Ok(1)
            }
        };
        self.func_depth -= 1;
        self.func_names.pop();
        self.pop_locals();
        self.loop_depth = saved_loop;
        self.positional = saved_pos;
        match r {
            Err(Flow::Return(n)) => Ok(n),
            Err(Flow::Break(_)) | Err(Flow::Continue(_)) => Ok(0),
            other => other,
        }
    }

    // ------------------------------------------------------------------
    // Substitutions
    // ------------------------------------------------------------------

    /// $(…) : exécute dans un sous-shell et capture la sortie standard.
    pub fn capture(&mut self, list: &List) -> String {
        // $(< fichier) : lecture directe, sans sous-shell
        if let [item] = list.as_slice() {
            if !item.background && item.and_or.rest.is_empty() && item.and_or.first.cmds.len() == 1 && !item.and_or.first.negate {
                if let Command::Simple(sc) = &item.and_or.first.cmds[0] {
                    if sc.words.is_empty() && sc.assigns.is_empty() && sc.redirs.len() == 1 && sc.redirs[0].op == RedirOp::In && sc.redirs[0].fd.is_none() {
                        if let RedirTarget::Word(w) = &sc.redirs[0].target {
                            let w = w.clone();
                            return match self.expand_str(&w) {
                                Ok(path) => match std::fs::read(&path) {
                                    Ok(b) => {
                                        self.cmdsub_status = Some(0);
                                        String::from_utf8_lossy(&b).into_owned()
                                    }
                                    Err(e) => {
                                        self.error(format!("{path} : {e}"));
                                        self.cmdsub_status = Some(1);
                                        String::new()
                                    }
                                },
                                Err(e) => {
                                    self.error(e);
                                    String::new()
                                }
                            };
                        }
                    }
                }
            }
        }
        let (r, w) = match make_pipe() {
            Ok(x) => x,
            Err(e) => {
                self.error(e);
                return String::new();
            }
        };
        flush_stdout();
        let pid = match unsafe { unistd::fork() } {
            Err(e) => {
                self.error(format!("fork : {e}"));
                close_fd(r);
                close_fd(w);
                return String::new();
            }
            Ok(ForkResult::Child) => {
                close_fd(r);
                dup2(w, 1);
                close_fd(w);
                let jc = self.job_control;
                self.job_control = false;
                self.child_setup(None, false);
                let _ = jc;
                let st = match self.exec_list(list) {
                    Ok(s) => s,
                    Err(Flow::Exit(s)) | Err(Flow::Return(s)) => s,
                    Err(_) => self.last_status,
                };
                self.run_exit_trap();
                child_exit(st);
            }
            Ok(ForkResult::Parent { child }) => child,
        };
        close_fd(w);
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = unsafe { libc::read(r, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        let _ = signal::kill(pid, Signal::SIGINT);
                    }
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        close_fd(r);
        let st = loop {
            match waitpid(pid, None) {
                Ok(WaitStatus::Exited(_, c)) => break c,
                Ok(WaitStatus::Signaled(_, s, _)) => {
                    if s == Signal::SIGINT {
                        INTERRUPTED.store(true, Ordering::SeqCst);
                    }
                    break 128 + s as i32;
                }
                Ok(_) => continue,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break 0,
            }
        };
        self.cmdsub_status = Some(st);
        self.last_status = st;
        String::from_utf8_lossy(&out).into_owned()
    }

    /// <(cmd) / >(cmd) : renvoie un chemin /dev/fd/N.
    pub fn process_substitution(&mut self, list: &List, is_out: bool) -> Result<String, String> {
        let (r, w) = make_pipe()?;
        let list = list.clone();
        let (keep, child_end) = if is_out { (w, r) } else { (r, w) };
        let pid = self
            .fork_child(None, false, move |sh| {
                close_fd(keep);
                dup2(child_end, if is_out { 0 } else { 1 });
                close_fd(child_end);
                match sh.exec_list(&list) {
                    Ok(s) => s,
                    Err(Flow::Exit(s)) | Err(Flow::Return(s)) => s,
                    Err(_) => 1,
                }
            })
            .map_err(|_| "fork impossible".to_string())?;
        let _ = pid;
        close_fd(child_end);
        // Le descripteur doit survivre à exec : on retire CLOEXEC
        unsafe {
            libc::fcntl(keep, libc::F_SETFD, 0);
        }
        self.procsub_fds.push(keep);
        Ok(format!("/dev/fd/{keep}"))
    }

    // ------------------------------------------------------------------
    // Redirections
    // ------------------------------------------------------------------

    /// Applique les redirections. Si `save`, renvoie de quoi les annuler.
    pub fn apply_redirs(&mut self, redirs: &[Redir], save: bool) -> Result<Vec<(i32, i32)>, String> {
        let mut saved: Vec<(i32, i32)> = Vec::new();
        if redirs.is_empty() {
            return Ok(saved);
        }
        flush_stdout();
        for r in redirs {
            match self.apply_one(r, save, &mut saved) {
                Ok(()) => {}
                Err(e) => {
                    self.restore_redirs(std::mem::take(&mut saved));
                    return Err(e);
                }
            }
        }
        Ok(saved)
    }

    fn save_fd(fd: i32, saved: &mut Vec<(i32, i32)>) {
        let s = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
        saved.push((fd, s));
    }

    fn apply_one(&mut self, r: &Redir, save: bool, saved: &mut Vec<(i32, i32)>) -> Result<(), String> {
        use RedirOp::*;
        let default_fd = match r.op {
            In | ReadWrite | DupIn | HereStr => 0,
            _ => 1,
        };
        let fd = r.fd.unwrap_or(default_fd);
        let src: i32 = match &r.target {
            RedirTarget::Here(body) => {
                let text = match &*body.borrow() {
                    HereBody::Literal(s) => s.clone(),
                    HereBody::Expand(parts) => {
                        let parts = parts.clone();
                        self.expand_str(&vec![Part::DQuoted(parts)])?
                    }
                    HereBody::Pending => String::new(),
                };
                mem_fd(&text)?
            }
            RedirTarget::Word(w) => {
                let target = self.expand_str(w)?;
                match r.op {
                    HereStr => mem_fd(&(target + "\n"))?,
                    DupIn | DupOut => {
                        if target == "-" {
                            if save {
                                Self::save_fd(fd, saved);
                            }
                            close_fd(fd);
                            return Ok(());
                        }
                        let t = target.strip_suffix('-').unwrap_or(&target);
                        match t.parse::<i32>() {
                            Ok(n) => {
                                if unsafe { libc::fcntl(n, libc::F_GETFD) } < 0 {
                                    return Err(format!("{n} : mauvais descripteur de fichier"));
                                }
                                if save {
                                    Self::save_fd(fd, saved);
                                }
                                if n != fd {
                                    dup2(n, fd);
                                }
                                if target.ends_with('-') {
                                    close_fd(n);
                                }
                                return Ok(());
                            }
                            Err(_) if r.op == DupOut && r.fd.is_none() => {
                                // >& fichier  ==  &> fichier
                                let s = open_file(&target, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC)?;
                                if save {
                                    Self::save_fd(1, saved);
                                    Self::save_fd(2, saved);
                                }
                                dup2(s, 1);
                                dup2(s, 2);
                                close_fd(s);
                                return Ok(());
                            }
                            Err(_) => return Err(format!("{target} : redirection ambiguë")),
                        }
                    }
                    _ => {
                        if target.is_empty() {
                            return Err("redirection ambiguë (nom de fichier vide)".into());
                        }
                        let flags = match r.op {
                            In => libc::O_RDONLY,
                            Out | AndOut => {
                                if self.opts.noclobber && std::path::Path::new(&target).is_file() {
                                    return Err(format!("{target} : refus d'écraser un fichier existant (noclobber)"));
                                }
                                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC
                            }
                            Clobber => libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                            Append | AndAppend => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                            ReadWrite => libc::O_RDWR | libc::O_CREAT,
                            _ => unreachable!(),
                        };
                        let s = open_file(&target, flags)?;
                        if matches!(r.op, AndOut | AndAppend) {
                            if save {
                                Self::save_fd(1, saved);
                                Self::save_fd(2, saved);
                            }
                            dup2(s, 1);
                            dup2(s, 2);
                            close_fd(s);
                            return Ok(());
                        }
                        s
                    }
                }
            }
        };
        if save {
            Self::save_fd(fd, saved);
        }
        if src != fd {
            dup2(src, fd);
            close_fd(src);
        }
        Ok(())
    }

    pub fn restore_redirs(&mut self, saved: Vec<(i32, i32)>) {
        if saved.is_empty() {
            return;
        }
        flush_stdout();
        for (fd, s) in saved.into_iter().rev() {
            if s >= 0 {
                dup2(s, fd);
                close_fd(s);
            } else {
                close_fd(fd);
            }
        }
    }

    // ------------------------------------------------------------------
    // Pièges (trap)
    // ------------------------------------------------------------------

    pub fn run_pending_traps(&mut self) -> Result<(), Flow> {
        if self.in_trap {
            return Ok(());
        }
        for sig in 1..65usize {
            if PENDING[sig].swap(false, Ordering::SeqCst) {
                if let Some(cmd) = self.traps.get(&(sig as i32)).cloned() {
                    if sig as i32 == libc::SIGINT {
                        INTERRUPTED.store(false, Ordering::SeqCst);
                    }
                    if !cmd.is_empty() {
                        let st = self.last_status;
                        self.in_trap = true;
                        let r = self.run_source(&cmd, "");
                        self.in_trap = false;
                        self.last_status = st;
                        if let Err(Flow::Exit(n)) = r {
                            return Err(Flow::Exit(n));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn run_exit_trap(&mut self) {
        if let Some(cmd) = self.traps.remove(&0) {
            if !cmd.is_empty() {
                let st = self.last_status;
                let _ = self.run_source(&cmd, "");
                self.last_status = st;
            }
        }
    }
}

/// Temps CPU (utilisateur, système) du shell + de ses enfants terminés.
fn rusage_total() -> (f64, f64) {
    let get = |who| {
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(who, &mut u) };
        let t = |tv: libc::timeval| tv.tv_sec as f64 + tv.tv_usec as f64 / 1e6;
        (t(u.ru_utime), t(u.ru_stime))
    };
    let a = get(libc::RUSAGE_SELF);
    let b = get(libc::RUSAGE_CHILDREN);
    (a.0 + b.0, a.1 + b.1)
}

fn open_file(path: &str, flags: i32) -> Result<i32, String> {
    let c = CString::new(path).map_err(|_| "nom de fichier invalide".to_string())?;
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, 0o666) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        let msg = match e.raw_os_error() {
            Some(libc::ENOENT) => "fichier ou dossier introuvable".to_string(),
            Some(libc::EACCES) => "permission refusée".to_string(),
            Some(libc::EISDIR) => "est un dossier".to_string(),
            _ => e.to_string(),
        };
        return Err(format!("{path} : {msg}"));
    }
    Ok(fd)
}

/// Descripteur en mémoire contenant `text` (here-doc, here-string).
fn mem_fd(text: &str) -> Result<i32, String> {
    let name = CString::new("shsh-here").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        // Repli : tube (limité à la taille du tampon)
        let (r, w) = make_pipe()?;
        write_fd(w, text);
        close_fd(w);
        return Ok(r);
    }
    write_fd(fd, text);
    unsafe {
        libc::lseek(fd, 0, libc::SEEK_SET);
    }
    Ok(fd)
}

fn byte_of(s: &str, char_pos: usize) -> usize {
    s.char_indices().nth(char_pos).map(|(i, _)| i).unwrap_or(s.len())
}

fn and_or_text(ao: &AndOr) -> String {
    let mut s = ao.first.text.clone();
    for (is_and, p) in &ao.rest {
        s.push_str(if *is_and { " && " } else { " || " });
        s.push_str(&p.text);
    }
    s
}

pub fn quote_if_needed(s: &str) -> String {
    if !s.is_empty()
        && s
            .chars()
            .all(|c| c.is_alphanumeric() || "-_./=:,+@%^".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
