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
