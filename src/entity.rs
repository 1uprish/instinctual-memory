//! Entity file parsing and rendering.
//!
//! An entity file is Markdown with a YAML frontmatter block. The frontmatter
//! is the structured record; the body is a human-readable rendering.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fact::{Fact, Visibility};

/// OKF §5.2: how the current content was produced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Generated {
    /// Actor in OKF §7 convention (`mem/<version>`, `human:<id>`, `process:<id>`).
    pub by: String,
    /// Last meaningful content change (ISO 8601, explicit offset).
    pub at: DateTime<Utc>,
}

/// OKF §5.2: one independent confirmation against sources/resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Verification {
    pub by: String,
    pub at: DateTime<Utc>,
}

/// Deserialize `verified` accepting a bare mapping as a one-element list (§5.2).
fn deser_verified<'de, D>(d: D) -> std::result::Result<Vec<Verification>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        None,
        One(Verification),
        Many(Vec<Verification>),
    }
    // Missing key is handled by #[serde(default)] before this runs;
    // a present null must also yield empty.
    let v = Option::<OneOrMany>::deserialize(d)?.unwrap_or(OneOrMany::None);
    match v {
        OneOrMany::None => Ok(vec![]),
        OneOrMany::One(one) => Ok(vec![one]),
        OneOrMany::Many(many) => Ok(many),
    }
}

/// OKF §5.4 lifecycle. Absent ⇒ stable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OkfStatus {
    Draft,
    #[default]
    Stable,
    Deprecated,
}

/// OKF §5.1: one material the concept derives from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OkfSource {
    /// Stable key for per-claim footnote attribution (`[^<id>]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// REQUIRED within an entry: URL, bundle-relative `/...` path,
    /// relative path, or scope descriptor.
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Authority signal, actor convention (§7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Adoption/liveness signal, framed by `usage_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_count: Option<u64>,
    /// Recency signal, distinct from `generated.at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<DateTime<Utc>>,
}

/// A parsed entity file.
///
/// Superset of OKF §4.1: `type` is the only required key for conformance;
/// `id`/`schema_version`/`facts` are this tool's extensions (preserved,
/// never rejected per §4.1/§11). Unknown keys land in `extra` and
/// round-trip untouched (e.g. Attested Computation `runtime`/`parameters`/
/// `computation`/`executor`/`attester`, §10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityFile {
    pub schema_version: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<Generated>,
    #[serde(default, deserialize_with = "deser_verified", skip_serializing_if = "Vec::is_empty")]
    pub verified: Vec<Verification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<OkfStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<OkfSource>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub facts: Vec<Fact>,
    pub visibility: Visibility,
    #[serde(default)]
    pub body: String,
    /// Producer-defined extensions + Attested Computation contract (§10).
    /// Flattened so they read/write as top-level frontmatter keys.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl EntityFile {
    /// OKF §5.3 trust tier derived from `verified`.
    pub fn trust_tier(&self) -> &'static str {
        if self.verified.iter().any(|v| v.by.starts_with("human:")) {
            "human-reviewed"
        } else if self.verified.is_empty() {
            "unverified"
        } else {
            "machine-confirmed"
        }
    }
}

impl EntityFile {
    /// Parse an entity file from raw Markdown.
    pub fn parse(content: &str) -> Result<Self> {
        let (frontmatter, body) = split_frontmatter(content)?;
        let mut value: serde_yaml::Value = serde_yaml::from_str(&frontmatter)
            .map_err(|e| Error::Frontmatter {
                path: "<inline>".into(),
                message: e.to_string(),
            })?;
        // Normalise visibility: default to Private if absent.
        if let serde_yaml::Value::Mapping(ref mut map) = value {
            if !map.contains_key("visibility") {
                map.insert("visibility".into(), serde_yaml::Value::from("private"));
            }
        }
        let mut entity: EntityFile = serde_yaml::from_value(value).map_err(|e| Error::Frontmatter {
            path: "<inline>".into(),
            message: e.to_string(),
        })?;
        entity.body = body.to_string();
        for fact in &entity.facts {
            fact.validate()?;
        }
        Ok(entity)
    }

    /// Read an entity file from disk.
    pub fn read(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let mut parsed = Self::parse(&content)?;
        // Override the visibility default when the file does not declare one.
        if !content.contains("visibility:") {
            parsed.visibility = Visibility::Private;
        }
        Ok(parsed)
    }

