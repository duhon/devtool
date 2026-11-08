use serde_json::{Value, json};
use services_module_agent::{Agent, Msg};
use services_sdk::{Cmd, Event, Module};

fn names(cmds: &[Cmd]) -> Vec<&str> {
    cmds.iter().map(|cmd| cmd.name.as_str()).collect()
}

fn find<'a>(cmds: &'a [Cmd], name: &str) -> &'a Cmd {
    cmds.iter()
        .find(|cmd| cmd.name == name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", names(cmds)))
}

fn reply(agent: &mut Agent, tag: &str, value: Value) -> Vec<Cmd> {
    agent.update(Event::Reply {
        tag: tag.into(),
        result: Ok(value),
    })
}

/// A loaded panel in console session 1 with a blank chat open.
fn loaded() -> Agent {
    let mut agent = Agent::default();
    let cmds = agent.update(Event::Init {
        settings: json!({"instance": "w1"}),
    });
    assert_eq!(
        names(&cmds)[..4],
        ["kv.watch", "kv.get", "workspace.list", "agent.options"]
    );
    reply(&mut agent, "chats", Value::Null);
    reply(
        &mut agent,
        "sessions",
        json!({"current": 1, "sessions": [{"id": 1, "name": "Work"}]}),
    );
    reply(&mut agent, "active", Value::Null);
    agent
}

#[test]
fn a_sent_message_runs_a_turn_and_keeps_its_answer() {
    let mut agent = loaded();
    let view = format!("{:?}", agent.view());
    assert!(view.contains("agent-intro"), "{view}");
    agent.update(Event::Ui {
        msg: Msg::Draft,
        value: Some("What failed?".into()),
    });
    let cmds = agent.update(Event::Ui {
        msg: Msg::Send,
        value: None,
    });
    let turn = find(&cmds, "agent.turn");
    let tag = turn.tag.clone();
    let chat = turn.args()["sessionId"].as_str().unwrap().to_owned();
    assert!(chat.starts_with("w1-"), "{chat}");
    assert_eq!(turn.args()["prompt"], "What failed?");
    // The chat is no longer blank, so it is saved with its question.
    let saved = find(&cmds, "kv.put").args();
    assert_eq!(saved["value"][&chat]["messages"][0]["text"], "What failed?");
    // A second message waits for the running turn.
    agent.update(Event::Ui {
        msg: Msg::Draft,
        value: Some("again".into()),
    });
    let cmds = agent.update(Event::Ui {
        msg: Msg::Send,
        value: None,
    });
    assert!(!names(&cmds).contains(&"agent.turn"));

    reply(
        &mut agent,
        &tag,
        json!({"type": "copilot", "event": "assistant.message_delta",
            "data": {"messageId": "m1", "deltaContent": "The check"}}),
    );
    reply(
        &mut agent,
        &tag,
        json!({"type": "copilot", "event": "assistant.message",
            "data": {"messageId": "m1", "content": "The check timed out."}}),
    );
    let cmds = reply(&mut agent, &tag, json!({"type": "result", "result": {}}));
    let saved = find(&cmds, "kv.put").args();
    let messages = &saved["value"][&chat]["messages"];
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["text"], "The check timed out.");
    let view = format!("{:?}", agent.view());
    assert!(view.contains("agent-message-assistant-m1"), "{view}");
}

#[test]
fn hiding_the_panel_stops_its_turns_and_page_calls_of_unknown_turns_are_revoked() {
    let mut agent = loaded();
    let cmds = agent.update(Event::ToolCall {
        tag: "call-1".into(),
        name: "page_read".into(),
        args: json!({"turn": "turn:9", "arguments": {}}),
    });
    let revoked = find(&cmds, "agent.reply");
    assert_eq!(revoked.tag, "call-1");
    assert!(revoked.args()["result"]["error"].is_string());

    agent.update(Event::Ui {
        msg: Msg::Send,
        value: Some("Hi".into()),
    });
    let cmds = agent.update(Event::Hidden);
    assert!(names(&cmds).contains(&"agent.stop"), "{:?}", names(&cmds));
    let saved = find(&cmds, "kv.put").args();
    let chat = saved["value"].as_object().unwrap().values().next().unwrap();
    assert_eq!(
        chat["messages"].as_array().unwrap().last().unwrap()["role"],
        "notice"
    );
}

#[test]
fn chats_of_another_window_are_merged_and_closed_ones_restored() {
    let mut agent = loaded();
    let other = json!({"w2-1": {"id": "w2-1", "title": "Other window", "draft": "",
        "messages": [{"id": "u", "role": "user", "text": "hello"}],
        "options": {}, "console_session": 1, "updated": 1.0, "created": 1.0,
        "closed": 2.0}});
    let cmds = agent.update(Event::Storage {
        key: "chats".into(),
    });
    assert_eq!(find(&cmds, "kv.get").tag, "chats");
    reply(&mut agent, "chats", other);
    agent.update(Event::Ui {
        msg: Msg::ToggleList,
        value: None,
    });
    let view = format!("{:?}", agent.view());
    assert!(view.contains("agent-chat-row-w2-1"), "{view}");
    agent.update(Event::Ui {
        msg: Msg::Restore("w2-1".into()),
        value: None,
    });
    let view = format!("{:?}", agent.view());
    assert!(view.contains("agent-chat-w2-1"), "{view}");
    assert!(view.contains("agent-message-user-u"), "{view}");
}
