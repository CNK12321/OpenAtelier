//! Tokens → statements and expressions.

use crate::lex::Tok;

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Num(f32),
    Name(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    Let(String, Expr),
    State(String, Expr),
    Line(String, Expr),
    Assign(String, Option<Bin>, Expr),
    Call(Expr),
}

pub(crate) struct Parser {
    tokens: Vec<(Tok, usize)>,
    at: usize,
    /// `line` is a keyword (the host offers delay lines).
    lines: bool,
}

impl Parser {
    pub(crate) fn new(tokens: Vec<(Tok, usize)>, lines: bool) -> Self {
        Parser { tokens, at: 0, lines }
    }

    pub(crate) fn line(&self) -> usize {
        self.tokens.get(self.at).or(self.tokens.last()).map_or(1, |t| t.1)
    }

    fn fail<T>(&self, what: impl std::fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {what}", self.line()))
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|t| &t.0)
    }

    fn eat(&mut self, sym: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Sym(s)) if *s == sym) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, sym: &str) -> Result<(), String> {
        if self.eat(sym) {
            return Ok(());
        }
        let got = match self.peek() {
            Some(Tok::Num(n)) => format!("{n}"),
            Some(Tok::Ident(s)) => format!("\"{s}\""),
            Some(Tok::Sym(s)) => format!("\"{s}\""),
            None => "the end".into(),
        };
        self.fail(format!("expected \"{sym}\", found {got}"))
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Ident(s)) => {
                self.at += 1;
                Ok(s)
            }
            _ => self.fail("expected a name"),
        }
    }

    /// The next statement and the line it starts on, or `None` at the end.
    pub(crate) fn statement(&mut self) -> Result<Option<(Stmt, usize)>, String> {
        if self.at >= self.tokens.len() {
            return Ok(None);
        }
        let line = self.line();
        let start = self.at;
        let word = self.ident()?;
        let stmt = match word.as_str() {
            "let" | "state" => {
                let name = self.ident()?;
                self.expect("=")?;
                let e = self.expr()?;
                if word == "let" { Stmt::Let(name, e) } else { Stmt::State(name, e) }
            }
            "line" if self.lines => {
                let name = self.ident()?;
                self.expect("=")?;
                Stmt::Line(name, self.expr()?)
            }
            _ if matches!(self.peek(), Some(Tok::Sym("("))) => {
                self.at = start;
                Stmt::Call(self.expr()?)
            }
            _ => {
                let op = if self.eat("=") {
                    None
                } else if self.eat("+=") {
                    Some(Bin::Add)
                } else if self.eat("-=") {
                    Some(Bin::Sub)
                } else if self.eat("*=") {
                    Some(Bin::Mul)
                } else if self.eat("/=") {
                    Some(Bin::Div)
                } else {
                    return self.fail(format!("expected \"=\" after \"{word}\""));
                };
                Stmt::Assign(word, op, self.expr()?)
            }
        };
        self.expect(";")?;
        Ok(Some((stmt, line)))
    }

    pub(crate) fn expr(&mut self) -> Result<Expr, String> {
        let mut e = self.and()?;
        while self.eat("||") {
            e = Expr::Bin(Bin::Or, Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut e = self.compare()?;
        while self.eat("&&") {
            e = Expr::Bin(Bin::And, Box::new(e), Box::new(self.compare()?));
        }
        Ok(e)
    }

    fn compare(&mut self) -> Result<Expr, String> {
        let e = self.sum()?;
        for (sym, op) in [("==", Bin::Eq), ("!=", Bin::Ne), ("<=", Bin::Le), (">=", Bin::Ge), ("<", Bin::Lt), (">", Bin::Gt)] {
            if self.eat(sym) {
                return Ok(Expr::Bin(op, Box::new(e), Box::new(self.sum()?)));
            }
        }
        Ok(e)
    }

    fn sum(&mut self) -> Result<Expr, String> {
        let mut e = self.product()?;
        loop {
            let op = if self.eat("+") {
                Bin::Add
            } else if self.eat("-") {
                Bin::Sub
            } else {
                return Ok(e);
            };
            e = Expr::Bin(op, Box::new(e), Box::new(self.product()?));
        }
    }

    fn product(&mut self) -> Result<Expr, String> {
        let mut e = self.unary()?;
        loop {
            let op = if self.eat("*") {
                Bin::Mul
            } else if self.eat("/") {
                Bin::Div
            } else if self.eat("%") {
                Bin::Rem
            } else {
                return Ok(e);
            };
            e = Expr::Bin(op, Box::new(e), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("-") {
            Ok(Expr::Neg(Box::new(self.unary()?)))
        } else if self.eat("!") {
            Ok(Expr::Not(Box::new(self.unary()?)))
        } else {
            self.power()
        }
    }

    /// `a ^ b` is `pow(a, b)`: right-associative, tighter than `*`.
    fn power(&mut self) -> Result<Expr, String> {
        let e = self.primary()?;
        if self.eat("^") {
            return Ok(Expr::Bin(Bin::Pow, Box::new(e), Box::new(self.unary()?)));
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.at += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Sym("(")) => {
                self.at += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Tok::Ident(name)) => {
                self.at += 1;
                if !self.eat("(") {
                    return Ok(Expr::Name(name));
                }
                let mut args = Vec::new();
                if !self.eat(")") {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(")") {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                Ok(Expr::Call(name, args))
            }
            Some(Tok::Sym(s)) => self.fail(format!("unexpected \"{s}\"")),
            None => self.fail("the script ends in the middle of a line"),
        }
    }
}