    /// Render the entity back into Markdown.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        let mut value = serde_yaml::to_value(self).expect("entity is yaml-safe");
        if let serde_yaml::Value::Mapping(ref mut map) = value {
            map.remove("body");
            if matches!(self.visibility, Visibility::Private) {
                // Only suppress explicit visibility if it's the default.
                map.remove("visibility");
            }
        }
        let yaml = serde_yaml::to_string(&value).expect("entity is yaml-safe");
        out.push_str(&yaml);
        out.push_str("---\n\n");
        out.push_str(&self.body);
        if !self.body.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    /// Apply a new fact by id.
    pub fn upsert_fact(&mut self, fact: Fact) {
        for existing in &mut self.facts {
            if existing.id == fact.id {
                *existing = fact;
                return;
            }
        }
        self.facts.push(fact);
    }

    /// Rewrite the Markdown body as the human-readable form of the
    /// frontmatter: the title and every active fact with its id.
    /// Per-claim attribution (§5.1) uses footnotes keyed by concept
    /// `sources[].id`; the footnote label is the join key, never a
    /// positional index, so reordering never misattributes.
    pub fn refresh_body(&mut self) {
        // Keep description fresh when the producer left it empty.
        if self.description.is_none() {
            self.description = self
                .facts
                .iter()
                .find(|f| f.status == crate::fact::FactStatus::Active)
                .map(|f| f.statement.clone());
        }
        let mut out = format!("# {}\n\n## Current facts\n\n", self.title);
        for fact in self.facts.iter().filter(|f| f.status == crate::fact::FactStatus::Active) {
            // Footnote key: first source whose event_id is a concept source id.
            let footnote = fact.sources.iter().find_map(|s| {
                self.sources
                    .iter()
                    .filter_map(|c| c.id.as_deref())
                    .find(|id| *id == s.event_id)
                    .map(|id| format!("[^{id}]"))
            });
            match footnote {
                Some(note) => out.push_str(&format!("- [{}] {}{}\n", fact.id, fact.statement, note)),
                None => out.push_str(&format!("- [{}] {}\n", fact.id, fact.statement)),
            }
        }
        // Footnote definitions for every concept source with an id.
        let defs: Vec<String> = self
            .sources
            .iter()
            .filter_map(|s| {
                s.id.as_deref().map(|id| {
                    format!("[^{id}]: {}", s.title.as_deref().unwrap_or(&s.resource))
                })
            })
            .collect();
        if !defs.is_empty() {
            out.push('\n');
            for d in defs {
                out.push_str(&d);
                out.push('\n');
            }
        }
        self.body = out;
    }

    /// Mark an existing fact as superseded by `new_id`.
    pub fn supersede(&mut self, old_id: &str, new_id: &str) -> Result<()> {
        let mut found = false;
        for fact in &mut self.facts {
            if fact.id == old_id {
                fact.status = crate::fact::FactStatus::Superseded;
                if !fact.supersedes.contains(&new_id.to_string()) {
                    fact.supersedes.push(new_id.to_string());
                }
                found = true;
            }
        }
        if !found {
            return Err(Error::InvalidFact(format!("no fact {old_id}")));
        }
        Ok(())
    }

    /// Mark an existing fact as retracted.
    pub fn retract(&mut self, fact_id: &str) -> Result<()> {
        for fact in &mut self.facts {
            if fact.id == fact_id {
                fact.status = crate::fact::FactStatus::Retracted;
                return Ok(());
            }
        }
        Err(Error::InvalidFact(format!("no fact {fact_id}")))
    }
}

