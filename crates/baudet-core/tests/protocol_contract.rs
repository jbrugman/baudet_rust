
use baudet_core::*;

#[test]
fn item_wire_names_are_the_app_server_spellings() {
    assert_eq!(ItemKind::UserMessage.wire_name(), "userMessage");
    assert_eq!(ItemKind::AgentMessage.wire_name(), "agentMessage");
    assert_eq!(ItemKind::Reasoning.wire_name(), "reasoning");
    assert_eq!(ItemKind::CommandExecution.wire_name(), "commandExecution");
    assert_eq!(ItemKind::FileChange.wire_name(), "fileChange");
    assert_eq!(ItemKind::Plan.wire_name(), "plan");
    assert_eq!(ItemKind::Error.wire_name(), "error");
    assert_eq!(ItemKind::ToolCall.wire_name(), "baudet/toolCall");
    assert_eq!(ItemKind::Image.wire_name(), "baudet/image");
}

#[test]
fn status_and_decision_wire_names() {
    assert_eq!(ItemStatus::InProgress.wire_name(), "inProgress");
    assert_eq!(ItemStatus::Interrupted.wire_name(), "interrupted");
    assert_eq!(ApprovalDecision::AcceptForSession.wire_name(), "acceptForSession");
    assert_eq!(ApprovalDecision::Decline.wire_name(), "decline");
}

#[test]
fn delta_method_splits_one_stream_per_content_type() {
    assert_eq!(delta_method(ItemKind::AgentMessage), "item/agentMessage/delta");
    assert_eq!(delta_method(ItemKind::Reasoning), "item/reasoning/textDelta");
    assert_eq!(delta_method(ItemKind::CommandExecution), "item/commandExecution/outputDelta");
    assert_eq!(delta_method(ItemKind::FileChange), "item/fileChange/outputDelta");
    assert_eq!(delta_method(ItemKind::Error), "baudet/item/delta");
    assert_eq!(delta_method(ItemKind::Plan), "item/plan/delta");
}

#[test]
fn approval_methods_are_per_request_kind() {
    let command = ApprovalRequest::RunCommand {
        command: "cargo test".into(),
        cwd: Some("/tmp".into()),
        timeout_seconds: Some(30),
    };
    assert_eq!(command.wire_method(), "item/commandExecution/requestApproval");
    let patch = ApprovalRequest::ApplyPatch { path: "src/main.rs".into(), diff: None };
    assert_eq!(patch.wire_method(), "item/fileChange/requestApproval");
    let mcp = ApprovalRequest::McpTool { server_id: "s".into(), tool: "t".into(), arguments: "{}".into() };
    assert_eq!(mcp.wire_method(), "item/mcpTool/requestApproval");
}

fn started() -> ThreadEvent {
    ThreadEvent::ItemStarted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::AgentMessage(AgentMessageItem {
            item_id: "it_1".into(),
            text: String::new(),
            status: ItemStatus::InProgress,
        }),
    }
}

#[test]
fn every_event_scopes_itself_by_thread_and_turn() {
    let events = vec![
        ThreadEvent::TurnStarted { thread_id: "th_1".into(), turn_id: "tu_1".into() },
        started(),
        ThreadEvent::ItemDelta {
            thread_id: "th_1".into(),
            turn_id: "tu_1".into(),
            item_id: "it_1".into(),
            kind: ItemKind::AgentMessage,
            delta: "hi".into(),
        },
        ThreadEvent::TokenUsageUpdated {
            thread_id: "th_1".into(),
            turn_id: "tu_1".into(),
            usage: Usage::new(Some(10), Some(3), Some(13)),
        },
        ThreadEvent::TurnCompleted {
            thread_id: "th_1".into(),
            turn_id: "tu_1".into(),
            response_id: Some("resp_1".into()),
            usage: None,
            final_event: Some("response.completed".into()),
            incomplete_reason: None,
        },
    ];
    for event in &events {
        assert_eq!(event.thread_id(), "th_1", "{}", event.wire_method());
        assert_eq!(event.turn_id(), "tu_1", "{}", event.wire_method());
    }
}

#[test]
fn event_methods_match_the_protocol() {
    assert_eq!(ThreadEvent::TurnStarted { thread_id: "t".into(), turn_id: "t".into() }.wire_method(), "turn/started");
    assert_eq!(started().wire_method(), "item/started");
    assert_eq!(
        ThreadEvent::ItemCompleted {
            thread_id: "t".into(),
            turn_id: "t".into(),
            item: Item::Error(ErrorItem { item_id: "e".into(), message: "boom".into(), status: ItemStatus::Failed }),
        }
        .wire_method(),
        "item/completed"
    );
    assert_eq!(
        ThreadEvent::TokenUsageUpdated { thread_id: "t".into(), turn_id: "t".into(), usage: Usage::default() }
            .wire_method(),
        "thread/tokenUsage/updated"
    );
    assert_eq!(
        ThreadEvent::ApprovalResolved {
            thread_id: "t".into(),
            turn_id: "t".into(),
            item_id: "i".into(),
            decision: ApprovalDecision::Accept,
        }
        .wire_method(),
        "serverRequest/resolved"
    );
}

#[test]
fn a_command_row_keeps_a_missing_exit_code_missing() {
    let item = Item::CommandExecution(CommandExecutionItem {
        item_id: "it_9".into(),
        command: "sleep 999".into(),
        cwd: None,
        timeout_seconds: Some(1),
        status: ItemStatus::Failed,
        exit_code: None,
        timed_out: true,
        duration_ms: 1_004,
        output: String::new(),
        approval: Some(ApprovalDecision::Accept),
    });
    let json = serde_json::to_string(&item).unwrap();
    assert!(!json.contains("exitCode"), "a missing exit code must not serialize as null: {json}");
    let back: Item = serde_json::from_str(&json).unwrap();
    assert_eq!(back, item);
    assert_eq!(back.item_id(), "it_9");
    assert_eq!(back.wire_name(), "commandExecution");
}

#[test]
fn diff_stats_skip_the_headers_a_path_is_not_an_add() {
    let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n+newer\n";
    let stats = DiffStats::of(Some(diff));
    assert_eq!(stats.additions, 2, "the +++ header is a path, not an added line");
    assert_eq!(stats.deletions, 1);
    assert_eq!(DiffStats::of(None).additions, 0);
}

#[test]
fn usage_keeps_a_missing_number_missing() {
    let usage = Usage::new(Some(10), None, None);
    let json = serde_json::to_string(&usage).unwrap();
    assert_eq!(json, "{\"prompt\":10}");
    let back: Usage = serde_json::from_str("{}").unwrap();
    assert_eq!(back, Usage::default());
}

#[test]
fn a_closed_stream_is_not_a_completion() {
    let mut turn = Turn::text("answer");
    assert!(turn.completed());
    turn.final_event = None;
    assert!(!turn.completed(), "a stream that simply closed is not a completion");
    turn.final_event = Some("response.incomplete".into());
    assert!(!turn.completed());
}
