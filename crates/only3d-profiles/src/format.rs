//! Number formatting identical to Bambu Studio's placeholder parser output.
//!
//! Bambu (`PlaceholderParser.cpp`, `expr::to_string`) prints doubles with a default
//! `std::ostringstream`, i.e. C `printf("%g")` with precision 6: at most 6 significant digits,
//! trailing zeros removed, scientific notation when the exponent is < -4 or >= 6. Integers are
//! printed with `std::to_string`. Legacy `[var]` expansion uses `ConfigOption::serialize()`, which
//! for floats is the same `ostringstream << value`.
//!
//! Examples: `12.6`, `0.4`, `498.898`, `100` (for `100.0`), `-0.02`, `1e+06`, `1e-05`, `-0`.

/// C `%g` with the given precision (0 is treated as 1, like C).
pub fn format_g(x: f64, precision: usize) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let p = precision.max(1);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let e_repr = format!("{:.*e}", p - 1, x);
    let (mantissa, exp) = e_repr.split_once('e').expect("exponent in {:e} output");
    let exp: i32 = exp.parse().expect("integer exponent");
    if exp < -4 || exp >= p as i32 {
        let m = strip_trailing_zeros(mantissa);
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.unsigned_abs())
    } else {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        strip_trailing_zeros(&format!("{x:.decimals$}")).to_string()
    }
}

/// Bambu's default double formatting (`%g`, precision 6).
pub fn format_float(x: f64) -> String {
    format_g(x, 6)
}

fn strip_trailing_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_printf_g() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (100.0, "100"),
            (12.6, "12.6"),
            (0.1 + 0.2, "0.3"),
            (0.4, "0.4"),
            (-0.02, "-0.02"),
            (25.0 / 2.4053 * 60.0 * 0.8, "498.898"),
            (123456.0, "123456"),
            (1234567.0, "1.23457e+06"),
            (999999.5, "1e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (0.000012345678, "1.23457e-05"),
            (7.123456789, "7.12346"),
            (2.5, "2.5"),
            (1e100, "1e+100"),
            (-123.456789, "-123.457"),
            (0.5, "0.5"),
            (9.9999999, "10"),
        ];
        for (x, want) in cases {
            assert_eq!(format_float(*x), *want, "formatting {x:?}");
        }
    }
}
