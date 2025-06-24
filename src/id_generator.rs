//! Module for generating unique IDs

/// A unique identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(usize);

/// Generator for unique IDs
#[derive(Debug)]
pub struct IdGenerator {
    next_id: usize,
}

impl IdGenerator {
    /// Create a new ID generator starting from 0
    pub fn new() -> Self {
        Self { next_id: 0 }
    }

    /// Generate a new unique ID
    pub fn gen_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        Id(id)
    }
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}