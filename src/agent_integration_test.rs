//! Integration tests for the agent loop using a mock OpenAI-compatible SSE server.
//! These validate harness behavior without a live LLM.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use crate::agent::{run_agent, AgentEvent};
use crate::agent_eval::{evaluate_checks, AgentEvalCheck, AgentEvalFixture};
use crate::backends::{BackendDescriptor, BackendName};
use crate::config::AgentConfig;
use crate::openai::{build_http_client, ChatMessage};
use crate::tools::build_tools_for_names;

fn mock_backend(listener: &TcpListener) -> BackendDescriptor {
    BackendDescriptor {
        name: BackendName::Ollama,
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        api_key: "test".into(),
        is_local: true,
        openrouter: crate::backends::OpenRouterConfig::default(),
    }
}

fn spawn_mock_server(
    listener: TcpListener,
    bodies: Vec<&'static str>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for body in bodies {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        }
    })
}

fn fixture_workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evals/fixtures/fix-failing-test")
}

#[tokio::test]
async fn read_and_explain_mock_loop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let backend_desc = mock_backend(&listener);
    let tool_call_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":",
        "{\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let answer_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"The add function sums two integers.\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_mock_server(listener, vec![tool_call_body, answer_body]);

    let mut config = AgentConfig::default();
    config.workspace_root = fixture_workspace().display().to_string();
    config.approval_policy = crate::config::ApprovalPolicy::Never;
    config.tools = vec!["file_read".into()];
    config.tool_selection = crate::config::ToolSelection::Fixed;

    let messages = vec![
        ChatMessage::System {
            content: "test".into(),
        },
        ChatMessage::User {
            content: "Read src/lib.rs and explain add.".into(),
        },
    ];
    let tools = build_tools_for_names(&config, &config.tools, None);
    let http = build_http_client();
    let mut tool_calls = Vec::new();

    let run = run_agent(
        &http,
        &backend_desc,
        "mock",
        messages,
        tools,
        6,
        |event| {
            if let AgentEvent::ToolCall { name, .. } = event {
                tool_calls.push(name);
            }
        },
        None,
        None,
        None,
        None,
        None,
        0,
    )
    .await
    .unwrap();

    server.join().unwrap();

    let fixture = AgentEvalFixture {
        id: "mock-read".into(),
        prompt: String::new(),
        workspace: None,
        checks: vec![
            AgentEvalCheck::ToolUsed {
                name: "file_read".into(),
            },
            AgentEvalCheck::AssistantMentions {
                needle: "add".into(),
            },
        ],
    };
    let checks = evaluate_checks(&fixture_workspace(), &fixture.checks, &run, &tool_calls);
    assert!(checks.iter().all(|c| c.passed), "{checks:?}");
    assert!(!run.hit_step_limit);
}

#[tokio::test]
async fn step_limit_surfaces_hit_step_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let backend_desc = mock_backend(&listener);
    let tool_call_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":",
        "{\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_mock_server(listener, vec![tool_call_body, tool_call_body]);

    let mut config = AgentConfig::default();
    config.workspace_root = fixture_workspace().display().to_string();
    config.tools = vec!["file_read".into()];
    config.tool_selection = crate::config::ToolSelection::Fixed;

    let messages = vec![ChatMessage::User {
        content: "keep reading".into(),
    }];
    let tools = build_tools_for_names(&config, &config.tools, None);
    let http = build_http_client();
    let mut hit_event = false;

    let run = run_agent(
        &http,
        &backend_desc,
        "mock",
        messages,
        tools,
        2,
        |event| {
            if matches!(event, AgentEvent::StepLimitReached { .. }) {
                hit_event = true;
            }
        },
        None,
        None,
        None,
        None,
        None,
        0,
    )
    .await
    .unwrap();

    server.join().unwrap();
    assert!(run.hit_step_limit);
    assert!(hit_event);
}
