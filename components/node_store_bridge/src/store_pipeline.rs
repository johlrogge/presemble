use std::collections::HashMap;

use node_store::{Edge, Node, NodeId, NodeStore};

/// Maps page URLs to their semantic content root NodeIds.
pub type NodeUrlIndex = HashMap<String, NodeId>;

/// Maps stems to lists of (url, semantic_root_id) pairs for item pages.
pub type NodeStemIndex = HashMap<String, Vec<(String, NodeId)>>;

/// Build URL and stem indexes from semantic content roots in the NodeStore.
///
/// `roots` is a list of (url, semantic_root_id) pairs.
/// Only "item" pages go into the indexes (not "collection" pages).
pub fn build_indexes_from_store(
    store: &NodeStore,
    roots: &[(String, NodeId)],
) -> (NodeUrlIndex, NodeStemIndex) {
    let mut url_index = NodeUrlIndex::new();
    let mut stem_index = NodeStemIndex::new();

    for (url, sem_root) in roots {
        let page_kind = get_attribute_text(store, *sem_root, "page-kind");
        if page_kind.as_deref() != Some("item") {
            continue;
        }

        url_index.insert(url.clone(), *sem_root);

        if let Some(stem) = get_attribute_text(store, *sem_root, "stem") {
            stem_index
                .entry(stem)
                .or_default()
                .push((url.clone(), *sem_root));
        }
    }

    (url_index, stem_index)
}

// ── Link-expression evaluation (ADR-058) ─────────────────────────────────────

pub use expressions::normalise_url;

/// Element name of a stem node: one per schema stem, the target of every
/// query over that stem's items.
pub const STEM_NODE_KIND: &str = "stem-items";
/// Element name of a query node: one per thread expression in a preamble slot.
pub const QUERY_NODE_KIND: &str = "link-query";
/// `Derived` edge from a page's semantic root to each of its query nodes.
pub const QUERY_EDGE: &str = "query";
/// `Derived` edge from a query node to the stem node it reads.
pub const OVER_EDGE: &str = "over";
/// `Derived` edge from a document root to its stored preamble fingerprint.
pub const FINGERPRINT_EDGE: &str = "preamble-fingerprint";

/// The store adapter for [`expressions::ThreadItem`]: a page's semantic root.
pub struct SemItem<'a> {
    pub store: &'a NodeStore,
    pub sem_root: NodeId,
    pub url: String,
}

impl expressions::ThreadItem for SemItem<'_> {
    fn url(&self) -> &str {
        &self.url
    }

    /// Follows the ConsistsOf edge named `field`, then the Reference edge,
    /// from the semantic root. A Text node gives its text; a heading,
    /// paragraph or link element gives its first Text child; anything else
    /// gives `None`. Mirrors `template::Value::display_text`.
    fn field_text(&self, field: &str) -> Option<String> {
        let store = self.store;
        let target = store
            .consists_of(self.sem_root)
            .into_iter()
            .chain(store.references(self.sem_root))
            .find(|(name, _)| store.resolve_name(*name) == field)
            .map(|(_, target)| target)?;
        match store.get(target)? {
            Node::Text(s) => Some(s.clone()),
            Node::Element(name)
                if matches!(store.resolve_name(*name), "heading" | "paragraph" | "link") =>
            {
                store.children(target).into_iter().find_map(|c| match store.get(c) {
                    Some(Node::Text(s)) => Some(s.clone()),
                    _ => None,
                })
            }
            _ => None,
        }
    }
}

/// The normalised URLs `doc_root` declares a reference to.
///
/// From preamble slots: `link` hrefs and `PathRef` link expressions. From
/// `preamble > unassigned`: `link` hrefs, `PathRef` link expressions and the
/// inline link destinations of every paragraph. Every destination goes
/// through [`expressions::page_reference`], so external URLs, anchors and
/// expressions are not page references. Images and thread expressions never
/// count; the body is not read (see [`links_to_index`]).
pub fn declared_references(store: &NodeStore, doc_root: NodeId) -> Vec<String> {
    use crate::content_bridge::{find_child_by_name, slot_element_target, SlotElementTarget};

    let Some(preamble) = find_child_by_name(store, doc_root, "preamble") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for container in store.children(preamble) {
        let unassigned = match element_name(store, container).as_deref() {
            Some("slot") => false,
            Some("unassigned") => true,
            _ => continue,
        };
        for el in store.children(container) {
            match slot_element_target(store, el) {
                Some(SlotElementTarget::Url { raw, .. }) => {
                    out.extend(expressions::page_reference(&raw));
                }
                Some(SlotElementTarget::Thread { .. }) => {}
                None if unassigned && element_name(store, el).as_deref() == Some("paragraph") => {
                    let text = store.children(el).into_iter().find_map(|c| match store.get(c) {
                        Some(Node::Text(s)) => Some(s.clone()),
                        _ => None,
                    });
                    if let Some(text) = text {
                        out.extend(
                            content::paragraph_link_destinations(&text)
                                .iter()
                                .filter_map(|d| expressions::page_reference(d)),
                        );
                    }
                }
                None => {}
            }
        }
    }
    out
}

/// Build the reference index: target URL → the URLs of the pages that
/// declare a reference to it. `docs` are `(url, document root)` pairs.
///
/// Every declared reference from every parsed page counts, publishable or not
/// (ADR-058 §8). Intentionally incomplete until S-E / ADR-059: body and
/// inline references are not yet counted. Pages that fail to parse are not in
/// the store and are skipped.
///
/// This is the only edge collector: `refs-to` in link expressions and the
/// REPL `refs-to` / `refs-from` primitives all read it.
pub fn links_to_index(store: &NodeStore, docs: &[(String, NodeId)]) -> expressions::LinksToIndex {
    let mut index = expressions::LinksToIndex::new();
    for (url, root) in docs {
        let source = normalise_url(url);
        for target in declared_references(store, *root) {
            index.entry(target).or_default().insert(source.clone());
        }
    }
    index
}

