/// Trait for counting tokens in text. Allows swapping word count for a real
/// tokenizer (e.g. HuggingFace tokenizers) later.
pub trait TokenCounter {
    fn count_tokens(&self, text: &str) -> usize;
}

/// Simple token counter that splits on whitespace and counts words.
#[derive(Debug, Clone, Default)]
pub struct WordCounter;

impl TokenCounter for WordCounter {
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_counter_empty() {
        assert_eq!(WordCounter.count_tokens(""), 0);
    }

    #[test]
    fn word_counter_whitespace_only() {
        assert_eq!(WordCounter.count_tokens("   \n\t  "), 0);
    }

    #[test]
    fn word_counter_simple() {
        assert_eq!(WordCounter.count_tokens("hello world"), 2);
    }

    #[test]
    fn word_counter_punctuation() {
        assert_eq!(WordCounter.count_tokens("Hello, world! How are you?"), 5);
    }
}
