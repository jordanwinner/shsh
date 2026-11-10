//! Évaluateur arithmétique $(( … )) / (( … )) — entiers 64 bits, syntaxe C.

pub trait Vars {
    fn get_var(&mut self, name: &str) -> Option<String>;
    fn set_var(&mut self, name: &str, val: String) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq)]
enum T {
    Num(i64),
    Id(String),
    Op(&'static str),
}

const OPS: &[&str] = &[
    "<<=", ">>=", "**", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=", "*=",
    "/=", "%=", "&=", "^=", "|=", "+", "-", "*", "/", "%", "<", ">", "=", "!", "~", "&", "^", "|",
    "?", ":", "(", ")", ",",
];

fn parse_number(s: &str) -> Result<i64, String> {
    let err = || format!("nombre invalide « {s} »");
    if let Some((base, digits)) = s.split_once('#') {
        let b: u32 = base.parse().map_err(|_| err())?;
        if !(2..=64).contains(&b) {
            return Err(err());
        }
        let mut v: i64 = 0;
        for c in digits.chars() {
            let d = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'a'..='z' => c as u32 - 'a' as u32 + 10,
                'A'..='Z' => {
                    if b <= 36 {
                        c as u32 - 'A' as u32 + 10
                    } else {
                        c as u32 - 'A' as u32 + 36
                    }
                }
                '@' => 62,
                '_' => 63,
                _ => return Err(err()),
            };
            if d >= b {
                return Err(err());
            }
            v = v.wrapping_mul(b as i64).wrapping_add(d as i64);
        }
        return Ok(v);
    }
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).map_err(|_| err());
    }
    if s.len() > 1 && s.starts_with('0') {
        return i64::from_str_radix(&s[1..], 8).map_err(|_| err());
    }
    s.parse::<i64>().map_err(|_| err())
}

fn tokenize(s: &str) -> Result<Vec<T>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    'outer: while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch.is_ascii_digit() {
            let st = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '#' || c[i] == '_' || c[i] == '@') {
                i += 1;
            }
            let t: String = c[st..i].iter().collect();
            out.push(T::Num(parse_number(&t)?));
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(T::Id(c[st..i].iter().collect()));
            continue;
        }
        for op in OPS {
            let n = op.len();
            if i + n <= c.len() && c[i..i + n].iter().copied().eq(op.chars()) {
                out.push(T::Op(op));
                i += n;
                continue 'outer;
            }
        }
        return Err(format!("caractère inattendu « {ch} » dans l'expression"));
    }
    Ok(out)
}

struct P<'a, V: Vars> {
    t: Vec<T>,
    i: usize,
    v: &'a mut V,
    /// Évaluation court-circuitée : on analyse sans effets de bord.
    skip: u32,
    depth: u32,
}

/// Valeur intermédiaire : nombre, éventuellement issu d'une variable (pour = et ++).
#[derive(Clone)]
struct Val {
    n: i64,
    lv: Option<String>,
}

