pub mod agent;
pub mod ingest;
pub mod report;
pub mod snapshot;
pub mod sync;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Counts {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
}

impl Counts {
    pub fn valid(self) -> bool {
        self.input_tokens <= 1_000_000_000_000
            && self.output_tokens <= 1_000_000_000_000
            && self
                .cached_input_tokens
                .is_none_or(|v| v <= self.input_tokens)
    }
    pub fn total(self) -> u64 {
        self.input_tokens + self.output_tokens
    }
    pub fn difference(self, previous: Self) -> Option<Self> {
        Some(Self {
            input_tokens: self.input_tokens.checked_sub(previous.input_tokens)?,
            output_tokens: self.output_tokens.checked_sub(previous.output_tokens)?,
            cached_input_tokens: match (self.cached_input_tokens, previous.cached_input_tokens) {
                (Some(now), Some(before)) => Some(now.checked_sub(before)?),
                _ => None,
            },
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub session: String,
    pub timestamp: DateTime<Utc>,
    pub project: String,
    pub counts: Counts,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Coverage {
    pub files_read: usize,
    pub unreadable: usize,
    pub malformed: usize,
    pub incomplete_lines: usize,
    pub duplicates: usize,
    pub conflicts: usize,
    pub skipped_usage: usize,
    pub excluded_history: usize,
    pub legacy_suppressed: usize,
    pub counter_resets: usize,
    pub unknown_baselines: usize,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Bundle {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub demo: bool,
    pub source: String,
    pub coverage: Coverage,
    pub records: Vec<Record>,
}

impl Bundle {
    pub fn empty(source: String) -> Self {
        Self {
            schema_version: 1,
            generated_at: Utc::now(),
            demo: false,
            source,
            coverage: Coverage::default(),
            records: Vec::new(),
        }
    }
}
