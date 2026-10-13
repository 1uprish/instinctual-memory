//! The canonical `INDEX.md`.
//!
//! `INDEX.md` lists every entity in the store with its type, title, and how
//! many active facts it holds. It is published by the writer in the same
//! commit as the entities it describes (consolidation, `change`, `erase`),
//! so it always matches its snapshot. `mem index` republishes it from the
//! current head, and reports the head unchanged when the file is already
//! current. One line per entity keeps it far below the 64 KiB
//! file limit; past that budget the remainder is counted, not listed.

use std::collections::BTreeMap;

use crate::entity::EntityFile;
use crate::error::Result;
use crate::fact::FactStatus;
use crate::repo::{Change, ChangeSet, GitRepo};

/// Path of the index in the store (legacy canonical name).
pub const INDEX_MD: &str = "INDEX.md";
/// OKF §8: lowercase per-directory listing for progressive disclosure.
pub const OKF_INDEX_MD: &str = "index.md";
/// OKF §9: chronological history of updates.
pub const OKF_LOG_MD: &str = "log.md";
/// OKF §12 version this bundle targets.
pub const OKF_VERSION: &str = "0.2";

/// Bytes of entity rows before the rest are summarised.
const INDEX_MD_BUDGET: usize = 60_000;

/// Render `INDEX.md` for a set of entities.
pub fn render_index_md<'a>(entities: impl IntoIterator<Item = &'a EntityFile>) -> String {
    let mut rows: Vec<&EntityFile> = entities.into_iter().collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let mut body = String::from("# Memory index\n\n");
    let mut listed = 0usize;
    for entity in &rows {
        let active = entity.facts.iter().filter(|f| f.status == FactStatus::Active).count();
        let row = format!(
            "- `{}` ({}) {} — {active} active fact{}\n",
            entity.id,
            entity.entity_type,
            entity.title,
            if active == 1 { "" } else { "s" }
        );
        if body.len() + row.len() > INDEX_MD_BUDGET {
            break;
        }
        body.push_str(&row);
        listed += 1;
    }
    if listed < rows.len() {
        body.push_str(&format!("- … and {} more entities\n", rows.len() - listed));
    }
    body
}

/// Render OKF §8 `index.md`: bullets of `[Title](relative-url) - description`.
/// The bundle-root document carries `okf_version` frontmatter (§12) — the
/// only place frontmatter is permitted in an index. Consumers MAY synthesize
/// this on the fly when absent; we publish it so plain `git clone` is enough.
pub fn render_okf_index_md<'a>(
    entities: impl IntoIterator<Item = (&'a str, &'a EntityFile)>,
    bundle_root: bool,
) -> String {
    let mut rows: Vec<(&str, &EntityFile)> = entities.into_iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut body = String::new();
    if bundle_root {
        body.push_str(&format!("---\nokf_version: \"{}\"\n---\n\n", OKF_VERSION));
    }
    body.push_str("# Index\n\n");
    let mut listed = 0usize;
    for (path, entity) in &rows {
        let desc = entity
            .description
            .as_deref()
            .or_else(|| {
                entity
                    .facts
                    .iter()
                    .find(|f| f.status == FactStatus::Active)
                    .map(|f| f.statement.as_str())
            })
            .unwrap_or(&entity.title);
        // One-line description: progressive disclosure, not full text.
        let desc: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        let row = format!("* [{}]({path}) - {desc}\n", entity.title);
        if body.len() + row.len() > INDEX_MD_BUDGET {
            break;
        }
        body.push_str(&row);
        listed += 1;
    }
    if listed < rows.len() {
        body.push_str(&format!("* … and {} more concepts\n", rows.len() - listed));
    }
    body
}

/// Render OKF §9 `log.md`: flat date-grouped entries, newest first,
/// headings in `YYYY-MM-DD` form.
pub fn render_okf_log_md(entries: &[(String, String)]) -> String {
    let mut body = String::from("# Update Log\n\n");
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    body.push_str(&format!("## {today}\n"));
    if entries.is_empty() {
        body.push_str("* **Initialization**: Created memory bundle.\n");
    } else {
        for (kind, text) in entries {
            body.push_str(&format!("* **{kind}**: {text}\n"));
        }
    }
    body
}
/// Render `INDEX.md` for the tree at `base` after replacing (`Some`) or
/// deleting (`None`) the entity files at the given paths.
pub fn index_md_after(
    repo: &GitRepo,
    base: &str,
    updates: &BTreeMap<String, Option<EntityFile>>,
) -> Result<String> {
    let mut by_path: BTreeMap<String, EntityFile> = BTreeMap::new();
    for (path, blob) in repo.list_eligible_entity_files(base)? {
        if let Ok(entity) = EntityFile::parse(&blob.content) {
            by_path.insert(path, entity);
        }
    }
    for (path, update) in updates {
        match update {
            Some(entity) => {
                by_path.insert(path.clone(), entity.clone());
            }
            None => {
                by_path.remove(path);
            }
        }
    }
    Ok(render_index_md(by_path.values()))
}

