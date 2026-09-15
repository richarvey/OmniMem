//! The seam between the engine and a language model.
//!
//! Fact extraction, query expansion and the second contradiction tier ask
//! Claude Haiku a single question and read one text reply. `omnimem-llm`
//! implements this for the Anthropic API; tests use a scripted fake, and an
//! engine with no model degrades exactly as 6.x did without an API key.

pub type LlmError = Box<dyn std::error::Error + Send + Sync>;

pub trait LanguageModel: Send + Sync {
    /// Send one user message to `model` and return the text of the reply.
    fn complete(&self, model: &str, prompt: &str, max_tokens: u32) -> Result<String, LlmError>;
}
