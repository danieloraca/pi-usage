use crate::{Bundle, Coverage, Record};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Default, Serialize)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub cache_known: bool,
    pub total: u64,
    pub records: usize,
}
impl Totals {
    fn add(&mut self, r: &Record) {
        if self.records == 0 {
            self.cache_known = true;
        }
        self.input += r.counts.input_tokens;
        self.output += r.counts.output_tokens;
        self.cached += r.counts.cached_input_tokens.unwrap_or(0);
        self.cache_known &= r.counts.cached_input_tokens.is_some();
        self.total += r.counts.total();
        self.records += 1;
    }
}
#[derive(Serialize)]
pub struct Day {
    pub date: NaiveDate,
    pub usage: Totals,
}
#[derive(Serialize)]
pub struct Project {
    pub name: String,
    pub usage: Totals,
}
#[derive(Serialize)]
pub struct Report<'a> {
    pub reload: Option<&'a crate::snapshot::ReloadStatus>,
    pub status: &'static str,
    pub demo: bool,
    pub source: &'a str,
    pub generated_at: DateTime<Utc>,
    pub evaluated_at: DateTime<Utc>,
    pub timezone: &'static str,
    pub cloud: &'static str,
    pub days: Option<u32>,
    pub totals: Option<Totals>,
    pub today: Option<Totals>,
    pub sessions: usize,
    pub first_record: Option<DateTime<Utc>>,
    pub last_record: Option<DateTime<Utc>>,
    pub coverage: &'a Coverage,
    pub daily: Vec<Day>,
    pub projects: Vec<Project>,
}

pub fn summarize(bundle: &Bundle, days: Option<u32>, now: DateTime<Utc>) -> Report<'_> {
    let start = days.map(|d| now.date_naive() - Duration::days(i64::from(d.saturating_sub(1))));
    let mut totals = Totals::default();
    let mut today = Totals::default();
    let mut daily: BTreeMap<NaiveDate, Totals> = BTreeMap::new();
    let mut projects: BTreeMap<String, Totals> = BTreeMap::new();
    let mut sessions = HashSet::new();
    for r in &bundle.records {
        if r.timestamp > now || start.is_some_and(|s| r.timestamp.date_naive() < s) {
            continue;
        }
        totals.add(r);
        if r.timestamp.date_naive() == now.date_naive() {
            today.add(r);
        }
        daily.entry(r.timestamp.date_naive()).or_default().add(r);
        projects.entry(r.project.clone()).or_default().add(r);
        sessions.insert(&r.session);
    }
    let mut projects: Vec<_> = projects
        .into_iter()
        .map(|(name, usage)| Project { name, usage })
        .collect();
    projects.sort_by(|a, b| b.usage.total.cmp(&a.usage.total).then(a.name.cmp(&b.name)));
    let status = if bundle.records.is_empty() {
        "unavailable"
    } else if totals.records == 0 {
        "no_records_in_range"
    } else {
        "measured_local_only"
    };
    Report {
        reload: None,
        status,
        demo: bundle.demo,
        source: &bundle.source,
        generated_at: bundle.generated_at,
        evaluated_at: now,
        timezone: "UTC",
        cloud: "unmeasured",
        days,
        totals: (totals.records > 0).then_some(totals),
        today: (today.records > 0).then_some(today),
        sessions: sessions.len(),
        first_record: bundle.records.iter().map(|r| r.timestamp).min(),
        last_record: bundle.records.iter().map(|r| r.timestamp).max(),
        coverage: &bundle.coverage,
        daily: daily
            .into_iter()
            .map(|(date, usage)| Day { date, usage })
            .collect(),
        projects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Counts, Record};
    #[test]
    fn utc_buckets_cache_subset_and_missing_today() {
        let mut b = Bundle::empty("test".into());
        for (id, at) in [
            ("a", "2026-10-29T23:30:00-02:00"),
            ("b", "2026-10-29T23:30:00Z"),
        ] {
            b.records.push(Record {
                id: id.into(),
                session: "s".into(),
                timestamp: at.parse().unwrap(),
                project: "pi".into(),
                counts: Counts {
                    input_tokens: 100,
                    output_tokens: 20,
                    cached_input_tokens: Some(80),
                },
            });
        }
        let r = summarize(&b, Some(1), "2026-10-30T12:00:00Z".parse().unwrap());
        assert_eq!(r.daily.len(), 1);
        assert_eq!(r.totals.unwrap().total, 120);
        assert_eq!(r.today.unwrap().cached, 80);
        let r = summarize(&b, Some(1), "2026-10-31T12:00:00Z".parse().unwrap());
        assert!(r.totals.is_none());
        assert!(r.today.is_none());
        assert_eq!(r.status, "no_records_in_range");
    }
    #[test]
    fn missing_cache_does_not_claim_zero_and_projects_rank() {
        let mut b = crate::ingest::demo();
        b.records[0].counts.cached_input_tokens = None;
        let r = summarize(&b, None, Utc::now() + Duration::days(1));
        assert!(!r.totals.unwrap().cache_known);
        assert!(
            r.projects
                .windows(2)
                .all(|p| p[0].usage.total >= p[1].usage.total)
        );
    }
}
