#![no_main]

use arbitrary::Arbitrary;
use ferrfleet_shared::branch_name;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    prefix: String,
    ticket_ref: String,
}

fuzz_target!(|input: Input| {
    let name = branch_name(&input.prefix, &input.ticket_ref);

    assert!(!name.ends_with('-') || input.prefix.ends_with('-') && name == input.prefix);
    assert!(name.starts_with(input.prefix.trim_end_matches('-')));

    if let Some(tail) = name.get(input.prefix.len()..) {
        assert!(
            tail.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "unsanitized tail {tail:?} in {name:?}"
        );
        assert!(!tail.contains("--"));
    }

    if !input.prefix.starts_with('-') && !input.prefix.is_empty() {
        assert!(!name.starts_with('-'));
    }
});
