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

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap();
        usize::try_from(self.next() % n).unwrap()
    }

    pub fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }

    pub fn alnum(&mut self) -> char {
        let digit = u32::try_from(self.below(62)).unwrap();
        match digit {
            0..=9 => char::from_digit(digit, 10).unwrap(),
            10..=35 => char::from_digit(digit, 36).unwrap(),
            _ => char::from_digit(digit - 26, 36)
                .unwrap()
                .to_ascii_uppercase(),
        }
    }

    pub fn sample(&mut self) -> String {
        let n = 8 + self.below(20);
        (0..n).map(|_| self.alnum()).collect()
    }
}
