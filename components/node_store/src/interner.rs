use im::{HashMap, Vector};

use crate::node::{Name, NameId};

/// String interner for Name values. Interning gives O(1) comparison.
#[derive(Debug, Clone, Default)]
pub struct NameInterner {
    to_id: HashMap<String, NameId>,
    to_name: Vector<String>,
}

impl NameInterner {
    /// Returns existing Name if already interned, otherwise allocates new NameId.
    pub fn intern(&mut self, s: &str) -> Name {
        if let Some(&id) = self.to_id.get(s) {
            return Name(id);
        }
        let id = NameId(self.to_name.len() as u32);
        self.to_name.push_back(s.to_string());
        self.to_id.insert(s.to_string(), id);
        Name(id)
    }

    /// Panics if name not in interner (internal invariant).
    pub fn resolve(&self, name: Name) -> &str {
        self.to_name
            .get(name.0.0 as usize)
            .expect("NameId not found in interner — internal invariant violated")
    }

    pub fn len(&self) -> usize {
        self.to_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.to_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_and_resolve_roundtrip() {
        let mut interner = NameInterner::default();
        let name = interner.intern("hello");
        assert_eq!(interner.resolve(name), "hello");
    }

    #[test]
    fn intern_same_string_twice_returns_same_name_id() {
        let mut interner = NameInterner::default();
        let a = interner.intern("foo");
        let b = interner.intern("foo");
        assert_eq!(a, b);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn intern_different_strings_returns_different_name_ids() {
        let mut interner = NameInterner::default();
        let a = interner.intern("foo");
        let b = interner.intern("bar");
        assert_ne!(a, b);
        assert_eq!(interner.len(), 2);
    }
}
