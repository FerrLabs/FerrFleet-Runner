#![no_main]
#![allow(dead_code)]

#[path = "../../runner/src/claude_version.rs"]
mod claude_version;

use claude_version::Version;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|raw: &str| {
    if let Some(version) = Version::parse(raw) {
        assert_eq!(Version::parse(&version.to_string()), Some(version));
    }
});