/// OKF companions for the tree at `base` after `updates`: root `index.md`
/// (§8, with `okf_version`) and `log.md` (§9, one entry per changed concept).
pub fn okf_after(
    repo: &GitRepo,
    base: &str,
    updates: &BTreeMap<String, Option<EntityFile>>,
) -> Result<(String, String)> {
    let mut by_path: BTreeMap<String, EntityFile> = BTreeMap::new();
    for (path, blob) in repo.list_eligible_entity_files(base)? {
        if let Ok(entity) = EntityFile::parse(&blob.content) {
            by_path.insert(path, entity);
        }
    }
    let mut log_entries: Vec<(String, String)> = Vec::new();
    for (path, update) in updates {
        match update {
            Some(entity) => {
                log_entries.push((
                    "Update".to_string(),
                    format!("Updated [{}]({path}).", entity.title),
                ));
                by_path.insert(path.clone(), entity.clone());
            }
            None => {
                log_entries.push((
                    "Deprecation".to_string(),
                    format!("Removed concept at {path}."),
                ));
                by_path.remove(path);
            }
        }
    }
    let pairs: Vec<(&str, &EntityFile)> = by_path
        .iter()
        .map(|(p, e)| (p.as_str(), e))
        .collect();
    Ok((
        render_okf_index_md(pairs, true),
        render_okf_log_md(&log_entries),
    ))
}

/// Republish `INDEX.md` from the current head. Returns the new revision, or
/// the head unchanged when the index is already current.
/// Also publishes OKF `index.md` + `log.md` so the bundle stays conformant.
pub fn rebuild(repo: &GitRepo) -> Result<String> {
    let base = repo.head()?;
    let body = index_md_after(repo, &base, &BTreeMap::new())?;
    let (okf_index, okf_log) = okf_after(repo, &base, &BTreeMap::new())?;
    let current = repo.read_snapshot(&base, INDEX_MD)?.map(|b| b.content);
    let current_okf = repo
        .read_blob_unchecked(&base, OKF_INDEX_MD)?
        .map(|b| b.content);
    let current_log = repo
        .read_blob_unchecked(&base, OKF_LOG_MD)?
        .map(|b| b.content);
    if current.as_deref() == Some(body.as_str())
        && current_okf.as_deref() == Some(okf_index.as_str())
        && current_log.as_deref() == Some(okf_log.as_str())
    {
        return Ok(base);
    }
    let request_id = format!("index_{}", chrono::Utc::now().timestamp_millis());
    let changes = ChangeSet::new(vec![
        (INDEX_MD.to_string(), Change::Write { content: body }),
        (OKF_INDEX_MD.to_string(), Change::Write { content: okf_index }),
        (OKF_LOG_MD.to_string(), Change::Write { content: okf_log }),
    ])?;
    Ok(repo.publish(&base, &request_id, &changes, &crate::validate::DomainValidator)?.revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_publishes_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = GitRepo::create(tmp.path()).unwrap();
        let first = rebuild(&repo).unwrap();
        let head = repo.head().unwrap();
        assert_eq!(first, head);
        let body = repo.read_snapshot(&head, INDEX_MD).unwrap().unwrap().content;
        assert!(body.starts_with("# Memory index"));
        assert_eq!(rebuild(&repo).unwrap(), head, "second rebuild changes nothing");
    }

    #[test]
    fn rows_are_bounded() {
        let entities: Vec<EntityFile> = (0..5000)
            .map(|i| EntityFile {
                schema_version: 2,
                id: format!("w_project_{i:05}"),
                entity_type: "project".into(),
                title: format!("Project number {i} with a fairly long descriptive title"),
                description: None,
                resource: None,
                tags: vec![],
                generated: None,
                verified: vec![],
                status: None,
                stale_after: None,
                sources: vec![],
                aliases: vec![],
                links: vec![],
                facts: vec![],
                visibility: crate::fact::Visibility::Private,
                body: String::new(),
                extra: Default::default(),
            })
            .collect();
        let body = render_index_md(entities.iter());
        assert!(body.len() <= INDEX_MD_BUDGET + 100);
        assert!(body.contains("more entities"));
    }

    #[test]
    fn okf_index_has_version_and_links() {
        use std::collections::BTreeMap;
        let mut extra = BTreeMap::new();
        extra.insert(
            "p_alex".to_string(),
            EntityFile {
                schema_version: 2,
                id: "p_alex".to_string(),
                entity_type: "person".to_string(),
                title: "Alex Chen".to_string(),
                description: Some("Lives in Austin.".to_string()),
                resource: None,
                tags: vec!["person".to_string()],
                generated: None,
                verified: vec![],
                status: None,
                stale_after: None,
                sources: vec![],
                aliases: vec![],
                links: vec![],
                facts: vec![],
                visibility: crate::fact::Visibility::Private,
                body: String::new(),
                extra: Default::default(),
            },
        );
        let pairs: Vec<(&str, &EntityFile)> = extra
            .iter()
            .map(|(p, e)| (p.as_str(), e))
            .collect();
        let body = render_okf_index_md(pairs, true);
        assert!(body.contains("okf_version"));
        assert!(body.contains("[Alex Chen](p_alex) - Lives in Austin."));
    }

    #[test]
    fn okf_log_uses_iso_date_headings() {
        let body = render_okf_log_md(&[("Update".to_string(), "Updated [X](y).".to_string())]);
        assert!(body.contains("## 20"));
        assert!(body.contains("**Update**"));
    }
}
