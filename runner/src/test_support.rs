pub fn fake_token(kind: &str, body: &str) -> String {
    format!("{kind}_{body}")
}

pub fn fake_value(label: &str) -> String {
    format!("{label}-{}", label.len() * 7919)
}
