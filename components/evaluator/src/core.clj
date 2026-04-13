;; Presemble core library — evaluated at nREPL startup.
;; Defines standard functions in terms of Rust primitives.
;; Inspired by Clojure's core, reimplemented for Presemble.
;;
;; NOTE: Some Clojure functions cannot be defined here due to evaluator
;; limitations in Phase 5:
;;   - `group-by`, `frequencies`, `update`, `update-in`, `assoc-in`:
;;     require dynamic keyword-variable access (get/assoc with non-literal keys).
;;     These require Phase 6 keyword semantics migration.
;;   - `zipmap` with 2-arg map: `(map f coll1 coll2)` not yet supported.
;;   - `comp` 0-arity: needs `identity` defined first (handled by ordering).

;; ── Identity & Constants ──────────────────────────────
(defn identity [x] x)
(defn constantly [x] (fn [& _] x))

;; ── Predicates ────────────────────────────────────────
(defn nil? [x] (= x nil))
(defn some? [x] (not (nil? x)))
(defn true? [x] (= x true))
(defn false? [x] (= x false))
(defn string? [x] (= (type x) :string))
(defn number? [x] (= (type x) :integer))
(defn integer? [x] (= (type x) :integer))
(defn keyword? [x] (= (type x) :keyword))
(defn map? [x] (= (type x) :map))
(defn vector? [x] (= (type x) :list))
(defn list? [x] (= (type x) :list))
(defn fn? [x] (= (type x) :fn))
(defn boolean? [x] (= (type x) :boolean))
(defn coll? [x] (or (vector? x) (map? x)))
(defn not-empty [x] (if (empty? x) nil x))

;; ── Arithmetic ────────────────────────────────────────
(defn inc [x] (+ x 1))
(defn dec [x] (- x 1))
(defn zero? [x] (= x 0))
(defn pos? [x] (> x 0))
(defn neg? [x] (< x 0))
(defn even? [x] (= 0 (mod x 2)))
(defn odd? [x] (not (even? x)))
(defn max [a b] (if (> a b) a b))
(defn min [a b] (if (< a b) a b))
(defn abs [x] (if (neg? x) (- x) x))
(defn not= [a b] (not (= a b)))
(defn <= [a b] (or (< a b) (= a b)))
(defn >= [a b] (or (> a b) (= a b)))

;; ── Higher-order combinators ──────────────────────────
(defn complement [f]
  (fn [& args] (not (apply f args))))

;; comp: multi-arity — 0, 1, and 2-arg variants.
;; NOTE: defn with multi-arity syntax (defn name ([args] body) ...) is supported.
(defn comp
  ([] identity)
  ([f] f)
  ([f g] (fn [& args] (f (apply g args)))))

(defn partial
  ([f x] (fn [& args] (apply f (cons x args))))
  ([f x y] (fn [& args] (apply f (cons x (cons y args))))))

(defn juxt [& fns]
  (fn [& args]
    (map (fn [f] (apply f args)) fns)))

(defn fnil [f default]
  (fn [x & args]
    (apply f (cons (if (nil? x) default x) args))))

;; ── Collection operations ─────────────────────────────
(defn second [coll] (first (rest coll)))

(defn butlast [coll]
  (take (dec (count coll)) coll))

(defn drop [n coll]
  (if (or (<= n 0) (empty? coll))
    coll
    (drop (dec n) (rest coll))))

(defn take-while [pred coll]
  (if (empty? coll)
    []
    (let [x (first coll)]
      (if (pred x)
        (cons x (take-while pred (rest coll)))
        []))))

(defn drop-while [pred coll]
  (if (empty? coll)
    coll
    (if (pred (first coll))
      (drop-while pred (rest coll))
      coll)))

(defn keep [f coll]
  (filter some? (map f coll)))

(defn distinct [coll]
  (reduce
    (fn [acc x]
      (if (some (fn [y] (= x y)) acc)
        acc
        (conj acc x)))
    []
    coll))

(defn flatten [coll]
  (reduce
    (fn [acc x]
      (if (coll? x)
        (concat acc (flatten x))
        (conj acc x)))
    []
    coll))

(defn partition [n coll]
  (if (< (count coll) n)
    []
    (cons (take n coll) (partition n (drop n coll)))))

(defn interleave [coll1 coll2]
  (if (or (empty? coll1) (empty? coll2))
    []
    (cons (first coll1)
      (cons (first coll2)
        (interleave (rest coll1) (rest coll2))))))

(defn interpose [sep coll]
  (if (empty? coll)
    []
    (rest (mapcat (fn [x] [sep x]) coll))))

(defn into [to from]
  (reduce conj to from))

;; ── String operations ─────────────────────────────────
(defn blank? [s]
  (or (nil? s) (= s "")))

(defn str-join [sep coll]
  (if (empty? coll)
    ""
    (reduce
      (fn [acc x] (str acc sep (str x)))
      (str (first coll))
      (rest coll))))
