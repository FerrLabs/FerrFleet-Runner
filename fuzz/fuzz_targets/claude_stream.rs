#![no_main]
#![allow(dead_code)]

#[path = "../../runner/src/claude_stream.rs"]
mod claude_stream;
#[path = "../../runner/src/usage.rs"]
mod usage;

use ferrfleet_shared::ExecutorEvent;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fn images_are_elided(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(images_are_elided),
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image") {
                return map
                    .get("source")
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str)
                    == Some("elided");
            }
            map.values().all(images_are_elided)
        }
        _ => true,
    }
}

fuzz_target!(|data: &str| {
    let mut ledger = usage::UsageLedger::default();
    for line in data.lines() {
        let Ok(events) = claude_stream::translate(line, &mut ledger) else {
            continue;
        };
        for event in &events {
            if let ExecutorEvent::ToolResult { output, .. } = event {
                assert!(
                    images_are_elided(output),
                    "image payload survived: {output}"
                );
            }
        }
    }
});
