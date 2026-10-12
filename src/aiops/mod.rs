//! Admin AI layer ("AI Copilot", Phase D).
//!
//! Pluggable by design: the brain can be swapped later without touching the
//! platform. Today two providers ship:
//!
//! * `heuristic` — deterministic rule engine over live system context
//!   (zero external dependencies, always available)
//! * `llm` — any OpenAI-compatible chat-completions endpoint; failures
//!   degrade gracefully and never take the admin surface down
//!
//! Governance lives in `ai_config` (DB, admin-editable):
//!
//!   mode:     off | heuristic | llm
//!   autonomy: observe (log only) · suggest (admin applies) · act (auto-apply)
//!   scopes:   sources | security | library | notifications | grants
//!   security_integration: when false the AI never sees or touches the
//!             security domain — it works with or without the risk layer.
//!
//! Every analysis and every applied/rejected action is written to `ai_events`.

pub mod engine;
pub mod provider;
pub mod routes;