/// Everything thread-expression evaluation reads, computed once per build or
/// edit.
#[derive(Debug, Clone, Default)]
pub struct ThreadContext {
    /// Stem → `(url, semantic root)` of its item pages, URL ascending (the
    /// base order, ADR-058 D2). Only `page-kind == "item"` pages.
    pub items_by_stem: HashMap<String, Vec<(String, NodeId)>>,
    /// Declared references of every document (see [`links_to_index`]).
    pub links_to: expressions::LinksToIndex,
    /// URL → semantic root, for resolving `link` and `PathRef` targets.
    pub url_to_semantic: HashMap<String, NodeId>,
    /// URL → document root, the fallback for targets without a semantic root.
    pub url_to_root: HashMap<String, NodeId>,
    /// Stem → stem node (see [`create_stem_node`]).
    pub stem_nodes: HashMap<String, NodeId>,
    /// Page URL → the query nodes recorded for it (any semantic root).
    queries_by_page: HashMap<String, Vec<NodeId>>,
}

impl ThreadContext {
    /// Build the context from the live indexes.
    pub fn build(
        store: &NodeStore,
        url_to_semantic: HashMap<String, NodeId>,
        url_to_root: HashMap<String, NodeId>,
        stem_nodes: HashMap<String, NodeId>,
    ) -> Self {
        let mut items_by_stem: HashMap<String, Vec<(String, NodeId)>> = HashMap::new();
        for (url, &sem) in &url_to_semantic {
            if get_attribute_text(store, sem, "page-kind").as_deref() != Some("item") {
                continue;
            }
            if let Some(stem) = get_attribute_text(store, sem, "stem") {
                items_by_stem.entry(stem).or_default().push((normalise_url(url), sem));
            }
        }
        for items in items_by_stem.values_mut() {
            items.sort_by(|a, b| a.0.cmp(&b.0));
        }

        let mut docs: Vec<(String, NodeId)> =
            url_to_root.iter().map(|(u, &r)| (u.clone(), r)).collect();
        docs.sort();
        let links_to = links_to_index(store, &docs);

        let mut queries_by_page: HashMap<String, Vec<NodeId>> = HashMap::new();
        if let Some(over) = store.lookup_name(OVER_EDGE) {
            for &stem_node in stem_nodes.values() {
                for q in store.derived_sources(stem_node, over) {
                    if let Some(page) = get_attribute_text(store, q, "page") {
                        queries_by_page.entry(page).or_default().push(q);
                    }
                }
            }
        }

        ThreadContext {
            items_by_stem,
            links_to,
            url_to_semantic,
            url_to_root,
            stem_nodes,
            queries_by_page,
        }
    }
}

/// Evaluate a thread expression `->> :source ops…` on page `page_url`:
/// the source's items in URL order through [`expressions::apply_link_ops`].
/// Returns `(url, semantic root)` pairs in result order.
pub fn evaluate_thread(
    store: &NodeStore,
    ctx: &ThreadContext,
    source: &str,
    ops: &[content::LinkOp],
    page_url: &str,
) -> Vec<(String, NodeId)> {
    let items: Vec<SemItem<'_>> = ctx
        .items_by_stem
        .get(source)
        .map(|items| {
            items
                .iter()
                .map(|(url, sem)| SemItem { store, sem_root: *sem, url: url.clone() })
                .collect()
        })
        .unwrap_or_default();
    expressions::apply_link_ops(items, ops, &normalise_url(page_url), &ctx.links_to)
        .into_iter()
        .map(|item| (item.url, item.sem_root))
        .collect()
}

/// A thread expression in a preamble slot of a document.
#[derive(Debug, Clone, PartialEq)]
pub struct PreambleThread {
    /// The slot it fills.
    pub slot: String,
    /// Its index among the thread expressions of that slot.
    pub ordinal: usize,
    /// The source stem.
    pub source: String,
    /// The operations.
    pub ops: Vec<content::LinkOp>,
}

/// The thread expressions in the preamble slots of `doc_root`, in document
/// order. Thread expressions in `unassigned` are not included.
pub fn preamble_threads(store: &NodeStore, doc_root: NodeId) -> Vec<PreambleThread> {
    use crate::content_bridge::{children_by_name, find_attr_text, find_child_by_name};
    use crate::content_bridge::{slot_element_target, SlotElementTarget};

    let Some(preamble) = find_child_by_name(store, doc_root, "preamble") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for slot in children_by_name(store, preamble, "slot") {
        let name = find_attr_text(store, slot, "name").unwrap_or_default();
        let mut ordinal = 0;
        for el in store.children(slot) {
            if let Some(SlotElementTarget::Thread { source, ops }) = slot_element_target(store, el) {
                out.push(PreambleThread { slot: name.clone(), ordinal, source, ops });
                ordinal += 1;
            }
        }
    }
    out
}

/// Create a stem node for `stem`: `Element("stem-items")` with a `stem`
/// attribute. Every query over `stem` has a `Derived { "over" }` edge to it.
pub fn create_stem_node(store: &mut NodeStore, stem: &str) -> NodeId {
    let name = store.intern(STEM_NODE_KIND);
    let node = store.add_node(Node::Element(name));
    crate::content_bridge::add_text_attr(store, node, "stem", stem);
    node
}

