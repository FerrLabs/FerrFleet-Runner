const BODY_LEN: u32 = 24;

pub fn fake_value(seed: u32) -> String {
    (0..BODY_LEN)
        .map(|i| {
            let digit = (seed * 7 + i * 13 + i * i) % 36;
            let c = char::from_digit(digit, 36).unwrap_or('0');
            if i % 3 == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

pub fn fake_token(kind: &str, seed: u32) -> String {
    format!("{kind}_{}", fake_value(seed))
}
