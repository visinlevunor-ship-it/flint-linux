//! fnmatch-style patterns as used in modules.alias: `*`, `?` and `[...]` (with `!`/`^` and
//! ranges). Matching is iterative, so long aliases with many `*` stay fast.

/// The text before the first wildcard, for quick filtering.
pub fn literal_prefix(p: &str) -> &str {
    let end = p.find(['*', '?', '[']).unwrap_or(p.len());
    &p[..end]
}

fn class_matches(class: &[u8], c: u8) -> (bool, usize) {
    // class starts just after '['; returns (matched, length including the closing ']')
    let mut i = 0;
    let negate = matches!(class.first(), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < class.len() {
        let x = class[i];
        if x == b']' && !first {
            return (matched != negate, i + 1);
        }
        first = false;
        if i + 2 < class.len() && class[i + 1] == b'-' && class[i + 2] != b']' {
            if class[i] <= c && c <= class[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if x == c {
                matched = true;
            }
            i += 1;
        }
    }
    // unterminated: treat '[' literally
    (false, usize::MAX)
}

pub fn matches(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                b'[' => {
                    let (ok, len) = class_matches(&p[pi + 1..], t[ti]);
                    if len == usize::MAX {
                        if t[ti] == b'[' {
                            pi += 1;
                            ti += 1;
                            continue;
                        }
                    } else if ok {
                        pi += 1 + len;
                        ti += 1;
                        continue;
                    }
                }
                c if c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, st)) => {
                pi = sp + 1;
                ti = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_alias_patterns() {
        let pci = "pci:v00008086d00002723sv00008086sd00000084bc02sc80i00";
        assert!(matches("pci:v00008086d00002723sv*sd*bc*sc*i*", pci));
        assert!(!matches("pci:v00008086d00002724sv*sd*bc*sc*i*", pci));
        assert!(matches("pci:v*d*sv*sd*bc02sc80i*", pci));
        let usb = "usb:v046Dp0825d0010dcEFdsc02dp01ic01isc01ip00in00";
        assert!(matches("usb:v*p*d*dc*dsc*dp*ic01isc01ip*in*", usb));
        assert!(matches("usb:v046Dp0825d*dc*dsc*dp*ic*isc*ip*in*", usb));
        assert!(matches("usb:v046Dp08[0-2]?d*", usb));
        assert!(!matches("usb:v046Dp08[3-9]?d*", usb));
        assert!(matches("acpi*:PNP0C50:*", "acpi:PNP0C50:"));
        assert!(matches("hid:b0003g*v*p*", "hid:b0003g0001v0000046Dp0000C52B"));
    }

    #[test]
    fn edge_cases() {
        assert!(matches("*", ""));
        assert!(matches("a*", "a"));
        assert!(!matches("a?", "a"));
        assert!(matches("[!a]x", "bx"));
        assert!(!matches("[!a]x", "ax"));
        assert!(matches("[]]", "]"));
        assert!(matches("a[", "a["), "an unterminated [ is literal");
        assert!(matches("*a*b*c*", "xxaxxbxxcxx"));
        assert!(!matches("*a*b*c*", "xxaxxcxxbxx"));
        assert_eq!(literal_prefix("pci:v00008086d*sv"), "pci:v00008086d");
        assert_eq!(literal_prefix("plain"), "plain");
    }

    #[test]
    fn many_stars_stay_fast() {
        let text = "a".repeat(5000);
        let pat = format!("{}b", "*a".repeat(50));
        let start = std::time::Instant::now();
        assert!(!matches(&pat, &text));
        assert!(start.elapsed().as_millis() < 2000);
    }
}
