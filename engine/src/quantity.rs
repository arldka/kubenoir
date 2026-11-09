//! Resource quantities (`500m`, `128Mi`, `1e3`), ported from upstream
//! `k8s.io/apimachinery/pkg/api/resource` (v0.36): `ParseQuantity`, `RoundUp` and the canonical
//! form `MarshalJSON` writes. The API server stores every quantity re-encoded, so `cpu: "0.5"`
//! comes back as `500m` and `memory: 1048576` as `1Mi`; one it parsed exactly keeps the text it
//! was given (`128Mi`, `250m`).
//!
//! Upstream keeps small values as an int64 with a decimal scale and falls back to arbitrary
//! precision; both produce the same canonical digits, so the port keeps one exact decimal
//! (`unscaled × 10^scale` in an i128) and tracks only what changes the output: the original
//! text upstream caches on its int64 path, and the format (decimal SI, binary SI, exponent).

/// Upstream's `resource.Format`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    DecimalExponent,
    BinarySi,
    DecimalSi,
}

#[derive(Clone, Debug)]
pub struct Quantity {
    /// The value is `unscaled × 10^scale`.
    unscaled: i128,
    scale: i32,
    format: Format,
    /// The text upstream keeps and writes back as is.
    cached: Option<String>,
}

/// `maxInt64Factors`: the decimal digits an int64 surely holds.
const MAX_INT64_FACTORS: i32 = 18;
/// `resource.Nano`.
const NANO: i32 = -9;

/// The suffix's (base, exponent, format), or `None` if it is not one (`interpret`).
fn interpret(suffix: &str) -> Option<(u32, i32, Format)> {
    let dec = |e| Some((10, e, Format::DecimalSi));
    let bin = |e| Some((2, e, Format::BinarySi));
    match suffix {
        "" => dec(0),
        "n" => dec(-9),
        "u" => dec(-6),
        "m" => dec(-3),
        "k" => dec(3),
        "M" => dec(6),
        "G" => dec(9),
        "T" => dec(12),
        "P" => dec(15),
        "E" => dec(18),
        "Ki" => bin(10),
        "Mi" => bin(20),
        "Gi" => bin(30),
        "Ti" => bin(40),
        "Pi" => bin(50),
        "Ei" => bin(60),
        s if s.len() > 1 && (s.starts_with('e') || s.starts_with('E')) => {
            s[1..].parse::<i32>().ok().map(|e| (10, e, Format::DecimalExponent))
        }
        _ => None,
    }
}

