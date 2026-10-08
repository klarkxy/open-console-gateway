use super::*;
use crate::model_metadata::ModelMetadata;

fn model(context: Option<u64>, output: Option<u64>) -> ByokModel {
    ByokModel {
        id: "vendor/new-model".into(),
        metadata: ModelMetadata {
            context_window: context,
            max_output_tokens: output,
            ..Default::default()
        },
        protocols: PublishedModelProtocolProfile {
            preferred: PublishedUpstreamProtocol::Responses,
            supported: vec![PublishedUpstreamProtocol::Responses],
        },
    }
}
#[test]
fn unknown_limits_get_local_budgets_without_mutating_public_metadata() {
    let original = model(None, None);
    let rows =
        with_copilot_token_budget(vec![original.clone()], CopilotTokenBudget::default()).unwrap();
    assert_eq!(original.metadata.context_window, None);
    assert_eq!(original.metadata.max_output_tokens, None);
    assert_eq!(rows[0].metadata.context_window, Some(108_192));
    assert_eq!(rows[0].metadata.max_output_tokens, Some(8_192));
    assert_eq!(rows[0].metadata.tool_calling, None);
    assert_eq!(
        rows[0].protocols.preferred,
        PublishedUpstreamProtocol::Responses
    );
}
#[test]
fn known_limits_constrain_adjustable_budgets() {
    let rows = with_copilot_token_budget(
        vec![model(Some(12_000), Some(2_000))],
        CopilotTokenBudget {
            max_input_tokens: 20_000,
            max_output_tokens: 8_192,
        },
    )
    .unwrap();
    assert_eq!(rows[0].metadata.context_window, Some(12_000));
    assert_eq!(rows[0].metadata.max_output_tokens, Some(1_090));
    let smaller = with_copilot_token_budget(
        rows,
        CopilotTokenBudget {
            max_input_tokens: 4_000,
            max_output_tokens: 1_000,
        },
    )
    .unwrap();
    assert_eq!(smaller[0].metadata.context_window, Some(5_000));
    assert_eq!(smaller[0].metadata.max_output_tokens, Some(1_000));
}
#[test]
fn context_only_limits_reserve_positive_input() {
    let rows =
        with_copilot_token_budget(vec![model(Some(100), None)], CopilotTokenBudget::default())
            .unwrap();
    assert_eq!(rows[0].metadata.context_window, Some(100));
    assert_eq!(rows[0].metadata.max_output_tokens, Some(7));
}
#[test]
fn invalid_and_unusable_budgets_are_rejected() {
    assert!(
        with_copilot_token_budget(
            vec![],
            CopilotTokenBudget {
                max_input_tokens: 0,
                max_output_tokens: 1
            }
        )
        .is_err()
    );
    assert!(
        with_copilot_token_budget(
            vec![],
            CopilotTokenBudget {
                max_input_tokens: 1,
                max_output_tokens: 0
            }
        )
        .is_err()
    );
    assert!(
        with_copilot_token_budget(vec![model(Some(1), Some(1))], CopilotTokenBudget::default())
            .is_err()
    );
}
#[test]
fn large_budget_arithmetic_is_safe() {
    let rows = with_copilot_token_budget(
        vec![model(None, None)],
        CopilotTokenBudget {
            max_input_tokens: u32::MAX,
            max_output_tokens: u32::MAX,
        },
    )
    .unwrap();
    assert_eq!(
        rows[0].metadata.context_window,
        Some(u64::from(u32::MAX) * 2)
    );
}
