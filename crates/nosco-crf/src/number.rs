use crate::{CrfError, is_c_space};

/// Parse a C99 hexadecimal floating-point value or a decimal floating-point
/// value. Hexadecimal significands are rounded directly to binary64, including
/// at the subnormal boundary, with round-to-nearest, ties-to-even.
pub fn parse_hex_float(text: &str) -> Result<f64, CrfError> {
    let text = text.trim_matches(|c: char| c.is_ascii() && is_c_space(c as u8));
    let invalid = || CrfError::InvalidFloat(text.to_owned());
    let (unsigned, sign) = match text.as_bytes().first() {
        Some(b'-') => (&text[1..], 1u64 << 63),
        Some(b'+') => (&text[1..], 0),
        _ => (text, 0),
    };
    let Some(hex) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    else {
        return text.parse().map_err(|_| invalid());
    };
    let (mantissa, exponent) = hex.split_once(['p', 'P']).ok_or_else(invalid)?;
    let exponent = parse_exponent(exponent).ok_or_else(invalid)?;

    let mut top = 0u64;
    let mut kept = 0;
    let mut omitted = 0;
    let mut sticky = false;
    let mut fraction = 0;
    let mut after_dot = false;
    let mut has_digit = false;
    for byte in mantissa.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            b'.' if !after_dot => {
                after_dot = true;
                continue;
            }
            _ => return Err(invalid()),
        };
        has_digit = true;
        fraction += i128::from(after_dot);
        if top == 0 && digit == 0 {
            continue;
        }
        if kept < 16 {
            top = (top << 4) | u64::from(digit);
            kept += 1;
        } else {
            omitted += 1;
            sticky |= digit != 0;
        }
    }
    if !has_digit {
        return Err(invalid());
    }
    if top == 0 {
        return Ok(f64::from_bits(sign));
    }

    let scale = exponent.saturating_add(4 * (omitted - fraction));
    let width = i128::from(u64::BITS - top.leading_zeros());
    let power = scale.saturating_add(width - 1);
    let magnitude = match power {
        1024.. => f64::INFINITY.to_bits(),
        -1022..=1023 => {
            let significand = round_shift(top, width - 53, sticky);
            (((power + 1022) as u64) << 52) + significand
        }
        ..=-1076 => 0,
        _ => round_shift(top, -1074 - scale, sticky),
    };
    Ok(f64::from_bits(sign | magnitude))
}

fn parse_exponent(text: &str) -> Option<i128> {
    let (digits, sign) = match text.as_bytes().first()? {
        b'-' => (&text[1..], -1),
        b'+' => (&text[1..], 1),
        _ => (text, 1),
    };
    if digits.is_empty() {
        return None;
    }
    let mut exponent = 0i128;
    for digit in digits.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        exponent = exponent
            .saturating_mul(10)
            .saturating_add(i128::from(digit - b'0'));
    }
    Some(exponent * sign)
}

fn round_shift(top: u64, shift: i128, sticky: bool) -> u64 {
    if shift <= 0 {
        return top << -shift;
    }
    // Widen so a shift of exactly 64 still sees the rounding bit.
    let top = u128::from(top);
    let rounded_down = top >> shift;
    let half = 1u128 << (shift - 1);
    let remainder = top & ((half << 1) - 1);
    (rounded_down
        + u128::from(remainder > half || (remainder == half && (sticky || rounded_down & 1 != 0))))
        as u64
}
