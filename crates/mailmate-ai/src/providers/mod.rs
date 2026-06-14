//! The concrete AI providers: the deterministic mock and the four HTTP adapters.
//!
//! Every provider-specific detail — endpoint paths, model names, request bodies, response
//! shapes, header conventions, grammar/format flags — lives ONLY in these modules. No
//! caller outside `mailmate-ai::providers` knows any of it; the rest of the system speaks
//! only the `AiProvider` port and the shared `mailmate-common::ai` vocabulary.

pub mod llama_cpp;
pub mod lm_studio;
pub mod mock;
pub mod ollama;
pub mod openai_compatible;

pub use llama_cpp::LlamaCppAdapter;
pub use lm_studio::LmStudioAdapter;
pub use mock::MockProvider;
pub use ollama::OllamaAdapter;
pub use openai_compatible::OpenAiCompatibleAdapter;

use mailmate_common::ai::{MessageRole, PromptMessage, StructuredResponse};
use mailmate_common::error::AiError;

/// A generic GBNF grammar that constrains output to any valid JSON value. The grammar-
/// capable backends (llama.cpp, LM Studio) send this so the output is *guaranteed* parseable
/// JSON. Field-by-field schema GBNF is a refinement; valid-JSON enforcement is the guarantee
/// that actually matters for the parse step.
pub(crate) const GENERIC_JSON_GBNF: &str = r#"root ::= object
value ::= object | array | string | number | "true" | "false" | "null"
object ::= "{" ws ( string ":" ws value ( "," ws string ":" ws value )* )? ws "}"
array ::= "[" ws ( value ( "," ws value )* )? ws "]"
string ::= "\"" ( [^"\\] | "\\" . )* "\""
number ::= "-"? [0-9]+ ( "." [0-9]+ )? ( [eE] [-+]? [0-9]+ )?
ws ::= [ \t\n]*"#;

/// Compile a (minimal) GBNF grammar from an optional JSON schema.
pub(crate) fn schema_to_gbnf(_schema: Option<&serde_json::Value>) -> String {
    GENERIC_JSON_GBNF.to_owned()
}

/// Flatten a message list into a single prompt string (for completion-style endpoints).
pub(crate) fn render_prompt(messages: &[PromptMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let tag = match m.role {
                MessageRole::System => "SYSTEM",
                MessageRole::User => "USER",
                MessageRole::Assistant => "ASSISTANT",
            };
            format!("{tag}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a provider's inner content string into a [`StructuredResponse`], tagging which
/// enforcement method produced it. A non-JSON content fails closed as a validation error.
pub(crate) fn structured_from_content(
    content: &str,
    method: &str,
) -> Result<StructuredResponse, AiError> {
    StructuredResponse::from_raw_json(content, method)
}
