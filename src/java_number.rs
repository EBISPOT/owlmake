//! Java's number syntax and printing: what `Integer.parseInt`,
//! `Float.parseFloat` and `Double.parseDouble` read, and what `Float.toString`
//! and `Double.toString` write. Literals and DOSDP patterns both take their
//! numbers this way.

/// `s` with what `String.trim` removes gone: every character up to `U+0020`
/// at either end.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// `s` as an `int`, as `Integer.parseInt` reads one: an optional sign and
/// decimal digits of any script, in range.
pub(crate) fn parse_int(s: &str) -> Option<i32> {
    let (negative, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for c in digits.chars() {
        v = v * 10 + i64::from(crate::dosdp::java::decimal_digit(c)?);
        if v > 1 << 31 {
            return None;
        }
    }
    i32::try_from(if negative { -v } else { v }).ok()
}

/// Whether `s` is a floating-point number as `Double.parseDouble` reads one,
/// and its text with any `f`/`d` suffix removed, for Rust to read.
fn java_float_text(s: &str) -> Option<String> {
    let t = trim(s);
    let (sign, body) = match t.strip_prefix(['+', '-']) {
        Some(b) => (&t[..1], b),
        None => ("", t),
    };
    if body == "NaN" || body == "Infinity" {
        return Some(format!("{sign}{}", if body == "NaN" { "NaN" } else { "inf" }));
    }
    let body = body.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(body);
    let lower = body.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        return hex_float(hex).map(|v| format!("{sign}{v:e}"));
    }
    let (mantissa, exponent) = match lower.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (lower.as_str(), None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int.is_empty() && frac.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    if let Some(e) = exponent {
        let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    Some(format!("{sign}{}{}", if int.is_empty() { "0" } else { int }, &mantissa[int.len()..])
        + &exponent.map(|e| format!("e{e}")).unwrap_or_default())
}

/// A hexadecimal floating-point body (after `0x`): hex digits with an optional
/// point, then a binary exponent `p±N`.
fn hex_float(s: &str) -> Option<f64> {
    let (m, e) = s.split_once('p')?;
    let e: i32 = e.parse().ok()?;
    let (int, frac) = m.split_once('.').unwrap_or((m, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut v = 0f64;
    for c in int.chars() {
        v = v * 16.0 + c.to_digit(16)? as f64;
    }
    let mut scale = 1.0 / 16.0;
    for c in frac.chars() {
        v += c.to_digit(16)? as f64 * scale;
        scale /= 16.0;
    }
    Some(v * 2f64.powi(e))
}

/// Whether `Double.parseDouble` reads `s`.
pub(crate) fn is_double(s: &str) -> bool {
    java_float_text(s).is_some_and(|t| t.parse::<f64>().is_ok())
}

/// `s` read as `Float.parseFloat` reads it.
pub(crate) fn parse_float(s: &str) -> Option<f32> {
    java_float_text(s)?.parse::<f32>().ok()
}

/// `s` read as `Double.parseDouble` reads it.
pub(crate) fn parse_double(s: &str) -> Option<f64> {
    java_float_text(s)?.parse::<f64>().ok()
}

/// `v` printed as `Float.toString` prints it.
pub(crate) fn float_to_string(v: f32) -> String {
    let a = v.abs();
    let digits = shortest(&format!("{a:e}"), &format!("{a:.1e}"), || format!("{a:.160e}"), |s| {
        s.parse::<f32>().ok() == Some(a)
    });
    java_decimal(v.is_nan(), v.is_infinite(), v.is_sign_negative(), v == 0.0, &digits)
}

/// `v` printed as `Double.toString` prints it.
pub(crate) fn double_to_string(v: f64) -> String {
    let a = v.abs();
    let digits = shortest(&format!("{a:e}"), &format!("{a:.1e}"), || format!("{a:.800e}"), |s| {
        s.parse::<f64>().ok() == Some(a)
    });
    java_decimal(v.is_nan(), v.is_infinite(), v.is_sign_negative(), v == 0.0, &digits)
}

/// The decimal a finite number prints as, in Rust's `{:e}` form: the closest
/// of the fewest digits that read back as the number (`reads_back`), counting
/// two digits as few as one, and of two equally close the one ending in an
/// even digit. `sci` is the shortest Rust finds, `two` the closest two digits,
/// and `exact` the number's full expansion.
fn shortest(sci: &str, two: &str, exact: impl Fn() -> String, reads_back: impl Fn(&str) -> bool) -> String {
    let Some((m, e)) = sci.split_once('e') else { return sci.to_string() };
    let digits: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 1 {
        return if reads_back(two) { two.to_string() } else { sci.to_string() };
    }
    // Halfway between two of its length, the number lies one digit further
    // out on a 5 and ends there.
    let exact = exact();
    let Some((xm, xe)) = exact.split_once('e') else { return sci.to_string() };
    let full: String = xm.chars().filter(|c| c.is_ascii_digit()).collect();
    let full = full.trim_end_matches('0');
    if xe != e || full.len() != digits.len() + 1 || !full.ends_with('5') {
        return sci.to_string();
    }
    let below = &full[..digits.len()];
    let even = if below.ends_with(['0', '2', '4', '6', '8']) {
        below.to_string()
    } else {
        // One more in the last place, which ends in an even digit: a 9
        // carries and leaves a 0.
        let mut up: Vec<u8> = below.bytes().collect();
        let mut i = up.len();
        loop {
            if i == 0 {
                return sci.to_string();
            }
            i -= 1;
            if up[i] == b'9' {
                up[i] = b'0';
            } else {
                up[i] += 1;
                break;
            }
        }
        String::from_utf8(up).unwrap_or_default()
    };
    let candidate = format!("{}.{}e{e}", &even[..1], &even[1..]);
    if reads_back(&candidate) {
        candidate
    } else {
        sci.to_string()
    }
}

/// The digits of a finite number (`sci`, its magnitude in Rust's `{:e}` form)
/// in Java's layout: plain from 10⁻³ up to 10⁷, else `d.dddE±n`, with at least
/// one digit after the point.
fn java_decimal(nan: bool, infinite: bool, negative: bool, zero: bool, sci: &str) -> String {
    if nan {
        return "NaN".to_string();
    }
    let sign = if negative { "-" } else { "" };
    if infinite {
        return format!("{sign}Infinity");
    }
    if zero {
        return format!("{sign}0.0");
    }
    let (m, e) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = e.parse().unwrap_or(0);
    let digits: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0",
        d => d,
    };
    let body = if (-3..7).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let int: String = digits.chars().chain(std::iter::repeat('0')).take(int_len).collect();
            let frac = if digits.len() > int_len { &digits[int_len..] } else { "0" };
            format!("{int}.{frac}")
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let frac = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{}.{frac}E{exp}", &digits[..1])
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_and_print_as_java_reads_and_prints_them() {
        assert_eq!(parse_int("+5"), Some(5));
        assert_eq!(parse_int("007"), Some(7));
        assert_eq!(parse_int("3000000000"), None);
        assert!(is_double("1e5") && is_double("1.5d") && is_double(".5") && !is_double("inf"));
        assert_eq!(float_to_string(1.5), "1.5");
        assert_eq!(float_to_string(1.0), "1.0");
        assert_eq!(float_to_string(1e10), "1.0E10");
        assert_eq!(float_to_string(0.0001), "1.0E-4");
        assert_eq!(float_to_string(0.001), "0.001");
        assert_eq!(double_to_string(1234567.0), "1234567.0");
        assert_eq!(double_to_string(12345678.0), "1.2345678E7");
        // The fewest digits are two where one digit is not the closest.
        assert_eq!(float_to_string(f32::from_bits(1)), "1.4E-45");
        assert_eq!(double_to_string(f64::from_bits(1)), "4.9E-324");
        assert_eq!(double_to_string(1e23), "1.0E23");
        assert_eq!(trim("\t 1 \u{a0}\n"), "1 \u{a0}");
    }
}