impl<'a, V: Vars> P<'a, V> {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn is_op(&self, o: &str) -> bool {
        matches!(self.peek(), Some(T::Op(x)) if *x == o)
    }
    fn eat(&mut self, o: &str) -> bool {
        if self.is_op(o) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn var_value(&mut self, name: &str) -> Result<i64, String> {
        if self.skip > 0 {
            return Ok(0);
        }
        let s = self.v.get_var(name).unwrap_or_default();
        let s = s.trim();
        if s.is_empty() {
            return Ok(0);
        }
        if let Ok(n) = parse_number(s) {
            return Ok(n);
        }
        // La valeur est elle-même une expression
        self.depth += 1;
        if self.depth > 64 {
            return Err("récursion trop profonde dans l'expression".into());
        }
        let r = eval_inner(s, self.v, self.depth);
        self.depth -= 1;
        r
    }

    fn assign(&mut self, name: &str, n: i64) -> Result<(), String> {
        if self.skip > 0 {
            return Ok(());
        }
        self.v.set_var(name, n.to_string())
    }

    fn rv(&mut self, v: Val) -> Result<i64, String> {
        Ok(v.n)
    }

    fn comma(&mut self) -> Result<Val, String> {
        let mut v = self.assign_expr()?;
        while self.eat(",") {
            v = self.assign_expr()?;
        }
        Ok(v)
    }

    fn assign_expr(&mut self) -> Result<Val, String> {
        let start = self.i;
        if let Some(T::Id(name)) = self.peek().cloned() {
            if let Some(T::Op(op)) = self.t.get(self.i + 1).cloned() {
                let ops = ["=", "+=", "-=", "*=", "/=", "%=", "<<=", ">>=", "&=", "^=", "|="];
                if ops.contains(&op) {
                    self.i += 2;
                    let rhs = self.assign_expr()?.n;
                    let n = if op == "=" {
                        rhs
                    } else {
                        let cur = self.var_value(&name)?;
                        binop(&op[..op.len() - 1], cur, rhs, self.skip > 0)?
                    };
                    self.assign(&name, n)?;
                    return Ok(Val { n, lv: None });
                }
            }
        }
        self.i = start;
        self.ternary()
    }

    fn ternary(&mut self) -> Result<Val, String> {
        let c = self.binary(0)?;
        if self.eat("?") {
            let cond = c.n != 0;
            if !cond {
                self.skip += 1;
            }
            let a = self.comma()?;
            if !cond {
                self.skip -= 1;
            }
            if !self.eat(":") {
                return Err("« : » attendu dans l'expression ?:".into());
            }
            if cond {
                self.skip += 1;
            }
            let b = self.assign_expr()?;
            if cond {
                self.skip -= 1;
            }
            return Ok(if cond { a } else { b });
        }
        Ok(c)
    }

    fn binary(&mut self, min: u8) -> Result<Val, String> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(T::Op(o)) => *o,
                _ => break,
            };
            let prec = match op {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                "**" => 11,
                _ => break,
            };
            if prec < min {
                break;
            }
            self.i += 1;
            if op == "&&" || op == "||" {
                let short = if op == "&&" { l.n == 0 } else { l.n != 0 };
                if short {
                    self.skip += 1;
                }
                let r = self.binary(prec + 1)?;
                if short {
                    self.skip -= 1;
                }
                let n = if op == "&&" {
                    (l.n != 0 && r.n != 0) as i64
                } else {
                    (l.n != 0 || r.n != 0) as i64
                };
                l = Val { n, lv: None };
                continue;
            }
            // ** est associatif à droite
            let r = if op == "**" {
                self.binary(prec)?
            } else {
                self.binary(prec + 1)?
            };
            let a = self.rv(l)?;
            let b = self.rv(r)?;
            l = Val {
                n: binop(op, a, b, self.skip > 0)?,
                lv: None,
            };
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Val, String> {
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("-") {
            let v = self.unary()?;
            return Ok(Val {
                n: v.n.wrapping_neg(),
                lv: None,
            });
        }
        if self.eat("!") {
            let v = self.unary()?;
            return Ok(Val {
                n: (v.n == 0) as i64,
                lv: None,
            });
        }
        if self.eat("~") {
            let v = self.unary()?;
            return Ok(Val { n: !v.n, lv: None });
        }
        for (op, d) in [("++", 1i64), ("--", -1)] {
            if self.eat(op) {
                let v = self.postfix()?;
                let name = v.lv.ok_or("++/-- s'applique à une variable")?;
                let n = self.var_value(&name)? + d;
                self.assign(&name, n)?;
                return Ok(Val { n, lv: None });
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Val, String> {
        let v = self.primary()?;
        if let Some(name) = v.lv.clone() {
            for (op, d) in [("++", 1i64), ("--", -1)] {
                if self.eat(op) {
                    self.assign(&name, v.n + d)?;
                    return Ok(Val { n: v.n, lv: None });
                }
            }
        }
        Ok(v)
    }

    fn primary(&mut self) -> Result<Val, String> {
        match self.peek().cloned() {
            Some(T::Num(n)) => {
                self.i += 1;
                Ok(Val { n, lv: None })
            }
            Some(T::Id(name)) => {
                self.i += 1;
                let n = self.var_value(&name)?;
                Ok(Val { n, lv: Some(name) })
            }
            Some(T::Op("(")) => {
                self.i += 1;
                let v = self.comma()?;
                if !self.eat(")") {
                    return Err("« ) » attendue dans l'expression".into());
                }
                Ok(Val { n: v.n, lv: None })
            }
            Some(T::Op(o)) => Err(format!("opérateur inattendu « {o} »")),
            None => Err("expression incomplète".into()),
        }
    }
}

fn binop(op: &str, a: i64, b: i64, skipping: bool) -> Result<i64, String> {
    Ok(match op {
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        "*" => a.wrapping_mul(b),
        "/" | "%" => {
            if b == 0 {
                if skipping {
                    return Ok(0);
                }
                return Err("division par zéro".into());
            }
            if op == "/" {
                a.wrapping_div(b)
            } else {
                a.wrapping_rem(b)
            }
        }
        "**" => {
            if b < 0 {
                return Err("exposant négatif".into());
            }
            a.wrapping_pow(b.min(u32::MAX as i64) as u32)
        }
        "<<" => a.wrapping_shl(b as u32),
        ">>" => a.wrapping_shr(b as u32),
        "<" => (a < b) as i64,
        ">" => (a > b) as i64,
        "<=" => (a <= b) as i64,
        ">=" => (a >= b) as i64,
        "==" => (a == b) as i64,
        "!=" => (a != b) as i64,
        "&" => a & b,
        "^" => a ^ b,
        "|" => a | b,
        _ => return Err(format!("opérateur inconnu « {op} »")),
    })
}

fn eval_inner<V: Vars>(s: &str, v: &mut V, depth: u32) -> Result<i64, String> {
    let t = tokenize(s)?;
    if t.is_empty() {
        return Ok(0);
    }
    let mut p = P {
        t,
        i: 0,
        v,
        skip: 0,
        depth,
    };
    let r = p.comma()?;
    if p.i != p.t.len() {
        return Err(format!("erreur de syntaxe dans l'expression « {} »", s.trim()));
    }
    Ok(r.n)
}

pub fn eval<V: Vars>(s: &str, v: &mut V) -> Result<i64, String> {
    eval_inner(s, v, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    struct M(HashMap<String, String>);
    impl Vars for M {
        fn get_var(&mut self, n: &str) -> Option<String> {
            self.0.get(n).cloned()
        }
        fn set_var(&mut self, n: &str, v: String) -> Result<(), String> {
            self.0.insert(n.into(), v);
            Ok(())
        }
    }
    #[test]
    fn basics() {
        let mut m = M(HashMap::new());
        assert_eq!(eval("1 + 2 * 3", &mut m), Ok(7));
        assert_eq!(eval("(1 + 2) * 3", &mut m), Ok(9));
        assert_eq!(eval("2 ** 3 ** 2", &mut m), Ok(512));
        assert_eq!(eval("x = 5, x += 2, x", &mut m), Ok(7));
        assert_eq!(eval("x++ + ++x", &mut m), Ok(7 + 9));
        assert_eq!(eval("0x10 + 010 + 2#101", &mut m), Ok(16 + 8 + 5));
        assert_eq!(eval("1 ? 2 : 3", &mut m), Ok(2));
        assert_eq!(eval("0 && 1/0", &mut m), Ok(0));
        assert_eq!(eval("-3 % 2", &mut m), Ok(-1));
        assert!(eval("1/0", &mut m).is_err());
    }
}
