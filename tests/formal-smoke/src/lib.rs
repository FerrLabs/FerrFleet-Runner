pub fn double(x: u8) -> u16 {
    u16::from(x) * 2
}

#[cfg(test)]
mod tests {
    use super::double;

    #[test]
    fn doubles() {
        assert_eq!(double(21), 42);
    }
}

#[cfg(kani)]
mod proofs {
    use super::double;

    #[kani::proof]
    fn double_never_overflows() {
        let x: u8 = kani::any();
        assert!(double(x) <= 510);
    }
}
