//! Integer arithmetic for `$(( ))`: + - * / % ( ) ! ~ unary +/-, comparisons, && || & | ^ << >>.
//! Names are looked up through a callback and count as 0 when unset or not a number.

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Name(String),
    Op(&'static str),
}

const OPS: [&str; 19] = ["<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "(", ")", "<", ">", "&", "|"];
const OPS_EXTRA: [&str; 3] = ["^", "!", "~"];

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < c.len() && c[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let text: String = c[start..i].iter().collect();
            let v = if let Some(h) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                i64::from_str_radix(h, 16)
            } else if text.len() > 1 && text.starts_with('0') {
                i64::from_str_radix(&text[1..], 8)
            } else {
                text.parse()
            };
            out.push(Tok::Num(v.map_err(|_| format!("invalid number '{text}'"))?));
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Name(c[start..i].iter().collect()));
        } else {
            let rest: String = c[i..].iter().take(2).collect();
            let op = OPS.iter().chain(OPS_EXTRA.iter()).find(|o| rest.starts_with(**o)).ok_or_else(|| format!("unexpected '{ch}' in arithmetic"))?;
            out.push(Tok::Op(op));
            i += op.len();
        }
    }
    Ok(out)
}

fn prec(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | "<=" | ">" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

struct Parser<'a> {
    t: Vec<Tok>,
    p: usize,
    lookup: &'a mut dyn FnMut(&str) -> i64,
}

impl Parser<'_> {
    fn peek_op(&self) -> Option<&'static str> {
        match self.t.get(self.p) {
            Some(Tok::Op(o)) => Some(o),
            _ => None,
        }
    }

    fn expr(&mut self, min: u8) -> Result<i64, String> {
        let mut lhs = self.unary()?;
        while let Some(op) = self.peek_op() {
            let Some(pr) = prec(op) else { break };
            if pr < min {
                break;
            }
            self.p += 1;
            let rhs = self.expr(pr + 1)?;
            lhs = apply(op, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<i64, String> {
        match self.t.get(self.p).cloned() {
            Some(Tok::Op("-")) => {
                self.p += 1;
                Ok(self.unary()?.wrapping_neg())
            }
            Some(Tok::Op("+")) => {
                self.p += 1;
                self.unary()
            }
            Some(Tok::Op("!")) => {
                self.p += 1;
                Ok(i64::from(self.unary()? == 0))
            }
            Some(Tok::Op("~")) => {
                self.p += 1;
                Ok(!self.unary()?)
            }
            Some(Tok::Op("(")) => {
                self.p += 1;
                let v = self.expr(1)?;
                if self.peek_op() != Some(")") {
                    return Err("missing ')' in arithmetic".to_string());
                }
                self.p += 1;
                Ok(v)
            }
            Some(Tok::Num(n)) => {
                self.p += 1;
                Ok(n)
            }
            Some(Tok::Name(n)) => {
                self.p += 1;
                Ok((self.lookup)(&n))
            }
            Some(Tok::Op(o)) => Err(format!("unexpected '{o}' in arithmetic")),
            None => Err("unexpected end of arithmetic expression".to_string()),
        }
    }
}

fn apply(op: &str, a: i64, b: i64) -> Result<i64, String> {
    Ok(match op {
        "||" => i64::from(a != 0 || b != 0),
        "&&" => i64::from(a != 0 && b != 0),
        "|" => a | b,
        "^" => a ^ b,
        "&" => a & b,
        "==" => i64::from(a == b),
        "!=" => i64::from(a != b),
        "<" => i64::from(a < b),
        "<=" => i64::from(a <= b),
        ">" => i64::from(a > b),
        ">=" => i64::from(a >= b),
        "<<" => a.wrapping_shl(b as u32),
        ">>" => a.wrapping_shr(b as u32),
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        "*" => a.wrapping_mul(b),
        "/" | "%" if b == 0 => return Err("division by zero".to_string()),
        "/" => a.wrapping_div(b),
        "%" => a.wrapping_rem(b),
        _ => return Err(format!("unknown operator {op}")),
    })
}

pub fn eval(expr: &str, lookup: &mut dyn FnMut(&str) -> i64) -> Result<i64, String> {
    let t = tokenize(expr)?;
    if t.is_empty() {
        return Ok(0);
    }
    let mut p = Parser { t, p: 0, lookup };
    let v = p.expr(1)?;
    if p.p != p.t.len() {
        return Err("unexpected token in arithmetic expression".to_string());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str) -> Result<i64, String> {
        eval(s, &mut |n| match n {
            "x" => 5,
            "y" => 7,
            _ => 0,
        })
    }

    #[test]
    fn basics_and_precedence() {
        assert_eq!(e("1+2*3"), Ok(7));
        assert_eq!(e("(1+2)*3"), Ok(9));
        assert_eq!(e("10-2-3"), Ok(5), "left associative");
        assert_eq!(e("2*3%4"), Ok(2));
        assert_eq!(e("-3+5"), Ok(2));
        assert_eq!(e("- -3"), Ok(3));
        assert_eq!(e(" 6 / 4 "), Ok(1));
        assert_eq!(e("-7/2"), Ok(-3));
        assert_eq!(e(""), Ok(0));
    }

    #[test]
    fn comparisons_and_logic() {
        assert_eq!(e("1<2"), Ok(1));
        assert_eq!(e("2<=1"), Ok(0));
        assert_eq!(e("3==3 && 4!=5"), Ok(1));
        assert_eq!(e("0 || 0"), Ok(0));
        assert_eq!(e("!0"), Ok(1));
        assert_eq!(e("!5"), Ok(0));
        assert_eq!(e("1+1==2"), Ok(1), "arithmetic binds tighter than ==");
    }

    #[test]
    fn bit_operations_and_literals() {
        assert_eq!(e("6&3"), Ok(2));
        assert_eq!(e("6|3"), Ok(7));
        assert_eq!(e("6^3"), Ok(5));
        assert_eq!(e("1<<4"), Ok(16));
        assert_eq!(e("256>>2"), Ok(64));
        assert_eq!(e("~0"), Ok(-1));
        assert_eq!(e("0x1f"), Ok(31));
        assert_eq!(e("010"), Ok(8));
    }

    #[test]
    fn names() {
        assert_eq!(e("x*y"), Ok(35));
        assert_eq!(e("x+unset"), Ok(5));
        assert_eq!(e("(x+y)/2"), Ok(6));
    }

    #[test]
    fn errors() {
        assert_eq!(e("1/0"), Err("division by zero".into()));
        assert_eq!(e("5%0"), Err("division by zero".into()));
        assert!(e("1+").is_err());
        assert!(e("(1+2").is_err());
        assert!(e("1 2").is_err());
        assert!(e("1 $ 2").is_err());
        assert!(e("09").is_err());
    }
}
