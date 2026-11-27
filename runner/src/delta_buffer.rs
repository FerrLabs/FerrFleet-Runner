use ferrfleet_shared::{DeltaKind, ExecutorEvent};
use time::OffsetDateTime;

struct Chunk {
    content: String,
    timestamp: OffsetDateTime,
}

#[derive(Default)]
pub struct DeltaBuffer {
    thinking: Option<Chunk>,
    text: Option<Chunk>,
}

impl DeltaBuffer {
    pub fn append(&mut self, kind: DeltaKind, content: &str, timestamp: OffsetDateTime) {
        let slot = match kind {
            DeltaKind::Thinking => &mut self.thinking,
            DeltaKind::Text => &mut self.text,
        };
        match slot {
            Some(chunk) => chunk.content.push_str(content),
            None => {
                *slot = Some(Chunk {
                    content: content.to_owned(),
                    timestamp,
                });
            }
        }
    }

    pub fn drain(&mut self) -> Vec<ExecutorEvent> {
        [
            (DeltaKind::Thinking, self.thinking.take()),
            (DeltaKind::Text, self.text.take()),
        ]
        .into_iter()
        .filter_map(|(kind, chunk)| {
            chunk.map(|chunk| ExecutorEvent::Delta {
                kind,
                content: chunk.content,
                timestamp: chunk.timestamp,
            })
        })
        .collect()
    }
}
