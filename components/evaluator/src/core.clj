;; Presemble core library — evaluated at nREPL startup.
;; Defines standard functions in terms of Rust primitives.
;; Inspired by Clojure's core, reimplemented for Presemble.

;; ── Identity & Constants ──────────────────────────────
(defn identity "Returns its argument unchanged." [x] x)
(defn constantly "Returns a function that always returns x, ignoring its arguments." [x] (fn [& _] x))

;; ── Predicates ────────────────────────────────────────
(defn nil? "Returns true if x is nil." [x] (= x nil))
(defn some? "Returns true if x is not nil." [x] (not (nil? x)))
(defn true? "Returns true if x is exactly true." [x] (= x true))
(defn false? "Returns true if x is exactly false." [x] (= x false))
(defn string? "Returns true if x is a string." [x] (= (type x) :string))
(defn number? "Returns true if x is a number." [x] (= (type x) :integer))
(defn integer? "Returns true if x is an integer." [x] (= (type x) :integer))
(defn keyword? "Returns true if x is a keyword." [x] (= (type x) :keyword))
(defn map? "Returns true if x is a map/record." [x] (= (type x) :map))
(defn vector? "Returns true if x is a vector/list." [x] (= (type x) :list))
(defn list? "Returns true if x is a list." [x] (= (type x) :list))
(defn fn? "Returns true if x is a function." [x] (= (type x) :fn))
(defn boolean? "Returns true if x is a boolean." [x] (= (type x) :boolean))
(defn coll? "Returns true if x is a collection (vector or map)." [x] (or (vector? x) (map? x)))
(defn not-empty "Returns x if non-empty, nil if empty." [x] (if (empty? x) nil x))

;; ── Arithmetic ────────────────────────────────────────
(defn inc "Returns x + 1." [x] (+ x 1))
(defn dec "Returns x - 1." [x] (- x 1))
(defn zero? "Returns true if x is zero." [x] (= x 0))
(defn pos? "Returns true if x is positive." [x] (> x 0))
(defn neg? "Returns true if x is negative." [x] (< x 0))
(defn even? "Returns true if x is even." [x] (= 0 (mod x 2)))
(defn odd? "Returns true if x is odd." [x] (not (even? x)))
(defn max "Returns the larger of a and b." [a b] (if (> a b) a b))
(defn min "Returns the smaller of a and b." [a b] (if (< a b) a b))
(defn abs "Returns the absolute value of x." [x] (if (neg? x) (- x) x))
(defn not= "Returns true if a and b are not equal." [a b] (not (= a b)))
(defn <= "Returns true if a is less than or equal to b." [a b] (or (< a b) (= a b)))
(defn >= "Returns true if a is greater than or equal to b." [a b] (or (> a b) (= a b)))

;; ── Higher-order combinators ──────────────────────────
(defn complement "Returns a function that is the logical complement of f." [f]
  (fn [& args] (not (apply f args))))

;; comp: multi-arity — 0, 1, and 2-arg variants.
;; NOTE: defn with multi-arity syntax (defn name ([args] body) ...) is supported.
(defn comp "Composes functions right to left. (comp f g) returns (fn [& args] (f (apply g args)))."
  ([] identity)
  ([f] f)
  ([f g] (fn [& args] (f (apply g args)))))

(defn partial "Returns a partially applied function with x (and optionally y) pre-filled."
  ([f x] (fn [& args] (apply f (cons x args))))
  ([f x y] (fn [& args] (apply f (cons x (cons y args))))))

(defn juxt "Returns a function that applies each of fns to its args and returns a vector of results." [& fns]
  (fn [& args]
    (map (fn [f] (apply f args)) fns)))

(defn fnil "Returns a function like f but replaces nil first argument with default." [f default]
  (fn [x & args]
    (apply f (cons (if (nil? x) default x) args))))

;; ── Collection operations ─────────────────────────────
(defn second "Returns the second item of a collection." [coll] (first (rest coll)))

(defn butlast "Returns all items of coll except the last." [coll]
  (take (dec (count coll)) coll))

