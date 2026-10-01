;; NED prelude — higher-level node editing operations.
;; Built on ned/* Rust primitives.

(def-doc! ned/headings "(ned/headings sel)" "Select all heading elements from a selection.")
(defn ned/headings [sel]
  (ned/filter sel :kind "heading"))

(def-doc! ned/paragraphs "(ned/paragraphs sel)" "Select all paragraph elements.")
(defn ned/paragraphs [sel]
  (ned/filter sel :kind "paragraph"))

(def-doc! ned/documents "(ned/documents sel)" "Select all document elements.")
(defn ned/documents [sel]
  (ned/filter sel :kind "document"))

(def-doc! ned/texts "(ned/texts sel)" "Select all text nodes.")
(defn ned/texts [sel]
  (ned/filter sel :text))

(def-doc! ned/elements "(ned/elements sel)" "Select all element nodes.")
(defn ned/elements [sel]
  (ned/filter sel :element))

(def-doc! ned/h1s "(ned/h1s sel)" "Select level-1 headings.")
(defn ned/h1s [sel]
  (-> sel ned/headings (ned/filter :attr "level" 1)))

(def-doc! ned/h2s "(ned/h2s sel)" "Select level-2 headings.")
(defn ned/h2s [sel]
  (-> sel ned/headings (ned/filter :attr "level" 2)))

(def-doc! ned/h3s "(ned/h3s sel)" "Select level-3 headings.")
(defn ned/h3s [sel]
  (-> sel ned/headings (ned/filter :attr "level" 3)))

;; Convenience: start from all nodes
(def-doc! ned/all-headings "(ned/all-headings)" "All headings in the store.")
(defn ned/all-headings [] (ned/headings (ned/all)))

(def-doc! ned/all-h2s "(ned/all-h2s)" "All level-2 headings in the store.")
(defn ned/all-h2s [] (ned/h2s (ned/all)))

(def-doc! ned/all-documents "(ned/all-documents)" "All documents in the store.")
(defn ned/all-documents [] (ned/documents (ned/all)))

;; ── Document metadata queries ────────────────────────────────────────

(def-doc! ned/by-stem "(ned/by-stem stem)" "Select documents with the given schema stem.")
(defn ned/by-stem [stem]
  (ned/filter (ned/all-documents) :attr "stem" stem))

(def-doc! ned/by-url "(ned/by-url url)" "Select the document at the given URL.")
(defn ned/by-url [url]
  (ned/filter (ned/all-documents) :attr "url" url))

(def-doc! ned/posts "(ned/posts)" "All post documents.")
(defn ned/posts [] (ned/by-stem "post"))

(def-doc! ned/features "(ned/features)" "All feature documents.")
(defn ned/features [] (ned/by-stem "feature"))

(def-doc! ned/url-of "(ned/url-of sel)" "Get URLs of selected documents.")
(defn ned/url-of [sel]
  (ned/attr-of sel "url"))

(def-doc! ned/stem-of "(ned/stem-of sel)" "Get stems of selected documents.")
(defn ned/stem-of [sel]
  (ned/attr-of sel "stem"))

;; ── Path-based addressing ─────────────────────────────────────────────────

(def-doc! ned/doc-by-path "(ned/doc-by-path path)" "Select the document root whose :file attribute matches path. Returns empty if not found.")
(defn ned/doc-by-path [path]
  (ned/filter (ned/all-documents) :attr "file" path))

(def-doc! ned/body "(ned/body doc)" "Given a document selection, return its body element.")
(defn ned/body [doc]
  (-> doc
      (ned/children)
      (ned/filter :kind "body")))

(def-doc! ned/slot "(ned/slot doc slot-name)" "Given a document selection, return the named slot element. \"body\" returns the document's body element (same as ned/body); any other name returns the named slot element under its preamble.")
(defn ned/slot [doc slot-name]
  (if (= slot-name "body")
    (ned/body doc)
    (-> doc
        (ned/children)
        (ned/filter :kind "preamble")
        (ned/children)
        (ned/filter :attr "name" slot-name))))

(def-doc! ned/body-at "(ned/body-at doc idx)" "Given a document selection, return the idx-th body child element.")
(defn ned/body-at [doc idx]
  (-> doc
      (ned/body)
      (ned/nth-child idx)))

;; ── Source document discovery ────────────────────────────────────────────

(def-doc! ned/source-docs-of "(ned/source-docs-of sel)" "Return the set of document-root nodes containing the selected nodes (ancestors filtered to Element(\"document\")).")
(defn ned/source-docs-of [sel]
  (ned/union
    (ned/filter sel :kind "document")
    (ned/filter (ned/ancestors sel) :kind "document")))

;; ── Structural anchor helpers ────────────────────────────────────────────

(def-doc! ned/heading-with-text "(ned/heading-with-text sel text)" "Filter sel to heading elements whose concatenated text descendants exactly equal text.")
(defn ned/heading-with-text [sel text]
  (-> sel (ned/filter :kind "heading") (ned/filter :text-equals text)))

(def-doc! ned/nth-heading "(ned/nth-heading sel idx)" "Pick the idx-th heading from sel.")
(defn ned/nth-heading [sel idx]
  (ned/nth-of-kind sel "heading" idx))

(def-doc! ned/first-after "(ned/first-after sel kind)" "For each node in sel, the first following sibling element of the given kind. Each anchor resolves separately, so two anchors give two results.")
(defn ned/first-after [sel kind]
  (ned/nth-following-of-kind sel kind 0))

(def-doc! ned/nth-after "(ned/nth-after sel kind idx)" "For each node in sel, the idx-th following sibling element of the given kind (document order). Each anchor resolves separately, so two anchors give two results; ambiguity is never narrowed.")
(defn ned/nth-after [sel kind idx]
  (ned/nth-following-of-kind sel kind idx))

;; ── Text addressing (ADR-056: SetText/SearchReplace target one Text node) ──
;; ned/nth-text is a Rust builtin: (ned/nth-text sel idx).

(def-doc! ned/blocks "(ned/blocks sel)" "The block elements of a slot or body: the children of each selected node.")
(defn ned/blocks [sel]
  (ned/children sel))

(def-doc! ned/text-in "(ned/text-in sel)" "The direct Text children of each selected block, i.e. \"the\" text of a block. Several Text children (or several blocks) give several results.")
(defn ned/text-in [sel]
  (-> sel ned/children ned/texts))

(def-doc! ned/texts-under "(ned/texts-under sel)" "All Text nodes at any depth under the selected nodes.")
(defn ned/texts-under [sel]
  (-> sel ned/descendants ned/texts))

(def-doc! ned/first-text "(ned/first-text sel)" "For each selected node, the first Text node under it in document order. Same as (ned/nth-text sel 0).")
(defn ned/first-text [sel]
  (ned/nth-text sel 0))

(def-doc! ned/text-containing "(ned/text-containing sel s)" "Text nodes at any depth under the selected nodes whose own text contains s. Every match is returned; ambiguity is never narrowed.")
(defn ned/text-containing [sel s]
  (-> sel ned/texts-under (ned/filter :text-contains s)))
