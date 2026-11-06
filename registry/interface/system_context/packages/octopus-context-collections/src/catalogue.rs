use octopus_ledger::{ArtifactRef, ContentHash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    BoundCollection, CollectionCursor, CollectionError, CollectionSelection, PublicationState,
    Result, ScopeContext,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueLimits {
    pub max_sources: usize,
    pub max_entries: usize,
    pub max_snapshot_bytes: usize,
    pub max_scan_bytes: u64,
}

impl Default for CatalogueLimits {
    fn default() -> Self {
        Self {
            max_sources: 32,
            max_entries: 64,
            max_snapshot_bytes: 65536,
            max_scan_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactEntry {
    pub pin: CollectionSelection,
    pub title: String,
    pub description: String,
    pub applicability: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactCollectionPage {
    pub source: ArtifactRef,
    pub description: String,
    pub snapshot: String,
    pub entries: Vec<CompactEntry>,
    pub has_more: bool,
    pub cursor: Option<CollectionCursor>,
    pub scope_limited: bool,
}

impl CompactCollectionPage {
    fn shorten(&mut self) -> Result<()> {
        self.entries
            .pop()
            .ok_or(CollectionError::LimitExceeded("catalogue source headers"))?;
        self.has_more = true;
        self.cursor = self
            .entries
            .last()
            .map(|entry| {
                Ok::<_, CollectionError>(CollectionCursor {
                    snapshot: ContentHash::from_hex(&self.snapshot)
                        .ok_or(CollectionError::SourceMismatch)?,
                    after: entry.pin.entry_id.clone(),
                })
            })
            .transpose()?;
        if self.scope_limited {
            self.cursor = None;
        }
        Ok(())
    }
}

impl BoundCollection {
    pub fn catalogue_page(
        &self,
        target: &ScopeContext,
        author: Option<&ScopeContext>,
        limit: u32,
        scan_budget: u64,
    ) -> Result<(CompactCollectionPage, u64)> {
        crate::validate_scope_context(target)?;
        if let Some(author) = author {
            crate::validate_scope_context(author)?;
        }
        if limit == 0 || limit > self.descriptor.limits.max_entries {
            return Err(CollectionError::LimitExceeded("catalogue page entry count"));
        }
        let mut page = CompactCollectionPage {
            source: self.source.clone(),
            description: self.descriptor.description.clone(),
            snapshot: ContentHash::of_bytes(&[]).to_hex(),
            entries: Vec::new(),
            has_more: false,
            cursor: None,
            scope_limited: author.is_some_and(|scope| scope != target),
        };
        let mut revisions = Vec::new();
        let mut full = false;
        let scanned = self.scan_bounded(scan_budget, |record| {
            let entry = &record.entry;
            if entry.state != PublicationState::Active || !entry.scope.permits(target) {
                return Ok(());
            }
            if author.is_some_and(|scope| !entry.scope.permits(scope)) {
                return Ok(());
            }
            revisions.push((entry.id.clone(), record.revision.to_hex()));
            if full || page.entries.len() >= limit as usize {
                page.has_more = true;
                return Ok(());
            }
            page.entries.push(CompactEntry {
                pin: CollectionSelection {
                    source: self.source.clone(),
                    entry_id: entry.id.clone(),
                    revision: record.revision,
                },
                title: entry.title.clone(),
                description: entry.description.clone(),
                applicability: entry.applicability.clone(),
            });
            if serde_json::to_vec(&page)?.len() as u64 > self.descriptor.limits.max_page_bytes {
                page.entries.pop();
                page.has_more = true;
                full = true;
            }
            Ok(())
        })?;
        let mut view = json!({"source":self.source,"scope":target,"query":"","entries":revisions});
        if page.scope_limited {
            view["author_scope"] = json!(author);
        }
        let snapshot = ContentHash::of_canonical_json(&view);
        page.snapshot = snapshot.to_hex();
        if page.has_more && !page.scope_limited {
            page.cursor = page.entries.last().map(|entry| CollectionCursor {
                snapshot,
                after: entry.pin.entry_id.clone(),
            });
        }
        Ok((page, scanned.bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UninspectedBinding {
    pub id: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionCatalogue {
    pub schema_version: u32,
    pub scope: ScopeContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_scope: Option<ScopeContext>,
    pub limits: CatalogueLimits,
    pub sources: Vec<CompactCollectionPage>,
    pub uninspected_bindings: Vec<UninspectedBinding>,
}

impl CollectionCatalogue {
    pub fn descriptor(mut self) -> Result<Value> {
        crate::validate_scope_context(&self.scope)?;
        if let Some(author) = &self.author_scope {
            crate::validate_scope_context(author)?;
        }
        if self.schema_version != 1
            || self.limits.max_sources == 0
            || self.limits.max_entries == 0
            || self.limits.max_snapshot_bytes == 0
            || self.limits.max_scan_bytes == 0
            || self.sources.len() > self.limits.max_sources
        {
            return Err(CollectionError::LimitExceeded("catalogue limits"));
        }
        loop {
            let value = self.encode()?;
            if self
                .sources
                .iter()
                .map(|page| page.entries.len())
                .sum::<usize>()
                <= self.limits.max_entries
                && serde_json::to_vec(&value)?.len() <= self.limits.max_snapshot_bytes
            {
                return Ok(value);
            }
            self.sources
                .iter_mut()
                .rev()
                .find(|page| !page.entries.is_empty())
                .ok_or(CollectionError::LimitExceeded("catalogue source headers"))?
                .shorten()?;
        }
    }

    fn encode(&self) -> Result<Value> {
        let text = format!(
            "Published knowledge captured when this task was authored. Titles, descriptions and applicability below are authored working context; judge relevance yourself. Detailed content and capability activation require explicit read and authoring operations. A page with has_more is incomplete. scope_limited means capture used the intersection of different declared author and target scopes; it does not establish whether other entries exist. Use context.discover with the exact source object and cursor shown here, or start without a cursor when none is provided; pass an entry pin to context.read verbatim. Uninspected bindings were not read by the author and do not establish an empty collection. This accepted snapshot is retained on continuation; discover explicitly to inspect later publications.\n{}",
            serde_json::to_string(self)?,
        );
        let mut value = json!({"label":"context.reference","description":"Bounded published knowledge captured when this task was authored",
            "text":text,"collection_catalogue":self});
        value["id"] = json!(format!(
            "artifact-collection-catalogue-{}",
            ContentHash::of_canonical_json(&value).to_hex()
        ));
        Ok(value)
    }

    pub fn validate_descriptor(value: &Value) -> Result<Self> {
        let catalogue: Self = serde_json::from_value(
            value
                .get("collection_catalogue")
                .ok_or(CollectionError::SourceMismatch)?
                .clone(),
        )?;
        if catalogue.clone().descriptor()? != *value {
            return Err(CollectionError::SourceMismatch);
        }
        Ok(catalogue)
    }
}
