//! Pure budget / large-result policy decisions and token helpers.

use hydrogen::Usage;

use super::config::BUDGET_INCREMENT;
use super::events::{BudgetDecision, LargeResultDecision, LargeToolResultReply};
use crate::tools::LARGE_TOOL_RESULT_TOKENS;

/// Total context-window tokens from a hydrogen usage report (matches Oxygen).
pub fn context_tokens(u: &Usage) -> u64 {
    u.total_input_tokens() as u64 + u.output_tokens as u64
}

/// Whether the agent should pause for a budget decision.
pub fn decide_budget_pause(ctx_tokens: u64, budget: u64) -> bool {
    ctx_tokens >= budget
}

/// Apply the user's budget choice. `continue_turn == true` → +100k budget.
pub fn apply_budget_continue(budget: u64, continue_turn: bool) -> BudgetDecision {
    if continue_turn {
        BudgetDecision::Continue {
            new_budget: budget + BUDGET_INCREMENT,
        }
    } else {
        BudgetDecision::Stop
    }
}

/// Whether a tool result should pause for large-result approval.
pub fn is_large_tool_result(tokens: usize) -> bool {
    tokens > LARGE_TOOL_RESULT_TOKENS
}

/// Map the user's large-result reply into a pure decision.
pub fn decide_large_tool_result(reply: LargeToolResultReply) -> LargeResultDecision {
    if reply.approve {
        LargeResultDecision::Approve
    } else {
        LargeResultDecision::Deny {
            message: reply.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::DEFAULT_BUDGET;

    #[test]
    fn budget_pause_and_continue_stop() {
        assert!(decide_budget_pause(200_000, DEFAULT_BUDGET));
        assert!(decide_budget_pause(200_001, DEFAULT_BUDGET));
        assert!(!decide_budget_pause(199_999, DEFAULT_BUDGET));

        assert_eq!(
            apply_budget_continue(DEFAULT_BUDGET, true),
            BudgetDecision::Continue {
                new_budget: DEFAULT_BUDGET + BUDGET_INCREMENT
            }
        );
        assert_eq!(
            apply_budget_continue(DEFAULT_BUDGET, false),
            BudgetDecision::Stop
        );
    }

    #[test]
    fn large_result_approve_deny() {
        assert!(!is_large_tool_result(50_000));
        assert!(is_large_tool_result(50_001));

        assert_eq!(
            decide_large_tool_result(LargeToolResultReply {
                approve: true,
                message: String::new(),
            }),
            LargeResultDecision::Approve
        );
        assert_eq!(
            decide_large_tool_result(LargeToolResultReply {
                approve: false,
                message: "use head".into(),
            }),
            LargeResultDecision::Deny {
                message: "use head".into()
            }
        );
    }

    #[test]
    fn context_tokens_sums_usage() {
        let u = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
        };
        assert_eq!(context_tokens(&u), 20);
    }
}
