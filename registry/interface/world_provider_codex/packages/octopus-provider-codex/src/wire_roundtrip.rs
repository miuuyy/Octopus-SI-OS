use super::*;

#[test]
fn emitted_labels_match_the_operator_projection_vocabulary() {
    use octopus_app_server_interface as projection;
    assert_eq!(
        AGENT_MESSAGE_DELTA_LABEL,
        projection::AGENT_MESSAGE_DELTA_LABEL
    );
    assert_eq!(TURN_EXEC_COMMAND_LABEL, projection::TURN_EXEC_COMMAND_LABEL);
    assert_eq!(TURN_REASONING_LABEL, projection::TURN_REASONING_LABEL);
    assert_eq!(TURN_PLAN_UPDATE_LABEL, projection::TURN_PLAN_UPDATE_LABEL);
    assert_eq!(TOOL_CALL_LABEL, projection::TOOL_CALL_LABEL);
    assert_eq!(TURN_ERROR_LABEL, projection::TURN_ERROR_LABEL);
    assert_eq!(TURN_WEB_SEARCH_LABEL, projection::TURN_WEB_SEARCH_LABEL);
    assert_eq!(GOAL_STATUS_LABEL, projection::GOAL_STATUS_LABEL);
    assert_eq!(HOOK_RESULT_LABEL, projection::HOOK_RESULT_LABEL);
    assert_eq!(HOOK_CATALOG_LABEL, projection::HOOK_CATALOG_LABEL);
    assert_eq!(
        OPERATOR_APPROVAL_REQUEST_LABEL,
        projection::OPERATOR_APPROVAL_REQUEST_LABEL
    );
    assert_eq!(
        OPERATOR_ELICITATION_REQUEST_LABEL,
        projection::OPERATOR_ELICITATION_REQUEST_LABEL
    );
    assert_eq!(
        INTERFACE_ARTIFACT_OUTPUT_LABEL,
        projection::ACT_OBSERVATION_REPLY_LABEL,
        "the terminating text reply is the client's TerminalReply lane"
    );
}