fn split_frontmatter(content: &str) -> Result<(String, String)> {
    let trimmed = content.trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("---\n") else {
        return Err(Error::Frontmatter {
            path: "<inline>".into(),
            message: "file must start with `---`".into(),
        });
    };
    let Some((front, body)) = rest.split_once("\n---\n") else {
        return Err(Error::Frontmatter {
            path: "<inline>".into(),
            message: "frontmatter must be closed with `---`".into(),
        });
    };
    Ok((front.to_string(), body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact::{FactKind, FactStatus};

    #[test]
    fn round_trip() {
        let original = r#"---
schema_version: 2
id: p_alex
type: person
title: Alex Chen
aliases: [Alex, Alex Chen]
links: [o_acme]
facts: []
visibility: private
---

# Alex Chen
"#;
        let parsed = EntityFile::parse(original).unwrap();
        assert_eq!(parsed.id, "p_alex");
        assert_eq!(parsed.title, "Alex Chen");
        assert_eq!(parsed.body.trim(), "# Alex Chen");
    }

    #[test]
    fn parse_fact_block() {
        let content = r#"---
schema_version: 2
id: p_alex
type: person
title: Alex Chen
aliases: []
links: []
facts:
  - id: fact_alex_city_02
    predicate: home_city
    statement: Alex lives in Austin.
    kind: explicit_assertion
    status: active
    observed_at: '2026-09-20T19:41:00Z'
    valid_from: '2026-09-01T00:00:00Z'
    supersedes: []
    sources:
      - event_id: evt_0192
        role: user
        evidence: Alex lives in Austin.
    visibility: private
---

# Alex Chen
"#;
        let parsed = EntityFile::parse(content).unwrap();
        assert_eq!(parsed.facts.len(), 1);
        assert_eq!(parsed.facts[0].kind, FactKind::ExplicitAssertion);
        assert_eq!(parsed.facts[0].status, FactStatus::Active);
    }

    #[test]
    fn reject_missing_frontmatter() {
        let content = "# No frontmatter here\n";
        let err = EntityFile::parse(content).unwrap_err();
        match err {
            Error::Frontmatter { .. } => {}
            other => panic!("expected frontmatter error, got {other}"),
        }
    }

    #[test]
    fn okf_bare_verified_mapping_is_one_element_list() {
        // §5.2: a single verifier MAY be written without the list dash.
        let content = r#"---
schema_version: 2
id: p_alex
type: person
title: Alex Chen
verified: { by: 'human:alex', at: '2026-06-25T09:00:00Z' }
---

# Alex Chen
"#;
        let parsed = EntityFile::parse(content).unwrap();
        assert_eq!(parsed.verified.len(), 1);
        assert_eq!(parsed.verified[0].by, "human:alex");
        assert_eq!(parsed.trust_tier(), "human-reviewed");
    }

    #[test]
    fn okf_trust_tiers() {
        let content = r#"---
schema_version: 2
id: p_alex
type: person
title: Alex Chen
generated: { by: 'mem/1.0.0', at: '2026-06-20T22:53:05Z' }
---

# Alex Chen
"#;
        let parsed = EntityFile::parse(content).unwrap();
        assert_eq!(parsed.trust_tier(), "unverified");
        assert!(parsed.description.is_none());
        assert!(parsed.sources.is_empty());
    }

    #[test]
    fn okf_extra_keys_round_trip() {
        // §4.1/§11: unknown keys (e.g. Attested Computation contract, §10)
        // must be preserved, never rejected.
        let content = r#"---
schema_version: 2
id: ac_revenue
type: Attested Computation
title: Revenue for fiscal year
runtime: bigquery
executor:
  resource: references/skills/run-on-bq.md
  receipt: [job_id, executed_sql, result]
---

# Computation
"#;
        let parsed = EntityFile::parse(content).unwrap();
        assert_eq!(parsed.entity_type, "Attested Computation");
        assert_eq!(
            parsed.extra.get("runtime").and_then(|v| v.as_str()),
            Some("bigquery")
        );
        let rendered = parsed.render();
        assert!(rendered.contains("runtime: bigquery"));
        assert!(rendered.contains("references/skills/run-on-bq.md"));
    }

    #[test]
    fn okf_footnote_body_keys_on_source_id() {
        // §5.1: footnote labels are stable ids, not positional indexes.
        let mut entity = EntityFile::parse(
            r#"---
schema_version: 2
id: p_alex
type: person
title: Alex Chen
sources:
  - id: evt_1
    resource: 'journal:evt_1'
    title: Chat message
---

# Alex Chen
"#,
        )
        .unwrap();
        entity.facts.push(Fact {
            id: "fact_1".into(),
            predicate: "note".into(),
            statement: "Alex likes tea.".into(),
            kind: FactKind::ExplicitAssertion,
            status: FactStatus::Active,
            observed_at: chrono::Utc::now(),
            valid_from: None,
            valid_from_precision: None,
            valid_to: None,
            expires_at: None,
            review_after: None,
            supersedes: vec![],
            sources: vec![crate::fact::SourceRef {
                event_id: "evt_1".into(),
                role: crate::fact::Role::User,
                evidence: "tea".into(),
                author: None,
                last_modified: None,
                usage_count: None,
            }],
            visibility: crate::fact::Visibility::Private,
        });
        entity.refresh_body();
        assert!(entity.body.contains("[^evt_1]"));
        assert!(entity.body.contains("[^evt_1]: Chat message"));
        assert!(entity.description.as_deref() == Some("Alex likes tea."));
    }
}