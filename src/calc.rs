//! Calculator: recursive-descent parser over f64.
//! + - * / ^ % (mod), parentheses, unary minus, `,` or `.` as decimal point,
//! functions sqrt abs ln log sin cos tan round floor ceil, constants pi and e.

/// Evaluates `s` if it looks like a calculation (has an operator or a function), not a bare number.
pub fn eval(s: &str) -> Option<f64> {
    let s = s.trim();
    let mathy = s.chars().any(|c| "+-*/^%()".contains(c) || c.is_ascii_alphabetic());
    if s.is_empty() || !mathy || !s.chars().any(|c| c.is_ascii_digit() || c == 'p' || c == 'e') {
        return None;
    }
    let mut p = Parser { s: s.as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

pub fn format(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.10}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v %= self.unary()?;
            } else {
                return Some(v);
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        // Right-associative; the exponent may carry its own sign: 2^-1.
        if self.eat(b'^') { Some(base.powf(self.unary()?)) } else { Some(base) }
    }

    fn atom(&mut self) -> Option<f64> {
        self.ws();
        if self.eat(b'(') {
            let v = self.expr()?;
            return self.eat(b')').then_some(v);
        }
        let start = self.i;
        if self.s.get(self.i)?.is_ascii_alphabetic() {
            while self.i < self.s.len() && self.s[self.i].is_ascii_alphabetic() {
                self.i += 1;
            }
            let name = std::str::from_utf8(&self.s[start..self.i]).ok()?.to_ascii_lowercase();
            return match name.as_str() {
                "pi" => Some(std::f64::consts::PI),
                "e" => Some(std::f64::consts::E),
                _ => {
                    let x = self.atom()?;
                    Some(match name.as_str() {
                        "sqrt" => x.sqrt(),
                        "abs" => x.abs(),
                        "ln" => x.ln(),
                        "log" => x.log10(),
                        "sin" => x.sin(),
                        "cos" => x.cos(),
                        "tan" => x.tan(),
                        "round" => x.round(),
                        "floor" => x.floor(),
                        "ceil" => x.ceil(),
                        _ => return None,
                    })
                }
            };
        }
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || matches!(self.s[self.i], b'.' | b',')) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.replace(',', ".").parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(eval("2+2*2"), Some(6.0));
        assert_eq!(eval("(1+2)^2"), Some(9.0));
        assert_eq!(eval("2^-1"), Some(0.5));
        assert_eq!(eval("-3 + 1,5"), Some(-1.5));
        assert_eq!(eval("10 % 4"), Some(2.0));
        assert_eq!(eval("sqrt(16)+1"), Some(5.0));
        assert_eq!(eval("42"), None);
        assert_eq!(eval("chrome"), None);
        assert_eq!(eval("2+"), None);
        assert_eq!(format(0.1 + 0.2), "0.3");
        assert_eq!(format(1e3), "1000");
    }
}
