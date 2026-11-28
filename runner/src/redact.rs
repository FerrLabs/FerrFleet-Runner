use std::sync::LazyLock;

use aho_corasick::AhoCorasick;

pub const REDACTED: &str = "<redacted>";
const KNOWN_USERNAME: &str = "x-access-token";
const TOKEN_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];
const HEADER_NAMES: [&str; 2] = ["proxy-authorization:", "authorization:"];
const CREDENTIAL_SCHEMES: [&str; 2] = ["bearer", "basic"];
const MIN_TOKEN_BODY: usize = 8;

static TOKEN_PREFIX_MATCHER: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::new(TOKEN_PREFIXES).expect("the token prefixes are valid patterns")
});

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rules {
    Diagnostic,
    Output,
}

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    Nothing,
    NextWord,
    RestOfLine,
}

pub fn redact(text: &str) -> String {
    redact_with(text, Rules::Diagnostic)
}

pub fn redact_output(text: &str) -> String {
    redact_with(text, Rules::Output)
}

pub fn redact_with(text: &str, rules: Rules) -> String {
    text.split('\n')
        .map(|line| redact_line(line, rules))
        .collect::<Vec<_>>()
        .join("\n")
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

fn redact_line(line: &str, rules: Rules) -> String {
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
            Pending::Nothing => pending = redact_leading_word(word, rules, &mut out),
        }
    }
    out
}

fn redact_leading_word(word: &str, rules: Rules, out: &mut String) -> Pending {
    let lower = word.to_ascii_lowercase();
    for name in HEADER_NAMES {
        if let Some(at) = lower.find(name) {
            let value_start = at + name.len();
            out.push_str(&redact_word(&word[..at], rules));
            out.push_str(&word[at..value_start]);
            if value_start < word.len() {
                out.push_str(REDACTED);
            }
            return Pending::RestOfLine;
        }
    }
    out.push_str(&redact_word(word, rules));
    let bare = lower.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if rules == Rules::Diagnostic && CREDENTIAL_SCHEMES.contains(&bare) {
        Pending::NextWord
    } else {
        Pending::Nothing
    }
}

fn redact_word(word: &str, rules: Rules) -> String {
    redact_tokens(&redact_userinfo(word, rules), rules)
}

fn redact_userinfo(word: &str, rules: Rules) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(at) = rest.find("://") {
        let (head, after) = rest.split_at(at + 3);
        out.push_str(head);
        match userinfo_end(after) {
            Some(sign) => {
                out.push_str(&redact_credentials(&after[..sign], rules));
                rest = &after[sign..];
            }
            None => rest = after,
        }
    }
    out.push_str(rest);
    out
}

fn userinfo_end(after: &str) -> Option<usize> {
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..authority_end];
    if let Some(sign) = authority.rfind('@') {
        return Some(sign);
    }
    if authority.starts_with('[') {
        return None;
    }
    let (_, port) = authority.split_once(':')?;
    if port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let next_scheme = after.find("://").unwrap_or(after.len());
    after
        .get(authority_end..next_scheme)?
        .rfind('@')
        .map(|sign| authority_end + sign)
}

fn redact_credentials(userinfo: &str, rules: Rules) -> String {
    if userinfo == KNOWN_USERNAME {
        userinfo.to_owned()
    } else if userinfo.starts_with(&format!("{KNOWN_USERNAME}:")) {
        format!("{KNOWN_USERNAME}:{REDACTED}")
    } else if rules == Rules::Output && !userinfo.contains(':') {
        userinfo.to_owned()
    } else {
        REDACTED.to_owned()
    }
}

fn redact_tokens(text: &str, rules: Rules) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = TOKEN_PREFIX_MATCHER.find(rest) {
        out.push_str(&rest[..found.start()]);
        let after = &rest[found.end()..];
        let run = after
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(after.len());
        if rules == Rules::Output && !looks_like_a_token(&out, run) {
            out.push_str(&rest[found.start()..found.end()]);
            rest = after;
            continue;
        }
        out.push_str(REDACTED);
        rest = &after[run..];
    }
    out.push_str(rest);
    out
}

