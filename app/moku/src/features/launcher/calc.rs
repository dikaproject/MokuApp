//! The launcher's calculator: `+ − × ÷ ^ %` and parentheses, a decimal comma or dot.
//!
//! * `^` binds tightest and is right-associative; `-2^2` = −4.
//! * `%` after a number is "percent" (`50%` = 0.5, `200 × 10%` = 20); between two numbers it is
//!   the remainder (`10 % 3` = 1).
//! * `x`, `*`, `×`, `·` multiply; `/`, `÷`, `:` divide; `−` is a minus.
//! * A number has at most one separator, `,` or `.`, and it is the decimal one (`1,5` = `1.5`).
//!   There is no thousands grouping in the input.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CalcError {
    Syntax,
    DivByZero,
    /// Infinite / not a number (e.g. `10^400`).
    Range,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Num(f64),
    Op(char),
    Open,
    Close,
}

fn op_char(c: char) -> Option<char> {
    Some(match c {
        '+' => '+',
        '-' | '−' | '–' => '-',
        '*' | 'x' | 'X' | '×' | '·' => '*',
        '/' | '÷' | ':' => '/',
        '^' => '^',
        '%' => '%',
        _ => return None,
    })
}

fn tokenize(s: &str) -> Result<Vec<Tok>, CalcError> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || ((c == '.' || c == ',') && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let mut num = String::new();
            let mut sep = false;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.' || cs[i] == ',') {
                if cs[i] == '.' || cs[i] == ',' {
                    if sep {
                        return Err(CalcError::Syntax);
                    }
                    sep = true;
                    num.push('.');
                } else {
                    num.push(cs[i]);
                }
                i += 1;
            }
            out.push(Tok::Num(num.parse().map_err(|_| CalcError::Syntax)?));
        } else if c == '(' {
            out.push(Tok::Open);
            i += 1;
        } else if c == ')' {
            out.push(Tok::Close);
            i += 1;
        } else if let Some(o) = op_char(c) {
            out.push(Tok::Op(o));
            i += 1;
        } else {
            return Err(CalcError::Syntax);
        }
    }
    Ok(out)
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<Tok> {
        self.t.get(self.i).copied()
    }
    fn starts_operand(t: Option<Tok>) -> bool {
        matches!(t, Some(Tok::Num(_) | Tok::Open))
    }
    fn expr(&mut self) -> Result<f64, CalcError> {
        let mut v = self.term()?;
        while let Some(Tok::Op(o @ ('+' | '-'))) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if o == '+' { v + r } else { v - r };
        }
        Ok(v)
    }
    fn term(&mut self) -> Result<f64, CalcError> {
        let mut v = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Op(o @ ('*' | '/'))) => {
                    self.i += 1;
                    let r = self.unary()?;
                    if o == '/' && r == 0.0 {
                        return Err(CalcError::DivByZero);
                    }
                    v = if o == '*' { v * r } else { v / r };
                }
                // `%` followed by an operand: remainder (a trailing one was taken as percent).
                Some(Tok::Op('%')) if Self::starts_operand(self.t.get(self.i + 1).copied()) => {
                    self.i += 1;
                    let r = self.unary()?;
                    if r == 0.0 {
                        return Err(CalcError::DivByZero);
                    }
                    v %= r;
                }
                _ => return Ok(v),
            }
        }
    }
    fn unary(&mut self) -> Result<f64, CalcError> {
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.i += 1;
                Ok(-self.unary()?)
            }
            Some(Tok::Op('+')) => {
                self.i += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }
    fn power(&mut self) -> Result<f64, CalcError> {
        let base = self.postfix()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.i += 1;
            let e = self.unary()?;
            return Ok(base.powf(e));
        }
        Ok(base)
    }
    fn postfix(&mut self) -> Result<f64, CalcError> {
        let mut v = self.primary()?;
        while self.peek() == Some(Tok::Op('%')) && !Self::starts_operand(self.t.get(self.i + 1).copied()) {
            self.i += 1;
            v /= 100.0;
        }
        Ok(v)
    }
    fn primary(&mut self) -> Result<f64, CalcError> {
        match self.peek() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(n)
            }
            Some(Tok::Open) => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(Tok::Close) {
                    return Err(CalcError::Syntax);
                }
                self.i += 1;
                Ok(v)
            }
            _ => Err(CalcError::Syntax),
        }
    }
}

/// Evaluate an expression (a leading `=` is allowed).
pub fn eval(s: &str) -> Result<f64, CalcError> {
    let s = s.trim().trim_start_matches('=');
    let t = tokenize(s)?;
    if t.is_empty() {
        return Err(CalcError::Syntax);
    }
    let mut p = P { t, i: 0 };
    let v = p.expr()?;
    if p.i != p.t.len() {
        return Err(CalcError::Syntax);
    }
    if !v.is_finite() {
        return Err(CalcError::Range);
    }
    Ok(if v == 0.0 { 0.0 } else { v })
}

