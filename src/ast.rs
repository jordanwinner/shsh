//! Arbre syntaxique de shsh (grammaire POSIX + quelques extensions bash/zsh).

use std::cell::RefCell;
use std::rc::Rc;

/// Un mot = suite de morceaux (littéraux, guillemets, expansions).
pub type Word = Vec<Part>;

#[derive(Debug, Clone)]
pub enum Part {
    /// Texte non protégé : soumis au globbing.
    Lit(String),
    /// Texte protégé ('...', \x, $'...') : ni découpage ni globbing.
    Quoted(String),
    /// "..." : contient Lit (littéral protégé), Param, CmdSub, Arith.
    DQuoted(Vec<Part>),
    Param(Box<ParamExp>),
    CmdSub(Rc<List>),
    Arith(Vec<Part>),
    /// ~ ou ~utilisateur
    Tilde(String),
    /// <(cmd) ou >(cmd)
    ProcSub(bool, Rc<List>),
    /// nom=(…) en argument de local/declare/typeset/readonly/export
    ArrayLit(String, bool, Vec<Word>),
}

#[derive(Debug, Clone)]
pub struct ParamExp {
    pub name: String,
    pub op: ParamOp,
    /// ${a[i]} / ${a[@]} / ${a[*]}
    pub index: Option<Index>,
    /// ${!nom} (indirection) ou ${!a[@]} (indices)
    pub indirect: bool,
}

#[derive(Debug, Clone)]
pub enum Index {
    /// [@] (false) ou [*] (true)
    All(bool),
    Expr(Vec<Part>),
}

impl ParamExp {
    pub fn simple(name: String, op: ParamOp) -> Self {
        ParamExp { name, op, index: None, indirect: false }
    }
}

#[derive(Debug, Clone)]
pub enum ParamOp {
    Plain,
    Length,
    /// ${x:-w} / ${x-w}
    Default(bool, Word),
    /// ${x:=w}
    Assign(bool, Word),
    /// ${x:?w}
    Error(bool, Word),
    /// ${x:+w}
    Alt(bool, Word),
    /// ${x#p} (false) / ${x##p} (true)
    TrimPrefix(bool, Word),
    TrimSuffix(bool, Word),
    /// ${x/p/r} : (tous, ancre '#' ou '%', motif, remplacement)
    Replace(bool, Option<char>, Word, Word),
    /// ${x^^} / ${x^} (+ motif facultatif : seuls les caractères qui correspondent)
    Upper(bool, Word),
    Lower(bool, Word),
    /// ${x:début:longueur}
    Substr(Word, Option<Word>),
    /// ${x@Q} ${x@U} ${x@u} ${x@L} ${x@E}
    Transform(char),
    /// ${!préfixe*} / ${!préfixe@} : noms des variables (true = forme *)
    PrefixNames(bool),
}

pub type List = Vec<ListItem>;

#[derive(Debug, Clone)]
pub struct ListItem {
    pub and_or: AndOr,
    pub background: bool,
}

#[derive(Debug, Clone)]
pub struct AndOr {
    pub first: Pipeline,
    /// (true = &&, false = ||)
    pub rest: Vec<(bool, Pipeline)>,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub negate: bool,
    /// précédé du mot-clé `time`
    pub timed: bool,
    pub cmds: Vec<Command>,
    /// Texte source (affiché par `jobs`).
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum Command {
    Simple(SimpleCommand),
    Compound(Box<Compound>, Vec<Redir>),
    FuncDef(String, Rc<FuncBody>),
}

#[derive(Debug)]
pub struct FuncBody {
    /// Source du corps, tel qu'écrit (pour `type` et `declare -f`)
    pub text: String,
    pub body: Compound,
    pub redirs: Vec<Redir>,
}

#[derive(Debug, Clone)]
pub struct Assign {
    pub name: String,
    pub append: bool,
    pub value: Word,
    /// nom[expr]=…
    pub index: Option<Vec<Part>>,
    /// nom=(…)
    pub array: Option<Vec<Word>>,
}

#[derive(Debug, Clone)]
pub struct SimpleCommand {
    pub assigns: Vec<Assign>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
    /// Ligne du source où commence la commande ($LINENO)
    pub line: usize,
}

#[derive(Debug, Clone)]
pub enum Compound {
    Brace(List),
    Subshell(List),
    If {
        branches: Vec<(List, List)>,
        else_body: Option<List>,
    },
    Loop {
        until: bool,
        cond: List,
        body: List,
    },
    For {
        var: String,
        items: Option<Vec<Word>>,
        body: List,
    },
    ArithFor {
        init: Vec<Part>,
        cond: Vec<Part>,
        step: Vec<Part>,
        body: List,
    },
    Case {
        word: Word,
        arms: Vec<CaseArm>,
    },
    Arith(Vec<Part>),
    Cond(CondExpr),
}

#[derive(Debug, Clone)]
pub struct CaseArm {
    pub patterns: Vec<Word>,
    pub body: List,
    /// ;& : exécute aussi la branche suivante
    pub fallthrough: bool,
    /// ;;& : continue à comparer les branches suivantes
    pub retest: bool,
}

#[derive(Debug, Clone)]
pub enum CondExpr {
    And(Box<CondExpr>, Box<CondExpr>),
    Or(Box<CondExpr>, Box<CondExpr>),
    Not(Box<CondExpr>),
    Unary(String, Word),
    Binary(Word, String, Word),
    Single(Word),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirOp {
    In,        // <
    Out,       // >
    Append,    // >>
    Clobber,   // >|
    ReadWrite, // <>
    DupIn,     // <&
    DupOut,    // >&
    HereStr,   // <<<
    AndOut,    // &>
    AndAppend, // &>>
}

#[derive(Debug, Clone)]
pub enum HereBody {
    Pending,
    Literal(String),
    Expand(Vec<Part>),
}

#[derive(Debug, Clone)]
pub enum RedirTarget {
    Word(Word),
    Here(Rc<RefCell<HereBody>>),
}

#[derive(Debug, Clone)]
pub struct Redir {
    /// {nom}>fichier : le shell choisit un descripteur libre et le range dans `nom`
    pub var: Option<String>,
    pub fd: Option<i32>,
    pub op: RedirOp,
    pub target: RedirTarget,
}