fn looks_like_a_token(before: &str, body_len: usize) -> bool {
    let glued = before
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    !glued && body_len >= MIN_TOKEN_BODY
}

#[cfg(test)]
mod tests {
    use super::{Rules, redact, redact_output, redact_with, tail};
    use crate::test_support::{Rng, fake_token, fake_value};

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

    const SWEEP: usize = 100_000;

    fn leak_count(rng: &mut Rng, build: impl Fn(&mut Rng) -> (String, String)) -> usize {
        (0..SWEEP)
            .filter(|_| {
                let (line, needle) = build(rng);
                redact(&line).contains(&needle)
            })
            .count()
    }

    #[test]
    fn a_token_glued_to_an_auth_header_name_never_survives_the_sweep() {
        let mut rng = Rng(11);
        let leaks = leak_count(&mut rng, |rng| {
            let sample = format!("{}_{}", rng.pick(&["ghp", "ghs", "gho"]), rng.sample());
            let shape = rng.pick(&[
                "{t},authorization:",
                "{t}authorization:",
                "x{t}Authorization:y",
                "{t}:proxy-authorization: v",
                "'{t}'authorization:",
            ]);
            (shape.replace("{t}", &sample), sample)
        });
        assert_eq!(leaks, 0, "{leaks} of {SWEEP} leaked");
    }

    #[test]
    fn a_userinfo_password_with_a_raw_quote_never_survives_the_sweep() {
        let mut rng = Rng(13);
        let leaks = leak_count(&mut rng, |rng| {
            let credential = format!("{}'{}", rng.sample(), rng.sample());
            let user = rng.pick(&["u", "x-access-token", "alice"]);
            let wrap = rng.pick(&["", "'", "\"", "("]);
            let line = format!("{wrap}https://{user}:{credential}@host/x{wrap}");
            (line, credential)
        });
        assert_eq!(leaks, 0, "{leaks} of {SWEEP} leaked");
    }

    #[test]
    fn redaction_is_idempotent_over_the_sweep() {
        let mut rng = Rng(17);
        let mut unstable = 0;
        for _ in 0..SWEEP {
            let n = rng.below(12);
            let mut line: String = (0..n).map(|_| rng.pick(NOISE)).collect();
            line.push_str(rng.pick(&["", "\n", "\n\n", "\r\n", " Bearer ", "\nAuthorization: x"]));
            for rules in [Rules::Diagnostic, Rules::Output] {
                let once = redact_with(&line, rules);
                if redact_with(&once, rules) != once {
                    unstable += 1;
                }
            }
        }
        assert_eq!(unstable, 0, "{unstable} of {SWEEP} not idempotent");
    }

    #[test]
    fn a_token_glued_before_an_auth_header_name_is_redacted() {
        let sample = fake_token("ghs", 21);
        let out = redact(&format!("{sample},authorization:"));
        assert_eq!(out, "<redacted>,authorization:");
    }

    #[test]
    fn a_userinfo_password_with_a_raw_quote_is_redacted() {
        let credential = format!("{}'{}", fake_value(22), fake_value(23));
        let out = redact(&format!("https://u:{credential}@host/x"));
        assert_eq!(out, "https://<redacted>@host/x");
    }

    #[test]
    fn a_quoted_url_in_prose_still_loses_its_password() {
        let credential = fake_value(24);
        let out = redact(&format!("fatal: 'https://u:{credential}@host' failed"));
        assert_eq!(out, "fatal: 'https://<redacted>@host' failed");
    }

