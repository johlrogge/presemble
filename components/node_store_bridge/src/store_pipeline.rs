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

/// Resolve link expressions in NodeStore semantic content.
///
/// In the NodeStore model, link expressions are resolved during `populate_node_store`
/// as Reference edges, so this is largely a verification/no-op.
/// Any remaining unresolved expressions (e.g., ThreadExpr with runtime operations)
/// need special handling — add that here when cases arise.
pub fn resolve_link_expressions_in_store(
    store: &mut NodeStore,
    sem_root: NodeId,
    url_index: &NodeUrlIndex,
    stem_index: &NodeStemIndex,
) {
    // Link expressions are resolved as Reference edges during populate_node_store.
    // This function is a placeholder for runtime resolution cases not yet implemented.
    let _ = (store, sem_root, url_index, stem_index);
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
pub fn inject_collections_in_store(
    store: &mut NodeStore,
    all_roots: &[(String, NodeId)],
    stem_index: &NodeStemIndex,
) {
    // Build one collection node per stem.
    let mut stem_collections: HashMap<String, NodeId> = HashMap::new();

    for (stem, items) in stem_index {
        let collection_id = store.add_node(Node::Collection);
        for (_url, item_root) in items {
            store.add_edge(collection_id, Edge::Child(*item_root));
        }
        stem_collections.insert(stem.clone(), collection_id);
    }

    // Wire each page's semantic root to every collection.
    for (_url, sem_root) in all_roots {
        let existing_names: Vec<String> = store
            .references(*sem_root)
            .iter()
            .map(|(name, _)| store.resolve_name(*name).to_string())
            .collect();

        for (stem, &collection_id) in &stem_collections {
            if !existing_names.contains(stem) {
                let name = store.intern(stem);
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
    fn resolve_link_expressions_is_noop() {
        // Smoke test — function must not panic and must not alter the store.
        let (mut store, sem1, _sem2) = build_test_store();
        let edge_count_before = store.edge_count();
        let url_idx = NodeUrlIndex::new();
        let stem_idx = NodeStemIndex::new();
        resolve_link_expressions_in_store(&mut store, sem1, &url_idx, &stem_idx);
        assert_eq!(store.edge_count(), edge_count_before);
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
}
