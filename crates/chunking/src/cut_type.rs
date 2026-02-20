use serde::{Deserialize, Serialize};
use std::fmt;

/// Describes how a chunk boundary was determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CutType {
    /// Sentence ending followed by a newline (paragraph boundary).
    ParagraphEnd,
    /// Sentence ending punctuation without a newline.
    SentenceEnd,
    /// Text ended mid-sentence (no ending punctuation).
    SentenceCut,
    /// Text ended mid-word.
    Word,
}

impl fmt::Display for CutType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CutType::ParagraphEnd => write!(f, "paragraph_end"),
            CutType::SentenceEnd => write!(f, "sentence_end"),
            CutType::SentenceCut => write!(f, "sentence_cut"),
            CutType::Word => write!(f, "word"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_formats() {
        assert_eq!(CutType::ParagraphEnd.to_string(), "paragraph_end");
        assert_eq!(CutType::SentenceEnd.to_string(), "sentence_end");
        assert_eq!(CutType::SentenceCut.to_string(), "sentence_cut");
        assert_eq!(CutType::Word.to_string(), "word");
    }
}