    #[test]
    fn trailing_newlines_are_preserved() {
        assert_eq!(redact("a\n\n"), "a\n\n");
        assert_eq!(redact("a\n"), "a\n");
        assert_eq!(redact(""), "");
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

    #[test]
    fn userinfo_with_an_unencoded_slash_question_mark_or_hash_is_redacted() {
        let value = fake_value(104);
        for sep in ['/', '?', '#'] {
            let secret = format!("{value}{sep}tail");
            for rules in [Rules::Diagnostic, Rules::Output] {
                let out = redact_with(
                    &format!("https://x-access-token:{secret}@github.com/o/r"),
                    rules,
                );
                assert_eq!(
                    out, "https://x-access-token:<redacted>@github.com/o/r",
                    "{sep} {rules:?}"
                );
                let out = redact_with(&format!("https://alice:{secret}@host/p"), rules);
                assert_eq!(out, "https://<redacted>@host/p", "{sep} {rules:?}");
            }
        }
    }

    #[test]
    fn a_port_or_an_at_sign_in_the_path_is_not_mistaken_for_userinfo() {
        for text in [
            "https://host:8443/a@b",
            "https://registry.example/@scope/pkg",
            "http://[::1]:8080/x@y",
            "https://host/p?q=a@b",
        ] {
            assert_eq!(redact(text), text);
            assert_eq!(redact_output(text), text);
        }
    }

    #[test]
    fn output_rules_keep_a_bare_username_and_diagnostics_do_not() {
        let text = "origin\tssh://git@github.com/o/r.git (fetch)";
        assert_eq!(redact_output(text), text);
        assert_eq!(
            redact(text),
            "origin\tssh://<redacted>@github.com/o/r.git (fetch)"
        );
    }

    #[test]
    fn git_trace_and_curl_verbose_authorization_lines_are_redacted() {
        let value = fake_value(105);
        let trace = format!(
            "12:00:00.000000 http.c:756              => Send header: Authorization: Basic {value}\n\
             12:00:00.000001 http.c:756              => Send header: Accept: */*\n\
             > Authorization: Bearer {value}\n\
             < HTTP/2 200"
        );
        for rules in [Rules::Diagnostic, Rules::Output] {
            let out = redact_with(&trace, rules);
            assert!(!out.contains(&value), "{rules:?}: {out}");
            assert!(out.contains("Send header: Authorization: <redacted> <redacted>\n"));
            assert!(out.contains("Send header: Accept: */*\n"));
            assert!(out.ends_with("< HTTP/2 200"));
        }
    }

    #[test]
    fn output_rules_leave_the_words_basic_and_bearer_alone() {
        let text = "Basic usage: the Bearer of this note is a basic example";
        assert_eq!(redact_output(text), text);
    }

    #[test]
    fn line_endings_survive_byte_for_byte() {
        for text in ["a\n", "a\r\nb\r\n", "\n\n", "x\n\ny\n", ""] {
            assert_eq!(redact(text), text, "{text:?}");
            assert_eq!(redact_output(text), text, "{text:?}");
        }
    }

    #[test]
    fn token_and_userinfo_scans_stay_linear_on_one_huge_word() {
        let word = format!("{},", fake_token("ghp", 106)).repeat(200_000);
        let started = std::time::Instant::now();
        let out = redact_output(&word);
        assert_eq!(out, "<redacted>,".repeat(200_000));
        let schemes = "a://b:c/".repeat(200_000);
        assert_eq!(redact_output(&schemes), schemes);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_rules_only_redact_a_whole_token_shape() {
        for text in [
            "fn highs_and_lows() {}",
            "the ghs_ prefix marks an installation token",
            "let ghp_count = 3;",
        ] {
            assert_eq!(redact_output(text), text);
        }
        let tok = fake_token("ghs", 107);
        assert_eq!(
            redact_output(&format!("GH_TOKEN={tok}")),
            "GH_TOKEN=<redacted>"
        );
        assert_eq!(redact(&format!("x{tok}")), "x<redacted>");
    }
}
