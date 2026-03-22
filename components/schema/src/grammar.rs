/// A document grammar: an ordered sequence of named, constrained slots
/// followed by optional body rules.
#[derive(Debug, Clone)]
pub struct Grammar {
    pub preamble: Vec<Slot>,
    pub body: Option<BodyRules>,
}

/// A named position in the document grammar with an expected element type,
/// constraints, and optional hint text for authors.
#[derive(Debug, Clone)]
pub struct Slot {
    pub name: SlotName,
    pub element: Element,
    pub constraints: Vec<Constraint>,
    pub hint_text: Option<String>,
}

/// A semantic name for a slot, used for template references (e.g. `${article:title}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotName(String);

impl SlotName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SlotName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The type of structural element expected in a slot.
#[derive(Debug, Clone)]
pub enum Element {
    Heading { level: HeadingLevelRange },
    /// One or more paragraphs. Cardinality is expressed via `Constraint::Occurs`.
    Paragraph,
    Link { pattern: String },
    Image { pattern: String },
}

/// A valid heading level (1..=6). Constructed via `HeadingLevel::new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HeadingLevel(u8);

impl HeadingLevel {
    /// Returns `Some(HeadingLevel)` for values 1..=6, `None` otherwise.
    pub fn new(level: u8) -> Option<Self> {
        if (1..=6).contains(&level) {
            Some(Self(level))
        } else {
            None
        }
    }

    pub fn value(self) -> u8 {
        self.0
    }
}

/// An inclusive range of heading levels.
#[derive(Debug, Clone)]
pub struct HeadingLevelRange {
    pub min: HeadingLevel,
    pub max: HeadingLevel,
}

/// A count range for occurrence or quantity constraints.
#[derive(Debug, Clone)]
pub enum CountRange {
    Exactly(usize),
    AtLeast(usize),
    AtMost(usize),
    Between { min: usize, max: usize },
}

/// A constraint applied to a slot.
#[derive(Debug, Clone)]
pub enum Constraint {
    Occurs(CountRange),
    Content(ContentConstraint),
    Alt(AltRequirement),
    Orientation(Orientation),
}

/// Image orientation constraint.
#[derive(Debug, Clone)]
pub enum Orientation {
    Landscape,
    Portrait,
}

/// Content text constraint.
#[derive(Debug, Clone)]
pub enum ContentConstraint {
    Capitalized,
}

/// Whether alt text is required for an image.
#[derive(Debug, Clone)]
pub enum AltRequirement {
    Required,
    Optional,
}

/// Rules governing the free body section after the `----` separator.
#[derive(Debug, Clone)]
pub struct BodyRules {
    pub heading_range: Option<HeadingLevelRange>,
}
