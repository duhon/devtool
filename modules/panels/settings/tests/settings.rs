use serde_json::json;
use services_module_settings::{Msg, Settings, summary};
use services_sdk::{Cmd, Event, Module};

fn names(cmds: &[Cmd]) -> Vec<&str> {
    cmds.iter().map(|cmd| cmd.name.as_str()).collect()
}

#[test]
fn the_check_reports_errors_and_newer_versions() {
    let check = json!({
        "sources": [
            {"slug": "team", "name": "Team", "modules": [{"id": "rest", "version": "1.2.0"}]},
            {"slug": "broken", "name": "Broken", "error": "404"},
        ],
        "installed": {"rest": {"source": "team", "version": "1.1.0"}},
    });
    assert_eq!(
        summary(&check),
        (
            vec!["Broken: 404".to_owned()],
            vec!["rest 1.2.0".to_owned()]
        )
    );
}

#[test]
fn the_sources_tab_loads_and_removes_a_source() {
    let mut settings = Settings::default();
    let cmds = settings.update(Event::Init {
        settings: json!({}),
    });
    assert_eq!(names(&cmds), ["kv.get", "modules.check"]);
    let cmds = settings.update(Event::Ui {
        msg: Msg::Tab,
        value: Some("settings-tab-sources".into()),
    });
    assert_eq!(names(&cmds)[..2], ["kv.put", "sources.list"]);
    settings.update(Event::Reply {
        tag: "sources".into(),
        result: Ok(json!({"sources": [
            {"slug": "a", "name": "A", "url": "https://example.com/a"},
            {"slug": "b", "name": "B", "url": "https://example.com/b"},
        ]})),
    });
    let view = format!("{:?}", settings.view());
    assert!(
        view.contains("source-name-a") && view.contains("source-name-b"),
        "{view}"
    );
    settings.update(Event::Ui {
        msg: Msg::RemoveSource(Some(0)),
        value: None,
    });
    let cmds = settings.update(Event::Ui {
        msg: Msg::RemoveSourceConfirm,
        value: None,
    });
    assert_eq!(cmds[0].name, "sources.save");
    let order: Vec<_> = cmds[0].args()["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["id"].clone())
        .collect();
    assert_eq!(order, [json!("b")]);
}

#[test]
fn installing_the_app_row_updates_the_app() {
    let mut settings = Settings::default();
    let cmds = settings.update(Event::Ui {
        msg: Msg::Install("team".into(), "app".into(), "9.0.0".into()),
        value: None,
    });
    assert_eq!(cmds[0].name, "app.update");
    let cmds = settings.update(Event::Ui {
        msg: Msg::Install("team".into(), "rest".into(), "1.2.0".into()),
        value: None,
    });
    assert_eq!(cmds[0].name, "modules.install");
}

#[test]
fn update_all_updates_installed_modules_one_by_one_and_the_app_last() {
    let mut settings = Settings::default();
    settings.update(Event::Init {
        settings: json!({}),
    });
    let check = json!({
        "sources": [{"slug": "team", "name": "Team", "modules": [
            {"id": "app", "version": "9.0.0"},
            {"id": "kafka", "version": "1.1.0"},
            {"id": "new", "version": "1.0.0"},
            {"id": "rest", "version": "2.0.0"},
            {"id": "same", "version": "1.0.0"},
        ]}],
        "installed": {
            "app": {"source": "team", "version": "8.0.0"},
            "kafka": {"source": "team", "version": "1.0.0", "name": "Kafka"},
            "rest": {"source": "team", "version": "1.0.0", "name": "REST"},
            "same": {"source": "team", "version": "1.0.0"},
        },
    });
    settings.update(Event::Reply {
        tag: "check:1".into(),
        result: Ok(check),
    });
    let install = |cmds: &[Cmd]| {
        assert_eq!(cmds[0].name, "modules.install");
        let args = cmds[0].args();
        (args["module"].clone(), args["version"].clone())
    };
    let cmds = settings.update(Event::Ui {
        msg: Msg::UpdateAll,
        value: None,
    });
    assert_eq!(install(&cmds), (json!("kafka"), json!("1.1.0")));
    assert_eq!(cmds[1].args()["text"], "Update all · Running… 1/2 Kafka");
    let cmds = settings.update(Event::Reply {
        tag: "change".into(),
        result: Err(services_sdk::Failure::new("broken")),
    });
    assert_eq!(install(&cmds), (json!("rest"), json!("2.0.0")));
    let cmds = settings.update(Event::Reply {
        tag: "change".into(),
        result: Ok(json!({"installed": {"name": "REST", "version": "2.0.0"}})),
    });
    assert_eq!(cmds[0].name, "app.update");
    assert_eq!(cmds[0].args()["version"], "9.0.0");
    let cmds = settings.update(Event::Reply {
        tag: "update".into(),
        result: Ok(json!({})),
    });
    let status = cmds.last().unwrap().args();
    assert_eq!(
        status["text"],
        "Update all · Updated REST 2.0.0 · ERROR · Kafka: broken · Update installed. Reload the extension to finish."
    );
    assert_eq!(status["indicator"], "error");
}

#[test]
fn a_sync_opens_each_conflict_in_diff_and_resolves_it_on_done() {
    let mut settings = Settings::default();
    settings.update(Event::Init {
        settings: json!({}),
    });
    settings.update(Event::Ui {
        msg: Msg::Tab,
        value: Some("settings-tab-sources".into()),
    });
    settings.update(Event::Reply {
        tag: "sources".into(),
        result: Ok(json!({"sources": [{"slug": "team", "name": "Team"}]})),
    });
    let cmds = settings.update(Event::Ui {
        msg: Msg::Sync("team".into()),
        value: None,
    });
    assert_eq!(cmds[0].name, "sources.sync");
    let cmds = settings.update(Event::Reply {
        tag: "sync".into(),
        result: Ok(
            json!({"conflicts": [{"file": "dictionary.json", "left": "{}", "right": "{\"a\":1}"}]}),
        ),
    });
    assert_eq!(cmds[0].name, "diff.open");
    assert_eq!(cmds[0].args()["left_title"], "Local");
    assert_eq!(cmds[0].args()["right_title"], "Team");
    let cmds = settings.update(Event::Reply {
        tag: "diff".into(),
        result: Ok(json!({"text": "{\"a\":1}"})),
    });
    assert_eq!(cmds[0].name, "sources.resolve");
    assert_eq!(cmds[0].args()["file"], "dictionary.json");
    let cmds = settings.update(Event::Reply {
        tag: "resolve".into(),
        result: Ok(json!({"written": true})),
    });
    assert!(names(&cmds).contains(&"modules.check"));
}