(defn drop "Returns coll with the first n items removed." [n coll]
  (if (or (<= n 0) (empty? coll))
    coll
    (drop (dec n) (rest coll))))

(defn take-while "Returns items from coll while pred returns truthy, stopping at first falsy result." [pred coll]
  (if (empty? coll)
    []
    (let [x (first coll)]
      (if (pred x)
        (cons x (take-while pred (rest coll)))
        []))))

(defn drop-while "Drops items from coll while pred returns truthy, returning the rest." [pred coll]
  (if (empty? coll)
    coll
    (if (pred (first coll))
      (drop-while pred (rest coll))
      coll)))

(defn keep "Returns a list of non-nil results of applying f to each item in coll." [f coll]
  (filter some? (map f coll)))

(defn mapcat "Applies f to each item in coll, concatenating the results." [f coll]
  (apply concat (map f coll)))

(defn distinct "Returns a collection with duplicate elements removed." [coll]
  (reduce
    (fn [acc x]
      (if (some (fn [y] (= x y)) acc)
        acc
        (conj acc x)))
    []
    coll))

(defn flatten "Recursively flattens nested collections into a single flat list." [coll]
  (reduce
    (fn [acc x]
      (if (coll? x)
        (concat acc (flatten x))
        (conj acc x)))
    []
    coll))

(defn partition "Partitions coll into chunks of n items each." [n coll]
  (if (< (count coll) n)
    []
    (cons (take n coll) (partition n (drop n coll)))))

(defn interleave "Returns a list of the first item of each coll, then the second, etc." [coll1 coll2]
  (if (or (empty? coll1) (empty? coll2))
    []
    (cons (first coll1)
      (cons (first coll2)
        (interleave (rest coll1) (rest coll2))))))

(defn interpose "Returns a list with sep inserted between each item in coll." [sep coll]
  (if (empty? coll)
    []
    (rest (mapcat (fn [x] [sep x]) coll))))

(defn into "Reduces from into to by conjoining items." [to from]
  (reduce conj to from))

;; ── String operations ─────────────────────────────────
(defn blank? "Returns true if s is nil or the empty string." [s]
  (or (nil? s) (= s "")))

(defn str-join "Joins elements of coll into a string separated by sep." [sep coll]
  (if (empty? coll)
    ""
    (reduce
      (fn [acc x] (str acc sep (str x)))
      (str (first coll))
      (rest coll))))

;; ── Map operations (Phase 6: keywords are first-class values) ────
;; These require (get m k) to work when k is a variable holding a
;; keyword value — enabled by the Phase 6 keyword semantics migration.

(defn update "Returns m with the value at key k replaced by (f current-value)." [m k f]
  (assoc m k (f (get m k))))

(defn update-in "Returns m with the value at the nested key path ks replaced by (f current-value)." [m ks f]
  (let [k (first ks)
        rest-ks (rest ks)]
    (if (empty? rest-ks)
      (update m k f)
      (assoc m k (update-in (get m k {}) rest-ks f)))))

(defn assoc-in "Associates value v at the nested key path ks in m." [m ks v]
  (let [k (first ks)
        rest-ks (rest ks)]
    (if (empty? rest-ks)
      (assoc m k v)
      (assoc m k (assoc-in (get m k {}) rest-ks v)))))

(defn group-by "Groups items of coll by the result of f, returning a map of key to vector of items." [f coll]
  (reduce
    (fn [acc x]
      (let [k (f x)
            existing (get acc k [])]
        (assoc acc k (conj existing x))))
    {}
    coll))

(defn frequencies "Returns a map from each distinct item in coll to the number of times it appears." [coll]
  (reduce
    (fn [acc x]
      (let [n (get acc x 0)]
        (assoc acc x (inc n))))
    {}
    coll))

(defn merge-with "Merges maps, applying f to combine values for duplicate keys." [f & maps]
  (reduce
    (fn [acc m]
      (reduce
        (fn [a k]
          (if (contains? a k)
            (assoc a k (f (get a k) (get m k)))
            (assoc a k (get m k))))
        acc
        (keys m)))
    {}
    maps))