/// Is the query meant for the calculator? A leading `=`, or only calculator characters with a
/// digit and an operator (`2024` alone is a search, `12*3` is a sum).
pub fn looks_like_calc(q: &str) -> bool {
    let q = q.trim();
    if let Some(rest) = q.strip_prefix('=') {
        return rest.chars().any(|c| c.is_ascii_digit());
    }
    let allowed = |c: char| c.is_ascii_digit() || c.is_whitespace() || "().,".contains(c) || op_char(c).is_some();
    // `x` multiplies only between numbers: "x264" or "box" are searches.
    let x_ok = !q.contains(['x', 'X']) || q.chars().filter(|c| c.is_alphabetic()).all(|c| c == 'x' || c == 'X') && !q.starts_with(['x', 'X']);
    q.chars().all(allowed) && x_ok && q.chars().any(|c| c.is_ascii_digit()) && q.chars().skip(1).any(|c| op_char(c).is_some() || c == '(') && tokenize(q).is_ok()
}

/// Format a result: at most 10 decimals, trailing zeros trimmed, `comma` = decimal comma (and
/// `.` for thousands when `group`), else a decimal point (and `,` for thousands).
pub fn format(v: f64, comma: bool, group: bool) -> String {
    let (dec, sep) = if comma { (',', '.') } else { ('.', ',') };
    if v.abs() >= 1e15 || (v != 0.0 && v.abs() < 1e-9) {
        return format!("{v:e}").replace('.', &dec.to_string());
    }
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if group && i > 0 && (int.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    if !frac.is_empty() {
        out.push(dec);
        out.push_str(frac);
    }
    if neg && out != "0" { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> f64 {
        eval(s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(ev("2+3*4"), 14.0);
        assert_eq!(ev("(2+3)*4"), 20.0);
        assert_eq!(ev("2^3^2"), 512.0, "right-associative");
        assert_eq!(ev("-2^2"), -4.0);
        assert_eq!(ev("2^-1"), 0.5);
        assert_eq!(ev("10-4-3"), 3.0, "left-associative");
        assert_eq!(ev("100/4/5"), 5.0);
        assert_eq!(ev("2 × 3 ÷ 4"), 1.5);
        assert_eq!(ev("7 − 2"), 5.0);
        assert_eq!(ev("3x4"), 12.0);
        assert_eq!(ev("= 1+1"), 2.0);
        assert_eq!(ev("--3"), 3.0);
    }

    #[test]
    fn percent_and_remainder() {
        assert_eq!(ev("50%"), 0.5);
        assert_eq!(ev("200*10%"), 20.0);
        assert_eq!(ev("10 % 3"), 1.0);
        assert_eq!(ev("(10+10)%"), 0.2);
    }

    #[test]
    fn locale_comma_and_dot() {
        assert_eq!(ev("1,5+1,5"), 3.0);
        assert_eq!(ev("1.5*2"), 3.0);
        assert_eq!(ev(",5*2"), 1.0);
        assert_eq!(eval("1,000.5"), Err(CalcError::Syntax), "one separator per number");
        assert_eq!(format(1234567.25, true, true), "1.234.567,25");
        assert_eq!(format(1234567.25, false, true), "1,234,567.25");
        assert_eq!(format(1234567.25, true, false), "1234567,25");
        assert_eq!(format(-0.5, true, true), "-0,5");
        assert_eq!(format(2.0 / 3.0, false, false), "0.6666666667");
        assert_eq!(format(0.1 + 0.2, false, false), "0.3");
        assert_eq!(format(-1e-12 * 0.0, true, true), "0");
        assert_eq!(format(123.0, true, true), "123");
        assert_eq!(format(1e20, false, true), "1e20");
    }

    #[test]
    fn errors() {
        assert_eq!(eval("1/0"), Err(CalcError::DivByZero));
        assert_eq!(eval("5 % 0"), Err(CalcError::DivByZero));
        assert_eq!(eval("(1+2"), Err(CalcError::Syntax));
        assert_eq!(eval("1+"), Err(CalcError::Syntax));
        assert_eq!(eval("1 2"), Err(CalcError::Syntax));
        assert_eq!(eval(""), Err(CalcError::Syntax));
        assert_eq!(eval("2+abc"), Err(CalcError::Syntax));
        assert_eq!(eval("10^400"), Err(CalcError::Range));
    }

    #[test]
    fn what_counts_as_a_sum() {
        for q in ["2+2", "12 * 3", "=42", "(1+2)", "1,5x2", "50%", "-5+1", "2^10"] {
            assert!(looks_like_calc(q), "{q}");
        }
        for q in ["2024", "notepad", "x264", "box 2+2", "timer 10m", "=", "+", "100", "1.5"] {
            assert!(!looks_like_calc(q), "{q}");
        }
    }
}
