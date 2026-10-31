#![no_main]

use arbitrary::Arbitrary;
use ferrfleet_shared::RunConfig;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    prompt: String,
    stable_prefix: Option<String>,
}

fuzz_target!(|input: Input| {
    let mut config: RunConfig = serde_json::from_value(serde_json::json!({
        "run_id": "01999999-9999-7999-9999-999999999999",
        "agent_id": "fuzz",
        "prompt": input.prompt,
        "working_dir": "/workdir",
    }))
    .unwrap();
    config.stable_prefix = input.stable_prefix;

    let parts = config.prompt_parts();
    assert!(!parts.per_run.is_empty() || config.prompt.is_empty());
    match parts.stable {
        None => assert_eq!(parts.per_run, config.prompt),
        Some(stable) => {
            assert_eq!(config.stable_prefix.as_deref(), Some(stable));
            assert!(config.prompt.starts_with(stable));
            assert!(config.prompt.ends_with(parts.per_run));
            assert!(!parts.per_run.is_empty());
            let middle = &config.prompt[stable.len()..config.prompt.len() - parts.per_run.len()];
            assert!(middle.trim().is_empty());
        }
    }
});
