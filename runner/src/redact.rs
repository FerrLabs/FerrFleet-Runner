const REDACTED: &str = "<redacted>";
const KNOWN_USERNAME: &str = "x-access-token";
const TOKEN_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];
const HEADER_NAMES: [&str; 2] = ["proxy-authorization:", "authorization:"];
const CREDENTIAL_SCHEMES: [&str; 2] = ["bearer", "basic"];

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    Nothing,
    NextWord,
    RestOfLine,
}

pub fn redact(text: &str) -> String {
    text.lines().map(redact_line).collect::<Vec<_>>().join("\n")
}

pub fn tail(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len().saturating_sub(max_lines)..].join("\n");
    let mut start = kept.len().saturating_sub(max_bytes);
    while !kept.is_char_boundary(start) {
        start += 1;
    }
    kept[start..].to_owned()
}

fn redact_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut pending = Pending::Nothing;
    let mut rest = line;
    while !rest.is_empty() {
        let spaces = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..spaces]);
        rest = &rest[spaces..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        rest = tail;
        if word.is_empty() {
            continue;
        }
        match pending {
            Pending::RestOfLine => out.push_str(REDACTED),
            Pending::NextWord => {
                out.push_str(REDACTED);
                pending = Pending::Nothing;
            }
            Pending::Nothing => pending = redact_leading_word(word, &mut out),
        }
    }
    out
}

fn redact_leading_word(word: &str, out: &mut String) -> Pending {
    let lower = word.to_ascii_lowercase();
    for name in HEADER_NAMES {
        if let Some(at) = lower.find(name) {
            let value_start = at + name.len();
            out.push_str(&word[..value_start]);
            if value_start < word.len() {
                out.push_str(REDACTED);
            }
            return Pending::RestOfLine;
        }
    }
    out.push_str(&redact_word(word));
    let bare = lower.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if CREDENTIAL_SCHEMES.contains(&bare) {
        Pending::NextWord
    } else {
        Pending::Nothing
    }
}

fn redact_word(word: &str) -> String {
    redact_tokens(&redact_userinfo(word))
}

fn redact_userinfo(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(at) = rest.find("://") {
        let (head, after) = rest.split_at(at + 3);
        out.push_str(head);
        let authority_end = after
            .find(['/', '?', '#', '\'', '"'])
            .unwrap_or(after.len());
        match after[..authority_end].rfind('@') {
            Some(sign) => {
                out.push_str(&redact_credentials(&after[..sign]));
                rest = &after[sign..];
            }
            None => rest = after,
        }
    }
    out.push_str(rest);
    out
}

fn redact_credentials(userinfo: &str) -> String {
    if userinfo == KNOWN_USERNAME {
        userinfo.to_owned()
    } else if userinfo.starts_with(&format!("{KNOWN_USERNAME}:")) {
        format!("{KNOWN_USERNAME}:{REDACTED}")
    } else {
        REDACTED.to_owned()
    }
}

fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, prefix)) = TOKEN_PREFIXES
        .iter()
        .filter_map(|prefix| rest.find(prefix).map(|at| (at, prefix)))
        .min_by_key(|(at, _)| *at)
    {
        out.push_str(&rest[..at]);
        out.push_str(REDACTED);
        let after = &rest[at + prefix.len()..];
        let run = after
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(after.len());
        rest = &after[run..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{redact, tail};
    use crate::test_support::{fake_token, fake_value};

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            let n = u64::try_from(n).unwrap();
            usize::try_from(self.next() % n).unwrap()
        }

        fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
            xs[self.below(xs.len())]
        }

        fn alnum(&mut self) -> char {
            let digit = u32::try_from(self.below(62)).unwrap();
            match digit {
                0..=9 => char::from_digit(digit, 10).unwrap(),
                10..=35 => char::from_digit(digit, 36).unwrap(),
                _ => char::from_digit(digit - 26, 36)
                    .unwrap()
                    .to_ascii_uppercase(),
            }
        }

        fn sample(&mut self) -> String {
            let n = 8 + self.below(20);
            (0..n).map(|_| self.alnum()).collect()
        }
    }

    const NOISE: &[&str] = &[
        "fatal:",
        "remote:",
        "'",
        "\"",
        "://",
        "@",
        "/",
        ":",
        " ",
        "\n",
        "é",
        "日本",
        "error:",
        "https://github.com/o/r",
        "ghs",
        "_",
        "403",
        "\t",
    ];

    #[test]
    fn redact_never_panics_on_arbitrary_text() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..20_000 {
            let n = rng.below(12);
            let s: String = (0..n).map(|_| rng.pick(NOISE)).collect();
            let _ = redact(&s);
        }
    }

    #[test]
    fn url_password_never_survives() {
        let mut rng = Rng(42);
        for _ in 0..5_000 {
            let credential = rng.sample();
            let url = format!("https://x-access-token:{credential}@github.com/o/r.git");
            let wrap = rng.pick(&["'", "\"", "", "("]);
            let line = format!("{} {wrap}{url}{wrap} {}", rng.pick(NOISE), rng.pick(NOISE));
            let out = redact(&line);
            assert!(!out.contains(&credential), "{line:?} -> {out:?}");
        }
    }

    #[test]
    fn github_token_never_survives_the_sweep() {
        let mut rng = Rng(7);
        let mut leaks = Vec::new();
        for _ in 0..5_000 {
            let sample = format!(
                "{}_{}",
                rng.pick(&["ghp", "ghs", "github_pat"]),
                rng.sample()
            );
            let shape = rng.pick(&[
                "{t}",
                "'{t}'",
                "{t}.",
                "https://{t}@github.com/o/r",
                "token={t}",
                "Bearer\t{t}",
            ]);
            let line = format!("fatal: {}", shape.replace("{t}", &sample));
            let out = redact(&line);
            if out.contains(&sample) {
                leaks.push((line, out));
            }
        }
        assert!(
            leaks.is_empty(),
            "{} of 5000 leaked, first: {:?}",
            leaks.len(),
            leaks.first()
        );
    }

    #[test]
    fn a_token_behind_an_alphanumeric_prefix_is_redacted() {
        let sample = fake_token("ghs", 1);
        let out = redact(&format!("token={sample}"));
        assert!(!out.contains(&sample), "{out}");
    }

    #[test]
    fn a_token_used_as_the_url_username_is_redacted() {
        let sample = fake_token("ghs", 1);
        let out = redact(&format!("https://{sample}@github.com/o/r"));
        assert!(!out.contains(&sample), "{out}");
        assert!(out.ends_with("@github.com/o/r"), "{out}");
    }

    #[test]
    fn a_token_after_a_tab_is_redacted() {
        let sample = fake_token("ghs", 1);
        let out = redact(&format!("Bearer\t{sample}"));
        assert!(!out.contains(&sample), "{out}");
    }

    #[test]
    fn the_known_username_survives_and_its_password_does_not() {
        let credential = fake_value(10);
        assert_eq!(
            redact(&format!(
                "fatal: unable to access 'https://x-access-token:{credential}@github.com/o/r.git/': 403"
            )),
            "fatal: unable to access 'https://x-access-token:<redacted>@github.com/o/r.git/': 403"
        );
        assert_eq!(
            redact("https://x-access-token@github.com/o/r.git"),
            "https://x-access-token@github.com/o/r.git"
        );
    }

    #[test]
    fn any_other_userinfo_is_redacted_whole() {
        let credential = fake_value(11);
        assert_eq!(
            redact(&format!("https://alice:{credential}@host/p")),
            "https://<redacted>@host/p"
        );
        assert_eq!(
            redact(&format!("https://{credential}@host/p")),
            "https://<redacted>@host/p"
        );
    }

    #[test]
    fn every_token_kind_is_redacted_wherever_it_sits() {
        for kind in ["ghp", "gho", "ghu", "ghs", "ghr", "github_pat"] {
            let prefix = format!("{kind}_");
            let out = redact(&format!("a:{prefix}Ab_9zZ,b={prefix}Cd_8yY."));
            assert_eq!(out, "a:<redacted>,b=<redacted>.", "{kind}");
        }
    }

    #[test]
    fn only_the_token_run_is_replaced() {
        assert_eq!(
            redact(&format!("({}) done", fake_token("ghs", 6))),
            "(<redacted>) done"
        );
    }

    #[test]
    fn authorization_header_values_are_redacted_to_the_end_of_the_line() {
        let value = fake_value(12);
        for line in [
            format!("Authorization: Bearer {value}"),
            format!("authorization: basic {value}"),
            format!("> Authorization: token {value}"),
            format!("Authorization:{value}"),
            format!("Proxy-Authorization: Basic {value}"),
        ] {
            let out = redact(&line);
            assert!(!out.contains(&value), "{line} -> {out}");
        }
        assert_eq!(
            redact("Authorization: Bearer x\nnext line"),
            "Authorization: <redacted> <redacted>\nnext line"
        );
    }

    #[test]
    fn bearer_and_basic_credentials_are_redacted() {
        let value = fake_value(13);
        assert_eq!(
            redact(&format!("sent Bearer {value} ok")),
            "sent Bearer <redacted> ok"
        );
        assert_eq!(
            redact(&format!("sent basic {value} ok")),
            "sent basic <redacted> ok"
        );
    }

    #[test]
    fn whitespace_and_clean_text_pass_through_unchanged() {
        let text = "fatal: repository 'file:///x/y.git' does not exist\n\tindented  line";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact(&format!(
            "Bearer {} https://u:p@h/x token={}",
            fake_token("ghs", 7),
            fake_token("ghp", 8)
        ));
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let text = (0..50)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let out = tail(&text, 20, 4096);
        assert_eq!(out.lines().count(), 20);
        assert!(out.starts_with("30\n") && out.ends_with("49"), "{out}");
    }

    #[test]
    fn tail_caps_bytes_on_a_char_boundary() {
        let text = "é".repeat(5000);
        let out = tail(&text, 20, 4096);
        assert!(out.len() <= 4096, "{}", out.len());
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn tail_leaves_short_text_alone() {
        assert_eq!(tail("a\nb", 20, 4096), "a\nb");
    }
}
