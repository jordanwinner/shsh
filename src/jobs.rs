//! Table des tâches (jobs) et attente des processus.

use nix::sys::termios::Termios;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcState {
    Running,
    Stopped,
    /// Statut final (code de sortie, ou 128+signal)
    Done(i32, Option<i32>),
}

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: Pid,
    pub state: ProcState,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,
    pub pgid: Pid,
    pub procs: Vec<Proc>,
    pub text: String,
    pub tmodes: Option<Termios>,
    pub notified: bool,
    pub background: bool,
}

impl Job {
    pub fn is_done(&self) -> bool {
        self.procs.iter().all(|p| matches!(p.state, ProcState::Done(..)))
    }
    pub fn is_stopped(&self) -> bool {
        !self.is_done()
            && self
                .procs
                .iter()
                .all(|p| matches!(p.state, ProcState::Done(..) | ProcState::Stopped))
    }
    pub fn status(&self, pipefail: bool) -> i32 {
        let codes: Vec<i32> = self
            .procs
            .iter()
            .map(|p| match p.state {
                ProcState::Done(c, _) => c,
                _ => 0,
            })
            .collect();
        if pipefail {
            codes.iter().rev().find(|&&c| c != 0).copied().unwrap_or(0)
        } else {
            codes.last().copied().unwrap_or(0)
        }
    }
    pub fn signal(&self) -> Option<i32> {
        self.procs.iter().rev().find_map(|p| match p.state {
            ProcState::Done(_, s) => s,
            _ => None,
        })
    }
    pub fn state_label(&self) -> String {
        if self.is_done() {
            let st = self.status(false);
            match self.signal() {
                Some(s) => crate::shell::signal_message(s).to_string(),
                None if st == 0 => "Terminé".into(),
                None => format!("Sortie {st}"),
            }
        } else if self.is_stopped() {
            "Stoppé".into()
        } else {
            "En cours".into()
        }
    }
}

#[derive(Debug, Default)]
pub struct JobTable {
    pub jobs: Vec<Job>,
    /// Historique d'usage : le dernier est la tâche courante (%+).
    pub order: Vec<usize>,
}

impl JobTable {
    pub fn add(&mut self, pgid: Pid, pids: Vec<Pid>, text: String, background: bool) -> usize {
        let id = (1..).find(|i| !self.jobs.iter().any(|j| j.id == *i)).unwrap();
        self.jobs.push(Job {
            id,
            pgid,
            procs: pids
                .into_iter()
                .map(|pid| Proc {
                    pid,
                    state: ProcState::Running,
                })
                .collect(),
            text,
            tmodes: None,
            notified: false,
            background,
        });
        self.touch(id);
        id
    }

    pub fn touch(&mut self, id: usize) {
        self.order.retain(|&x| x != id);
        self.order.push(id);
    }

    pub fn get(&self, id: usize) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn remove(&mut self, id: usize) {
        self.jobs.retain(|j| j.id != id);
        self.order.retain(|&x| x != id);
    }

    pub fn current(&self) -> Option<usize> {
        self.order.last().copied()
    }

    pub fn previous(&self) -> Option<usize> {
        if self.order.len() >= 2 {
            Some(self.order[self.order.len() - 2])
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.jobs.clear();
        self.order.clear();
    }

    /// Met à jour l'état d'un processus après waitpid.
    pub fn update(&mut self, st: WaitStatus) -> bool {
        let (pid, state) = match st {
            WaitStatus::Exited(pid, code) => (pid, ProcState::Done(code, None)),
            WaitStatus::Signaled(pid, sig, _) => {
                (pid, ProcState::Done(128 + sig as i32, Some(sig as i32)))
            }
            WaitStatus::Stopped(pid, _) => (pid, ProcState::Stopped),
            WaitStatus::Continued(pid) => (pid, ProcState::Running),
            _ => return false,
        };
        for j in self.jobs.iter_mut() {
            for p in j.procs.iter_mut() {
                if p.pid == pid {
                    p.state = state;
                    return true;
                }
            }
        }
        false
    }

    /// Relève sans bloquer les processus terminés ou stoppés.
    pub fn reap(&mut self) {
        loop {
            match waitpid(
                Pid::from_raw(-1),
                Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED),
            ) {
                Ok(WaitStatus::StillAlive) | Err(_) => break,
                Ok(st) => {
                    self.update(st);
                }
            }
        }
    }

    /// Résout une spécification %n, %+, %-, %chaîne, %?chaîne.
    pub fn resolve(&self, spec: &str) -> Option<usize> {
        let s = spec.strip_prefix('%').unwrap_or(spec);
        match s {
            "" | "+" | "%" => self.current(),
            "-" => self.previous(),
            _ => {
                if let Ok(n) = s.parse::<usize>() {
                    return self.get(n).map(|j| j.id);
                }
                if let Some(q) = s.strip_prefix('?') {
                    return self.jobs.iter().rev().find(|j| j.text.contains(q)).map(|j| j.id);
                }
                self.jobs
                    .iter()
                    .rev()
                    .find(|j| j.text.starts_with(s))
                    .map(|j| j.id)
            }
        }
    }

    pub fn marker(&self, id: usize) -> char {
        if self.current() == Some(id) {
            '+'
        } else if self.previous() == Some(id) {
            '-'
        } else {
            ' '
        }
    }
}