/// The result URLs last stored on query node `q`.
pub fn query_result(store: &NodeStore, q: NodeId) -> Vec<String> {
    store
        .children(q)
        .into_iter()
        .filter_map(|c| match store.get(c) {
            Some(Node::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The query nodes over `stem_node`.
pub fn queries_over(store: &NodeStore, stem_node: NodeId) -> Vec<NodeId> {
    store
        .lookup_name(OVER_EDGE)
        .map(|over| store.derived_sources(stem_node, over))
        .unwrap_or_default()
}

/// The page and thread expression query node `q` evaluates. `None` when `q`
/// is not a query node, or its page or expression is gone.
pub fn query_thread(
    store: &NodeStore,
    ctx: &ThreadContext,
    q: NodeId,
) -> Option<(String, PreambleThread)> {
    let page = get_attribute_text(store, q, "page")?;
    let slot = get_attribute_text(store, q, "slot")?;
    let ordinal: usize = get_attribute_text(store, q, "ordinal")?.parse().ok()?;
    let doc_root = *ctx.url_to_root.get(&page)?;
    let thread = preamble_threads(store, doc_root)
        .into_iter()
        .find(|t| t.slot == slot && t.ordinal == ordinal)?;
    Some((page, thread))
}

/// Re-evaluate query node `q` against `ctx` without changing the store.
/// Returns `None` when `q` is not a query node or its page is unknown.
pub fn reevaluate_query(store: &NodeStore, ctx: &ThreadContext, q: NodeId) -> Option<Vec<String>> {
    let (page, thread) = query_thread(store, ctx, q)?;
    Some(
        evaluate_thread(store, ctx, &thread.source, &thread.ops, &page)
            .into_iter()
            .map(|(url, _)| url)
            .collect(),
    )
}

/// Whether `thread` on `page` admits the item at `x_url` on its own: the
/// operations applied to `[X]` keep it. False when X is not an item of the
/// thread's source. Only meaningful for operations without `take`, whose
/// result is a filter of the candidates.
pub fn thread_admits(
    store: &NodeStore,
    ctx: &ThreadContext,
    thread: &PreambleThread,
    page: &str,
    x_url: &str,
) -> bool {
    let Some(items) = ctx.items_by_stem.get(&thread.source) else {
        return false;
    };
    let Ok(index) = items.binary_search_by(|(url, _)| url.as_str().cmp(x_url)) else {
        return false;
    };
    let (url, sem_root) = &items[index];
    let item = SemItem { store, sem_root: *sem_root, url: url.clone() };
    !expressions::apply_link_ops(vec![item], &thread.ops, &normalise_url(page), &ctx.links_to)
        .is_empty()
}

/// Stored link-expression results keyed by `(page URL, slot name)`.
pub type QueryResults = HashMap<(String, String), Vec<String>>;

/// The stored results of every query over the given stem nodes, keyed by
/// `(page, slot)`; several thread expressions in one slot are concatenated in
/// order.
pub fn snapshot_query_results<'a>(
    store: &NodeStore,
    stem_nodes: impl IntoIterator<Item = &'a NodeId>,
) -> QueryResults {
    // (page, slot) → (ordinal, result URLs) per thread expression.
    type Parts = Vec<(usize, Vec<String>)>;
    let mut by_key: HashMap<(String, String), Parts> = HashMap::new();
    for &stem_node in stem_nodes {
        for q in queries_over(store, stem_node) {
            let (Some(page), Some(slot)) =
                (get_attribute_text(store, q, "page"), get_attribute_text(store, q, "slot"))
            else {
                continue;
            };
            let ordinal = get_attribute_text(store, q, "ordinal")
                .and_then(|o| o.parse().ok())
                .unwrap_or(0);
            by_key.entry((page, slot)).or_default().push((ordinal, query_result(store, q)));
        }
    }
    by_key
        .into_iter()
        .map(|(key, mut parts)| {
            parts.sort_by_key(|(ordinal, _)| *ordinal);
            (key, parts.into_iter().flat_map(|(_, urls)| urls).collect())
        })
        .collect()
}

/// Resolve the link expressions in the preamble slots of page `page_url`:
/// evaluate every thread expression against `ctx`, rewrite the semantic
/// slots, and record one query node per thread expression.
///
/// 1. Query nodes recorded for `page_url` by another semantic root (a
///    superseded version of the page) are removed.
/// 2. For each grammar preamble slot whose document slot holds a thread
///    expression, the full target list is recomputed in element order: a
///    `link` block or `PathRef` gives its target page, a thread expression
///    gives its evaluated result.
/// 3. The semantic slot is rewritten: a single-value slot
///    (`max_count() == 1`) becomes a Reference to the first target (none when
///    empty); any other becomes a Collection of the targets.
/// 4. Each thread expression's query node
///    (`P_sem --Derived{query}--> Q --Derived{over}--> stem node`) is created
///    or refreshed with the result URLs as ordered Text children.
///
/// Always recomputed from the candidates, so the function is idempotent.
pub fn resolve_page_link_expressions(
    store: &mut NodeStore,
    sem_root: NodeId,
    doc_root: NodeId,
    page_url: &str,
    grammar: &schema::Grammar,
    ctx: &ThreadContext,
) {
    use crate::content_bridge::{
        element_link_targets, find_child_by_name, find_slot_by_name, slot_element_target,
        SlotElementTarget,
    };

    let query_edge = store.intern(QUERY_EDGE);
    let over_edge = store.intern(OVER_EDGE);

    // 1. Stale query nodes of this page.
    if let Some(qs) = ctx.queries_by_page.get(page_url) {
        for &q in qs {
            if store.get(q).is_some() && !store.derived_sources(q, query_edge).contains(&sem_root) {
                remove_query_node(store, q);
            }
        }
    }

    // Existing query nodes of this semantic root, by (slot, ordinal).
    let mut existing: HashMap<(String, usize), NodeId> = HashMap::new();
    for (name, q) in store.derived(sem_root) {
        if name != query_edge {
            continue;
        }
        if let (Some(slot), Some(ordinal)) = (
            get_attribute_text(store, q, "slot"),
            get_attribute_text(store, q, "ordinal").and_then(|o| o.parse().ok()),
        ) {
            existing.insert((slot, ordinal), q);
        }
    }

    let Some(preamble) = find_child_by_name(store, doc_root, "preamble") else {
        return;
    };

    for slot in &grammar.preamble {
        let slot_name = slot.name.as_str();
        let Some(doc_slot) = find_slot_by_name(store, preamble, slot_name) else {
            continue;
        };
        let elements: Vec<(NodeId, SlotElementTarget)> = store
            .children(doc_slot)
            .into_iter()
            .filter_map(|el| slot_element_target(store, el).map(|t| (el, t)))
            .collect();
        if !elements.iter().any(|(_, t)| matches!(t, SlotElementTarget::Thread { .. })) {
            continue;
        }

        // 2. The full target list, in element order.
        let mut targets: Vec<NodeId> = Vec::new();
        let mut results: Vec<(String, Vec<String>)> = Vec::new();
        for (_, target) in &elements {
            match target {
                SlotElementTarget::Thread { source, ops } => {
                    let evaluated = evaluate_thread(store, ctx, source, ops, page_url);
                    targets.extend(evaluated.iter().map(|(_, sem)| *sem));
                    results.push((source.clone(), evaluated.into_iter().map(|(u, _)| u).collect()));
                }
                url_target => targets.extend(element_link_targets(
                    store,
                    url_target,
                    page_url,
                    &HashMap::new(),
                    &ctx.url_to_root,
                    Some(&ctx.url_to_semantic),
                    Some(ctx),
                )),
            }
        }

        // 3. Rewrite the semantic slot.
        let name = store.intern(slot_name);
        if slot.max_count() == 1 {
            store.remove_reference(sem_root, name);
            if let Some(&first) = targets.first() {
                store.add_edge(sem_root, Edge::Reference { name, target: first });
            }
        } else {
            let collection = store
                .consists_of(sem_root)
                .into_iter()
                .find(|(n, part)| *n == name && matches!(store.get(*part), Some(Node::Collection)))
                .map(|(_, part)| part);
            match collection {
                Some(collection) => store.set_children(collection, &targets),
                None if !targets.is_empty() => {
                    let collection = store.add_node(Node::Collection);
                    store.set_children(collection, &targets);
                    store.add_edge(sem_root, Edge::ConsistsOf { name, part: collection });
                }
                None => {}
            }
        }

        // 4. Create or refresh the query nodes.
        for (ordinal, (source, urls)) in results.into_iter().enumerate() {
            let stem_node = ctx.stem_nodes.get(&source).copied();
            let reusable = match existing.remove(&(slot_name.to_string(), ordinal)) {
                Some(q) if get_attribute_text(store, q, "source").as_deref() == Some(source.as_str()) => Some(q),
                Some(q) => {
                    remove_query_node(store, q);
                    None
                }
                None => None,
            };
            let q = match reusable {
                Some(q) => q,
                None => {
                    let kind = store.intern(QUERY_NODE_KIND);
                    let q = store.add_node(Node::Element(kind));
                    crate::content_bridge::add_text_attr(store, q, "page", page_url);
                    crate::content_bridge::add_text_attr(store, q, "slot", slot_name);
                    crate::content_bridge::add_text_attr(store, q, "ordinal", &ordinal.to_string());
                    crate::content_bridge::add_text_attr(store, q, "source", &source);
                    store.add_edge(sem_root, Edge::Derived { name: query_edge, target: q });
                    if let Some(stem_node) = stem_node {
                        store.add_edge(q, Edge::Derived { name: over_edge, target: stem_node });
                    }
                    q
                }
            };
            let old_children = store.children(q);
            let new_children: Vec<NodeId> =
                urls.into_iter().map(|u| store.add_node(Node::Text(u))).collect();
            store.set_children(q, &new_children);
            for old in old_children {
                store.remove_node(old);
            }
        }
    }

    // Query nodes of this root whose thread expression is gone.
    for (_, q) in existing {
        remove_query_node(store, q);
    }
}

/// Remove a query node and its result Text nodes.
fn remove_query_node(store: &mut NodeStore, q: NodeId) {
    for child in store.children(q) {
        store.remove_node(child);
    }
    for (_, value) in store.attributes(q) {
        store.remove_node(value);
    }
    store.remove_node(q);
}

/// Fingerprint of a document's preamble: a hash of its canonical
/// serialization with the body emptied. Covers slots, `unassigned`, hrefs
/// and slot markers, so an edit that leaves it unchanged is a body-only edit.
pub fn preamble_fingerprint(store: &NodeStore, doc_root: NodeId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match crate::content_bridge::store_to_document(store, doc_root) {
        Ok(mut doc) => {
            doc.body = im::Vector::new();
            content::serialize_document(&doc).hash(&mut hasher);
        }
        Err(e) => format!("\0undecodable: {e}").hash(&mut hasher),
    }
    hasher.finish()
}

/// Store `fingerprint` on `doc_root` (`doc_root --Derived{preamble-fingerprint}--> Text(hex)`),
/// replacing any stored value.
pub fn store_fingerprint(store: &mut NodeStore, doc_root: NodeId, fingerprint: u64) {
    let name = store.intern(FINGERPRINT_EDGE);
    for (n, old) in store.derived(doc_root) {
        if n == name {
            store.remove_node(old);
        }
    }
    let value = store.add_node(Node::Text(format!("{fingerprint:016x}")));
    store.add_edge(doc_root, Edge::Derived { name, target: value });
}

/// The fingerprint stored on `doc_root`, if any.
pub fn fingerprint_of(store: &NodeStore, doc_root: NodeId) -> Option<u64> {
    let name = store.lookup_name(FINGERPRINT_EDGE)?;
    store.derived(doc_root).into_iter().find(|(n, _)| *n == name).and_then(|(_, v)| {
        match store.get(v) {
            Some(Node::Text(hex)) => u64::from_str_radix(hex, 16).ok(),
            _ => None,
        }
    })
}

fn element_name(store: &NodeStore, id: NodeId) -> Option<String> {
    match store.get(id) {
        Some(Node::Element(name)) => Some(store.resolve_name(*name).to_string()),
        _ => None,
    }
}

/// Resolve cross-references in NodeStore.
///
/// When a referenced element has an `href` attribute matching a page URL,
/// merge the target page's Reference edges into the element (preserving existing fields).
pub fn resolve_cross_references_in_store(
    store: &mut NodeStore,
    sem_root: NodeId,
    url_index: &NodeUrlIndex,
) {
    let refs = store.references(sem_root);
    let target_ids: Vec<NodeId> = refs.iter().map(|(_, target)| *target).collect();
    for target_id in target_ids {
        resolve_element_cross_refs(store, target_id, url_index);
    }
}

fn resolve_element_cross_refs(
    store: &mut NodeStore,
    element_id: NodeId,
    url_index: &NodeUrlIndex,
) {
    let href = get_attribute_text(store, element_id, "href");
    if let Some(href_val) = href
        && let Some(&target_root) = url_index.get(&href_val)
    {
            // Collect existing reference names on element_id to avoid duplicates.
            let existing_names: Vec<String> = store
                .references(element_id)
                .iter()
                .map(|(name, _)| store.resolve_name(*name).to_string())
                .collect();

            // Collect target page's fields upfront (avoids borrow issues).
            // Fields live on both ConsistsOf (structural) and Reference (cross-doc) edges.
            let mut target_fields: Vec<(String, NodeId)> = store
                .consists_of(target_root)
                .iter()
                .map(|(name, node)| (store.resolve_name(*name).to_string(), *node))
                .collect();
            target_fields.extend(
                store
                    .references(target_root)
                    .iter()
                    .map(|(name, node)| (store.resolve_name(*name).to_string(), *node)),
            );

            for (name_str, target_value) in target_fields {
                if name_str != "href"
                    && name_str != "text"
                    && !existing_names.contains(&name_str)
                {
                    let name = store.intern(&name_str);
                    store.add_edge(element_id, Edge::Reference { name, target: target_value });
                }
            }
    }

    // Also recurse into Collection children (for List values).
    if matches!(store.get(element_id), Some(Node::Collection)) {
        let children = store.children(element_id);
        for child_id in children {
            resolve_element_cross_refs(store, child_id, url_index);
        }
    }
}

/// Inject stem-based collection nodes into each semantic root.
///
/// For each stem in the stem_index, creates a Collection node and wires
/// Child edges to each item's semantic root. Each page gets a Reference
/// edge to the collection under the stem name, unless that reference already exists.
///
/// Idempotent: if all stems already have Reference edges on the first root,
/// the function returns early without creating any new nodes or edges.
///
/// Performance: O(pages × stems) with O(1) per-lookup via interned Names.
/// Previously O(pages × stems × edges_per_page) due to string allocation and
/// linear Vec search on every iteration.
pub fn inject_collections_in_store(
    store: &mut NodeStore,
    all_roots: &[(String, NodeId)],
    stem_index: &NodeStemIndex,
) {
    // Pre-intern all stem names.
    let interned_stems: HashMap<&str, node_store::Name> = stem_index
        .keys()
        .map(|s| (s.as_str(), store.intern(s)))
        .collect();

    // Determine which stems still need injection by checking the first root.
    // If a Reference edge for a stem already exists on the first root, it was
    // wired in a previous call and we skip that stem entirely — no new Collection
    // node is created for it.
    let needs_injection: Vec<&str> = if let Some((_, first_root)) = all_roots.first() {
        let existing: std::collections::HashSet<node_store::Name> = store
            .references(*first_root)
            .iter()
            .map(|(name, _)| *name)
            .collect();
        stem_index
            .keys()
            .filter(|stem| {
                let name = interned_stems[stem.as_str()];
                !existing.contains(&name)
            })
            .map(|s| s.as_str())
            .collect()
    } else {
        return; // no pages to inject into
    };

    if needs_injection.is_empty() {
        return; // all stems already injected — idempotent early exit
    }

    // Build collection nodes only for stems that need injection.
    let mut stem_collections: HashMap<&str, NodeId> = HashMap::new();
    for stem in &needs_injection {
        if let Some(items) = stem_index.get(*stem) {
            let collection_id = store.add_node(Node::Collection);
            for (_url, item_root) in items {
                store.add_edge(collection_id, Edge::Child(*item_root));
            }
            stem_collections.insert(stem, collection_id);
        }
    }

    // Wire each page's semantic root to the new collections.
    // Use a HashSet<Name> for O(1) duplicate check instead of Vec<String> with linear search.
    for (_url, sem_root) in all_roots {
        let existing: std::collections::HashSet<node_store::Name> = store
            .references(*sem_root)
            .iter()
            .map(|(name, _)| *name)
            .collect();

        for (stem, &collection_id) in &stem_collections {
            let name = interned_stems[stem];
            if !existing.contains(&name) {
                store.add_edge(*sem_root, Edge::Reference { name, target: collection_id });
            }
        }
    }
}

/// Read-only helper: get a text attribute value from a node by attribute name.
///
/// Iterates Attribute edges and compares resolved names — no interning required.
fn get_attribute_text(store: &NodeStore, node_id: NodeId, attr_name: &str) -> Option<String> {
    for (name, value_id) in store.attributes(node_id) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Text(t)) = store.get(value_id)
        {
            return Some(t.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::{Edge, Node, NodeStore};

    /// Build a NodeStore with two item pages (stem = "post").
    fn build_test_store() -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();

        // Intern shared names up front.
        let page_name = store.intern("page");
        let title_name = store.intern("title");
        let stem_name = store.intern("stem");
        let url_name = store.intern("url");
        let kind_name = store.intern("page-kind");

        // --- Page 1 ---
        let sem1 = store.add_node(Node::Element(page_name));
        let title1 = store.add_node(Node::Text("Post One".into()));
        let stem_val1 = store.add_node(Node::Text("post".into()));
        let url_val1 = store.add_node(Node::Text("/post/one".into()));
        let kind_val1 = store.add_node(Node::Text("item".into()));
        store.add_edge(sem1, Edge::Reference { name: title_name, target: title1 });
        store.add_edge(sem1, Edge::Attribute { name: stem_name, value: stem_val1 });
        store.add_edge(sem1, Edge::Attribute { name: url_name, value: url_val1 });
        store.add_edge(sem1, Edge::Attribute { name: kind_name, value: kind_val1 });

        // --- Page 2 ---
        let sem2 = store.add_node(Node::Element(page_name));
        let title2 = store.add_node(Node::Text("Post Two".into()));
        let stem_val2 = store.add_node(Node::Text("post".into()));
        let url_val2 = store.add_node(Node::Text("/post/two".into()));
        let kind_val2 = store.add_node(Node::Text("item".into()));
        store.add_edge(sem2, Edge::Reference { name: title_name, target: title2 });
        store.add_edge(sem2, Edge::Attribute { name: stem_name, value: stem_val2 });
        store.add_edge(sem2, Edge::Attribute { name: url_name, value: url_val2 });
        store.add_edge(sem2, Edge::Attribute { name: kind_name, value: kind_val2 });

        (store, sem1, sem2)
    }

    #[test]
    fn build_indexes_filters_item_pages() {
        let (store, sem1, sem2) = build_test_store();
        let roots = vec![
            ("/post/one".to_string(), sem1),
            ("/post/two".to_string(), sem2),
        ];
        let (url_idx, stem_idx) = build_indexes_from_store(&store, &roots);
        assert_eq!(url_idx.len(), 2);
        assert_eq!(stem_idx["post"].len(), 2);
    }

    #[test]
    fn build_indexes_excludes_collection_pages() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let kind_name = store.intern("page-kind");
        let stem_name = store.intern("stem");

        let sem = store.add_node(Node::Element(page_name));
        let kind_val = store.add_node(Node::Text("collection".into()));
        let stem_val = store.add_node(Node::Text("post".into()));
        store.add_edge(sem, Edge::Attribute { name: kind_name, value: kind_val });
        store.add_edge(sem, Edge::Attribute { name: stem_name, value: stem_val });

        let roots = vec![("/post".to_string(), sem)];
        let (url_idx, stem_idx) = build_indexes_from_store(&store, &roots);
        assert!(url_idx.is_empty(), "collection pages should not appear in url_index");
        assert!(stem_idx.is_empty(), "collection pages should not appear in stem_index");
    }

    #[test]
    fn collection_injection_adds_reference() {
        let (mut store, sem1, sem2) = build_test_store();
        let roots = vec![
            ("/post/one".to_string(), sem1),
            ("/post/two".to_string(), sem2),
        ];
        let (_, stem_idx) = build_indexes_from_store(&store, &roots);
        inject_collections_in_store(&mut store, &roots, &stem_idx);

        // Each root should now have a "post" reference to a Collection.
        let refs1 = store.references(sem1);
        let has_post = refs1
            .iter()
            .any(|(name, _)| store.resolve_name(*name) == "post");
        assert!(has_post, "sem1 should have a 'post' collection reference");

        let refs2 = store.references(sem2);
        let has_post2 = refs2
            .iter()
            .any(|(name, _)| store.resolve_name(*name) == "post");
        assert!(has_post2, "sem2 should have a 'post' collection reference");
    }

    #[test]
    fn collection_injection_collection_node_has_children() {
        let (mut store, sem1, sem2) = build_test_store();
        let roots = vec![
            ("/post/one".to_string(), sem1),
            ("/post/two".to_string(), sem2),
        ];
        let (_, stem_idx) = build_indexes_from_store(&store, &roots);
        inject_collections_in_store(&mut store, &roots, &stem_idx);

        // Find the collection node via sem1's "post" reference.
        let refs1 = store.references(sem1);
        let collection_id = refs1
            .iter()
            .find(|(name, _)| store.resolve_name(*name) == "post")
            .map(|(_, id)| *id)
            .expect("should have a 'post' reference");

        assert!(
            matches!(store.get(collection_id), Some(Node::Collection)),
            "referenced node should be a Collection"
        );
        let children = store.children(collection_id);
        assert_eq!(children.len(), 2, "collection should have 2 children");
    }

    #[test]
    fn collection_skips_existing_references() {
        let (mut store, sem1, sem2) = build_test_store();

        // Pre-add a "post" reference to sem1 pointing at a dummy node.
        let dummy = store.add_node(Node::Text("existing".into()));
        let post_name = store.intern("post");
        store.add_edge(sem1, Edge::Reference { name: post_name, target: dummy });

        let roots = vec![
            ("/post/one".to_string(), sem1),
            ("/post/two".to_string(), sem2),
        ];
        let (_, stem_idx) = build_indexes_from_store(&store, &roots);
        inject_collections_in_store(&mut store, &roots, &stem_idx);

        // sem1 should still point to dummy, not the new collection.
        let refs1 = store.references(sem1);
        let post_refs: Vec<_> = refs1
            .iter()
            .filter(|(name, _)| store.resolve_name(*name) == "post")
            .collect();
        assert_eq!(post_refs.len(), 1, "exactly one 'post' reference should exist");
        assert_eq!(post_refs[0].1, dummy, "existing reference must not be replaced");
    }

    #[test]
    fn inject_collections_is_idempotent() {
        let (mut store, sem1, sem2) = build_test_store();
        let roots = vec![
            ("/post/one".to_string(), sem1),
            ("/post/two".to_string(), sem2),
        ];
        let (_, stem_idx) = build_indexes_from_store(&store, &roots);

        inject_collections_in_store(&mut store, &roots, &stem_idx);
        let node_count_after_first = store.node_count();
        let edge_count_after_first = store.edge_count();

        // Second call must not add any new nodes or edges.
        inject_collections_in_store(&mut store, &roots, &stem_idx);
        assert_eq!(
            store.node_count(),
            node_count_after_first,
            "second inject_collections_in_store call must not add new Collection nodes"
        );
        assert_eq!(
            store.edge_count(),
            edge_count_after_first,
            "second inject_collections_in_store call must not add new edges"
        );

        // Both roots should still have exactly one 'post' reference.
        for &sem_root in &[sem1, sem2] {
            let post_ref_count = store
                .references(sem_root)
                .iter()
                .filter(|(name, _)| store.resolve_name(*name) == "post")
                .count();
            assert_eq!(post_ref_count, 1, "each root should have exactly one 'post' reference after idempotent calls");
        }
    }

    #[test]
    fn cross_reference_resolution_merges_page_fields() {
        let mut store = NodeStore::new();

        // Build a target page (the linked-to page).
        let page_name = store.intern("page");
        let a_name = store.intern("a");
        let kind_name = store.intern("page-kind");
        let stem_name = store.intern("stem");

        let target_root = store.add_node(Node::Element(page_name));
        let kind_val = store.add_node(Node::Text("item".into()));
        let stem_val = store.add_node(Node::Text("post".into()));
        store.add_edge(target_root, Edge::Attribute { name: kind_name, value: kind_val });
        store.add_edge(target_root, Edge::Attribute { name: stem_name, value: stem_val });

        let headline_name = store.intern("headline");
        let headline_val = store.add_node(Node::Text("The Headline".into()));
        store.add_edge(target_root, Edge::ConsistsOf { name: headline_name, part: headline_val });

        // Build a source page that has a link element pointing to the target.
        let href_name = store.intern("href");
        let link_elem = store.add_node(Node::Element(a_name));
        let href_val = store.add_node(Node::Text("/target".into()));
        store.add_edge(link_elem, Edge::Attribute { name: href_name, value: href_val });

        let link_ref_name = store.intern("link");
        let sem_root = store.add_node(Node::Element(page_name));
        store.add_edge(sem_root, Edge::Reference { name: link_ref_name, target: link_elem });

        // Build url_index mapping "/target" -> target_root.
        let mut url_index = NodeUrlIndex::new();
        url_index.insert("/target".to_string(), target_root);

        resolve_cross_references_in_store(&mut store, sem_root, &url_index);

        // The link element should now have a "headline" reference.
        let link_refs = store.references(link_elem);
        let has_headline = link_refs
            .iter()
            .any(|(name, _)| store.resolve_name(*name) == "headline");
        assert!(has_headline, "link element should have inherited 'headline' from target page");
    }

    // ── Link-expression evaluation ──────────────────────────────────────────

    use crate::content_bridge::{create_semantic_content, document_to_store, DocumentMeta};
    use expressions::ThreadItem;

    const POST: &str = "# Post title {#title}\noccurs\n: exactly once\n\n\
        Published date. {#published}\noccurs\n: exactly once\n\n\
        [<name>](/author/<name>) {#author}\noccurs\n: exactly once\ntype\n: link(author)\n\n\
        ----\n\nBody.\n";
    const PAGE: &str = "# Page title {#title}\noccurs\n: exactly once\n\n\
        [<name>](/post/<name>) {#latest}\noccurs\n: *\ntype\n: link(post)\n\n\
        [<name>](/author/<name>) {#owner}\noccurs\n: exactly once\ntype\n: link(author)\n\n\
        ----\n\nBody.\n";

    struct Site {
        store: NodeStore,
        url_to_root: HashMap<String, NodeId>,
        url_to_sem: HashMap<String, NodeId>,
        stem_nodes: HashMap<String, NodeId>,
        page: schema::Grammar,
    }

    impl Site {
        fn ctx(&self) -> ThreadContext {
            ThreadContext::build(
                &self.store,
                self.url_to_sem.clone(),
                self.url_to_root.clone(),
                self.stem_nodes.clone(),
            )
        }
    }

    fn add_doc(site: &mut Site, url: &str, stem: &str, src: &str, grammar: &schema::Grammar) {
        let doc = content::parse_and_assign(src, grammar).unwrap();
        let meta = DocumentMeta {
            url: url.to_string(),
            stem: stem.to_string(),
            file: format!("content{url}.md"),
            page_kind: "item".to_string(),
        };
        let root = document_to_store(&doc, &mut site.store, Some(&meta));
        let sem = create_semantic_content(
            &mut site.store,
            root,
            grammar,
            &meta,
            &HashMap::new(),
            &site.url_to_root,
            Some(&site.url_to_sem),
        );
        site.url_to_root.insert(url.to_string(), root);
        site.url_to_sem.insert(url.to_string(), sem);
    }

    /// Two posts, an author and a page that lists the posts.
    fn site() -> Site {
        let post = schema::parse_schema(POST).unwrap();
        let page = schema::parse_schema(PAGE).unwrap();
        let mut site = Site {
            store: NodeStore::new(),
            url_to_root: HashMap::new(),
            url_to_sem: HashMap::new(),
            stem_nodes: HashMap::new(),
            page: page.clone(),
        };
        add_doc(&mut site, "/post/one", "post", "# One\n\n2\n\n[a](/author/a)\n\n----\n\nBody.\n", &post);
        add_doc(
            &mut site,
            "/post/two",
            "post",
            "# Two\n\n1\n\n[a](/author/a)\n\nSee [b](author/b) here.\n\n![img](/author/c)\n\n----\n\n[x](/author/x)\n",
            &post,
        );
        add_doc(
            &mut site,
            "/page/home",
            "page",
            "# Home\n\n[Latest](->> :post (sort-by :published)) {#latest}\n\n[](:author/a) {#owner}\n\n----\n\nWelcome.\n",
            &page,
        );
        for stem in ["post", "page"] {
            let node = create_stem_node(&mut site.store, stem);
            site.stem_nodes.insert(stem.to_string(), node);
        }
        site
    }

    #[test]
    fn links_to_index_counts_declared_references_only() {
        let site = site();
        let docs: Vec<(String, NodeId)> =
            site.url_to_root.iter().map(|(u, &r)| (u.clone(), r)).collect();
        let index = links_to_index(&site.store, &docs);
        let sources = |target: &str| {
            let mut s: Vec<String> =
                index.get(target).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            s.sort();
            s
        };
        // Preamble link slots and a normalised PathRef.
        assert_eq!(sources("/author/a"), vec!["/page/home", "/post/one", "/post/two"]);
        // An inline link in an unassigned paragraph, normalised.
        assert_eq!(sources("/author/b"), vec!["/post/two"]);
        // Images and body links never count.
        assert!(sources("/author/c").is_empty());
        assert!(sources("/author/x").is_empty());
        // Thread expressions are not references.
        assert!(sources("/post/one").is_empty());
    }

    #[test]
    fn sem_item_reads_heading_and_paragraph_text() {
        let site = site();
        let item = SemItem {
            store: &site.store,
            sem_root: site.url_to_sem["/post/one"],
            url: "/post/one".to_string(),
        };
        assert_eq!(item.field_text("title").as_deref(), Some("One"));
        assert_eq!(item.field_text("published").as_deref(), Some("2"));
        // A link slot resolves to another page's semantic root: no text.
        assert_eq!(item.field_text("author"), None);
        assert_eq!(item.field_text("missing"), None);
    }

    fn slot_targets(site: &Site, page: &str, slot: &str) -> Vec<NodeId> {
        let sem = site.url_to_sem[page];
        let store = &site.store;
        let mut out = Vec::new();
        for (name, target) in store.consists_of(sem).into_iter().chain(store.references(sem)) {
            if store.resolve_name(name) != slot {
                continue;
            }
            if matches!(store.get(target), Some(Node::Collection)) {
                out.extend(store.children(target));
            } else {
                out.push(target);
            }
        }
        out
    }

    #[test]
    fn resolve_page_applies_ops_records_a_query_and_is_idempotent() {
        let mut site = site();
        let ctx = site.ctx();
        let (sem, root) = (site.url_to_sem["/page/home"], site.url_to_root["/page/home"]);
        let page = site.page.clone();
        resolve_page_link_expressions(&mut site.store, sem, root, "/page/home", &page, &ctx);
        let expected = vec![site.url_to_sem["/post/two"], site.url_to_sem["/post/one"]];
        assert_eq!(slot_targets(&site, "/page/home", "latest"), expected);

        let queries = queries_over(&site.store, site.stem_nodes["post"]);
        assert_eq!(queries.len(), 1);
        assert_eq!(query_result(&site.store, queries[0]), vec!["/post/two", "/post/one"]);
        assert_eq!(reevaluate_query(&site.store, &ctx, queries[0]), Some(vec![
            "/post/two".to_string(),
            "/post/one".to_string(),
        ]));
        assert_eq!(
            snapshot_query_results(&site.store, site.stem_nodes.values())
                .get(&("/page/home".to_string(), "latest".to_string())),
            Some(&vec!["/post/two".to_string(), "/post/one".to_string()])
        );

        let counts = (site.store.node_count(), site.store.edge_count());
        resolve_page_link_expressions(&mut site.store, sem, root, "/page/home", &page, &ctx);
        assert_eq!((site.store.node_count(), site.store.edge_count()), counts);
        assert_eq!(slot_targets(&site, "/page/home", "latest"), expected);
        assert_eq!(queries_over(&site.store, site.stem_nodes["post"]), queries);
    }

    #[test]
    fn resolve_page_removes_queries_of_a_superseded_semantic_root() {
        let mut site = site();
        let page = site.page.clone();
        let (sem, root) = (site.url_to_sem["/page/home"], site.url_to_root["/page/home"]);
        let ctx = site.ctx();
        resolve_page_link_expressions(&mut site.store, sem, root, "/page/home", &page, &ctx);
        let old = queries_over(&site.store, site.stem_nodes["post"]);

        // Re-ingest the page: a new document and semantic root for the same URL.
        let src = "# Home\n\n[Latest](->> :post (take 1)) {#latest}\n\n----\n\nWelcome.\n";
        add_doc(&mut site, "/page/home", "page", src, &page);
        let ctx = site.ctx();
        let (sem, root) = (site.url_to_sem["/page/home"], site.url_to_root["/page/home"]);
        resolve_page_link_expressions(&mut site.store, sem, root, "/page/home", &page, &ctx);

        let new = queries_over(&site.store, site.stem_nodes["post"]);
        assert_eq!(new.len(), 1);
        assert_ne!(new, old);
        assert!(site.store.get(old[0]).is_none());
        assert_eq!(query_result(&site.store, new[0]), vec!["/post/one"]);
    }

    #[test]
    fn single_value_slot_with_a_thread_becomes_a_reference_to_the_first_result() {
        let mut site = site();
        let page = site.page.clone();
        let src = "# Home\n\n[Owner](->> :post (sort-by :published :desc)) {#owner}\n\n----\n\nWelcome.\n";
        add_doc(&mut site, "/page/other", "page", src, &page);
        let ctx = site.ctx();
        let (sem, root) = (site.url_to_sem["/page/other"], site.url_to_root["/page/other"]);
        resolve_page_link_expressions(&mut site.store, sem, root, "/page/other", &page, &ctx);
        assert_eq!(slot_targets(&site, "/page/other", "owner"), vec![site.url_to_sem["/post/one"]]);
        let owner = site.store.intern("owner");
        assert_eq!(site.store.references(sem).iter().filter(|(n, _)| *n == owner).count(), 1);
    }

    #[test]
    fn preamble_threads_skip_unassigned_expressions() {
        let mut site = site();
        let page = site.page.clone();
        let src = "# Home\n\n[A](->> :post) {#latest}\n\n[B](->> :post (take 1)) {#nope}\n\n----\n\nWelcome.\n";
        add_doc(&mut site, "/page/other", "page", src, &page);
        let threads = preamble_threads(&site.store, site.url_to_root["/page/other"]);
        assert_eq!(
            threads,
            vec![PreambleThread {
                slot: "latest".to_string(),
                ordinal: 0,
                source: "post".to_string(),
                ops: vec![],
            }]
        );
    }

    #[test]
    fn preamble_fingerprint_ignores_the_body_and_round_trips() {
        let mut site = site();
        let post = schema::parse_schema(POST).unwrap();
        let fp = |site: &Site, url: &str| preamble_fingerprint(&site.store, site.url_to_root[url]);
        let original = fp(&site, "/post/one");
        add_doc(&mut site, "/post/one", "post", "# One\n\n2\n\n[a](/author/a)\n\n----\n\nNew body.\n", &post);
        assert_eq!(fp(&site, "/post/one"), original, "body-only edit");
        add_doc(&mut site, "/post/one", "post", "# One\n\n3\n\n[a](/author/a)\n\n----\n\nNew body.\n", &post);
        assert_ne!(fp(&site, "/post/one"), original, "preamble edit");
        add_doc(&mut site, "/post/one", "post", "# One\n\n2\n\n[a](/author/a)\n\nStray.\n\n----\n\nNew body.\n", &post);
        assert_ne!(fp(&site, "/post/one"), original, "unassigned edit");

        let root = site.url_to_root["/post/one"];
        assert_eq!(fingerprint_of(&site.store, root), None);
        store_fingerprint(&mut site.store, root, 0xdead_beef);
        store_fingerprint(&mut site.store, root, original);
        assert_eq!(fingerprint_of(&site.store, root), Some(original));
        let name = site.store.intern(FINGERPRINT_EDGE);
        assert_eq!(site.store.derived(root).iter().filter(|(n, _)| *n == name).count(), 1);
    }
}
