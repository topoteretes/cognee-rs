use std::collections::HashMap;

use crate::retrievers::SearchRetrieverRef;
use crate::types::{SearchError, SearchType};

#[derive(Default)]
pub struct SearchTypeRegistry {
    retrievers: HashMap<SearchType, SearchRetrieverRef>,
}

impl SearchTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, retriever: SearchRetrieverRef) {
        self.retrievers.insert(retriever.search_type(), retriever);
    }

    pub fn get(&self, search_type: SearchType) -> Result<SearchRetrieverRef, SearchError> {
        self.retrievers
            .get(&search_type)
            .cloned()
            .ok_or(SearchError::UnsupportedSearchType(search_type))
    }

    pub fn contains(&self, search_type: SearchType) -> bool {
        self.retrievers.contains_key(&search_type)
    }
}