/// Upstream's `parseQuantityString`: (positive, value, num, denom, suffix).
fn parse_quantity_string(s: &str) -> Option<(bool, &str, &str, &str, &str)> {
    let b = s.as_bytes();
    let end = b.len();
    let mut pos = 0;
    let mut positive = true;
    if pos < end {
        match b[0] {
            b'-' => {
                positive = false;
                pos += 1;
            }
            b'+' => pos += 1,
            _ => {}
        }
    }
    // Leading zeros.
    while pos < end && b[pos] == b'0' {
        pos += 1;
    }
    if pos >= end {
        return Some((positive, "0", "0", "", ""));
    }
    let num_start = pos;
    while pos < end && b[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos >= end {
        return Some((positive, s, &s[num_start..end], "", ""));
    }
    let mut num = &s[num_start..pos];
    if num.is_empty() {
        num = "0";
    }
    let mut denom = "";
    if b[pos] == b'.' {
        pos += 1;
        let denom_start = pos;
        while pos < end && b[pos].is_ascii_digit() {
            pos += 1;
        }
        denom = &s[denom_start..pos];
        if pos >= end {
            return Some((positive, s, num, denom, ""));
        }
    }
    let value = &s[..pos];
    let suffix_start = pos;
    while pos < end && "eEinumkKMGTP".contains(b[pos] as char) {
        pos += 1;
    }
    if pos >= end {
        return Some((positive, value, num, denom, &s[suffix_start..]));
    }
    if b[pos] == b'-' || b[pos] == b'+' {
        pos += 1;
    }
    while pos < end && b[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos >= end {
        return Some((positive, value, num, denom, &s[suffix_start..]));
    }
    None
}

fn pow10(n: u32) -> i128 {
    10i128.pow(n)
}

/// Rounds `unscaled × 10^scale` to `to_scale` (when coarser), away from zero; exact or not.
fn round_up(unscaled: i128, scale: i32, to_scale: i32) -> (i128, i32, bool) {
    if scale >= to_scale {
        return (unscaled, scale, true);
    }
    let div = pow10((to_scale - scale) as u32);
    let (q, r) = (unscaled / div, unscaled % div);
    let q = if r == 0 { q } else { q + unscaled.signum() };
    (q, to_scale, r == 0)
}

impl Quantity {
    /// `ParseQuantity`. `None` for text that is not a quantity.
    pub fn parse(text: &str) -> Option<Quantity> {
        if text.is_empty() {
            return None;
        }
        if text == "0" {
            return Some(Quantity {
                unscaled: 0,
                scale: 0,
                format: Format::DecimalSi,
                cached: Some(text.into()),
            });
        }
        let (positive, _value, num, denom, suffix) = parse_quantity_string(text)?;
        let (base, exponent, format) = interpret(suffix)?;
        let digits: i128 = format!("{num}{denom}").parse().ok()?;
        let sign = if positive { 1 } else { -1 };

        // Upstream's int64 fast path decides whether the original text is kept.
        let precision = match format {
            Format::DecimalExponent | Format::DecimalSi => MAX_INT64_FACTORS - (num.len() + denom.len()) as i32,
            Format::BinarySi if exponent >= 0 && denom.is_empty() => {
                15 - num.len() as i32 - (exponent as f32 * 3.0 / 10.0) as i32 - 1
            }
            Format::BinarySi => -1,
        };
        let int_scale = if format == Format::BinarySi { 0 } else { exponent } - denom.len() as i32;
        if precision >= 0 && int_scale >= NANO {
            let shifted = format!("{num}{denom}");
            let keep = match format {
                Format::BinarySi => exponent % 10 == 0 && (digits & 0x07) != 0,
                _ => int_scale % 3 == 0 && !shifted.ends_with("000") && !shifted.starts_with('0'),
            };
            let unscaled = if format == Format::BinarySi {
                digits << exponent
            } else {
                digits
            };
            if unscaled <= i64::MAX as i128 {
                return Some(Quantity {
                    unscaled: sign * unscaled,
                    scale: int_scale,
                    format,
                    cached: keep.then(|| text.to_string()),
                });
            }
        }

        // Arbitrary precision: exact value, rounded up to nanos.
        let (mut unscaled, mut scale) = if base == 10 {
            (digits, exponent - denom.len() as i32)
        } else {
            (digits << exponent, -(denom.len() as i32))
        };
        if unscaled != 0 {
            (unscaled, scale, _) = round_up(unscaled, scale, NANO);
        }
        let mut format = format;
        if format == Format::BinarySi && unscaled > 0 && Self::lt_one(unscaled, scale) {
            format = Format::DecimalSi;
        }
        Some(Quantity {
            unscaled: sign * unscaled,
            scale,
            format,
            cached: None,
        })
    }

    fn lt_one(unscaled: i128, scale: i32) -> bool {
        if scale >= 0 {
            false
        } else {
            unscaled.abs() < pow10((-scale) as u32)
        }
    }

    /// `RoundUp(scale)`: to that many decimal places at most (rounding away from zero).
    pub fn round_up(&mut self, scale: i32) {
        if self.scale >= scale {
            return;
        }
        self.cached = None;
        (self.unscaled, self.scale, _) = round_up(self.unscaled, self.scale, scale);
    }

    /// What upstream writes for the quantity (`MarshalJSON` / `String`).
    pub fn canonical(&self) -> String {
        if let Some(text) = &self.cached {
            return text.clone();
        }
        if self.unscaled == 0 {
            return "0".into();
        }
        let mut format = self.format;
        if format == Format::BinarySi {
            let whole = self.scale >= 0 || self.unscaled % pow10((-self.scale) as u32) == 0;
            let small = Self::abs_lt(self.unscaled, self.scale, 1024);
            if small || !whole {
                format = Format::DecimalSi;
            }
        }
        if format == Format::BinarySi {
            let mut value = if self.scale >= 0 {
                self.unscaled * pow10(self.scale as u32)
            } else {
                self.unscaled / pow10((-self.scale) as u32)
            };
            let mut exponent = 0;
            while value != 0 && value % 1024 == 0 {
                value /= 1024;
                exponent += 1;
            }
            let suffix = match exponent * 10 {
                0 => "",
                10 => "Ki",
                20 => "Mi",
                30 => "Gi",
                40 => "Ti",
                50 => "Pi",
                60 => "Ei",
                _ => "",
            };
            return format!("{value}{suffix}");
        }
        let mut amount = self.unscaled;
        let mut exponent = self.scale;
        while amount % 10 == 0 {
            amount /= 10;
            exponent += 1;
        }
        while exponent % 3 != 0 {
            amount *= 10;
            exponent -= 1;
        }
        let suffix = match format {
            Format::DecimalExponent if exponent == 0 => String::new(),
            Format::DecimalExponent => format!("e{exponent}"),
            _ => match exponent {
                -9 => "n",
                -6 => "u",
                -3 => "m",
                0 => "",
                3 => "k",
                6 => "M",
                9 => "G",
                12 => "T",
                15 => "P",
                18 => "E",
                _ => "",
            }
            .to_string(),
        };
        format!("{amount}{suffix}")
    }

    /// |value| < n.
    fn abs_lt(unscaled: i128, scale: i32, n: i128) -> bool {
        if scale >= 0 {
            unscaled.abs() * pow10(scale as u32) < n
        } else {
            unscaled.abs() < n * pow10((-scale) as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms_match_upstream() {
        // (text, String(), String() after RoundUp(-3)), printed by upstream v0.36's
        // resource.ParseQuantity.
        for (text, want, rounded) in [
            ("0.5", "500m", "500m"),
            ("500m", "500m", "500m"),
            ("128Mi", "128Mi", "128Mi"),
            ("1048576", "1048576", "1048576"),
            ("1Gi", "1Gi", "1Gi"),
            ("1.5Gi", "1536Mi", "1536Mi"),
            ("1000m", "1", "1"),
            ("1e3", "1e3", "1e3"),
            ("1500", "1500", "1500"),
            ("2000", "2k", "2k"),
            ("0.1", "100m", "100m"),
            ("100Ki", "100Ki", "100Ki"),
            ("1024", "1024", "1024"),
            ("0", "0", "0"),
            ("-1.5", "-1500m", "-1500m"),
            ("12e6", "12e6", "12e6"),
            ("0.0001", "100u", "1m"),
            ("250m", "250m", "250m"),
            ("64Mi", "64Mi", "64Mi"),
            ("1M", "1M", "1M"),
            ("1000000", "1M", "1M"),
            ("2048Ki", "2Mi", "2Mi"),
            ("0.5Gi", "512Mi", "512Mi"),
            ("1.25", "1250m", "1250m"),
            ("3G", "3G", "3G"),
            ("1Ei", "1Ei", "1Ei"),
            ("100u", "100u", "1m"),
            ("1n", "1n", "1m"),
            ("0.0000000001", "1n", "1m"),
            ("7Ki", "7Ki", "7Ki"),
            ("8Ki", "8Ki", "8Ki"),
            ("16Ki", "16Ki", "16Ki"),
            ("1536", "1536", "1536"),
        ] {
            let mut q = Quantity::parse(text).expect("parses");
            assert_eq!(q.canonical(), want, "{text}");
            q.round_up(-3);
            assert_eq!(q.canonical(), rounded, "{text} rounded up");
        }
    }

    #[test]
    fn rejects_garbage() {
        for text in ["", "abc", "1.5.5", "5Xi", "1e", "--1"] {
            assert!(Quantity::parse(text).is_none(), "{text}");
        }
    }
}
