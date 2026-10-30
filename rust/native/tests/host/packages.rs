//! Packaged modules: the host loads `<data>/modules/<id>/module.dylib`, reports the
//! packages it could not load and serves each package's console part.

use serde_json::json;
use services_testkit::{DataDir, Host, packages};

fn library() -> String {
    format!("module{}", std::env::consts::DLL_SUFFIX)
}

#[tokio::test]
async fn a_broken_misnamed_or_misdescribed_package_is_reported_and_skipped() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let describe = |dir: &str, package: serde_json::Value| {
        std::fs::write(modules.join(dir).join("package.json"), package.to_string()).unwrap()
    };
    std::fs::create_dir_all(modules.join("broken")).unwrap();
    std::fs::write(modules.join("broken").join(library()), "not a library").unwrap();
    describe("broken", json!({"id":"broken","version":"1.0.0"}));
    for (dir, package) in [
        ("second-rest", "services-module-rest"),
        ("undescribed", "services-module-graphql"),
    ] {
        std::fs::create_dir_all(modules.join(dir)).unwrap();
        std::os::unix::fs::symlink(
            packages::library(package),
            modules.join(dir).join(library()),
        )
        .unwrap();
    }
    describe(
        "second-rest",
        json!({"id":"rest","version":env!("CARGO_PKG_VERSION")}),
    );
    describe("graphql", json!({"id":"graphql","version":"0.0.1"}));
    let host = Host::on(data);

    let reply = host.send(json!({"service":"modules"})).await;
    let ids: Vec<_> = reply.ok()["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.iter().filter(|id| **id == "rest").count(), 1);
    assert!(!ids.contains(&"graphql"));
    let failed: Vec<_> = reply.ok()["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| {
            let package = failure["package"].as_str().unwrap();
            assert_eq!(package.rsplit('/').next(), failure["module"].as_str());
            (
                failure["module"].as_str().unwrap().to_owned(),
                failure["error"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(failed.len(), 4, "{failed:?}");
    assert_eq!(failed[0].0, "broken");
    assert_eq!(
        failed[1],
        (
            "graphql".into(),
            format!(
                "package.json describes graphql 0.0.1 but the module is graphql {}",
                env!("CARGO_PKG_VERSION")
            )
        )
    );
    assert_eq!(
        failed[2],
        (
            "second-rest".into(),
            "the folder of module \"rest\" is named \"second-rest\"; it must be named by the module id"
                .into()
        )
    );
    assert_eq!(failed[3].0, "undescribed");
    assert!(failed[3].1.contains("package.json"), "{}", failed[3].1);
}

fn hidden(modules: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(modules)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect()
}

fn ids(reply: &services_testkit::Reply) -> Vec<String> {
    reply.ok()["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["id"].as_str().unwrap().to_owned())
        .collect()
}

fn install(file: &[u8]) -> serde_json::Value {
    use base64::Engine;
    json!({
        "service": "modules",
        "op": "install",
        "file": base64::engine::general_purpose::STANDARD.encode(file),
    })
}

#[tokio::test]
async fn a_package_file_installs_replaces_and_is_removed_for_the_next_host() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    std::fs::remove_dir_all(modules.join("graphql")).unwrap();
    let host = Host::on(data);
    let version = env!("CARGO_PKG_VERSION");
    let file = packages::file("graphql", version);

    let installed = host.send(install(&file)).await;
    assert_eq!(
        installed.ok(),
        &json!({"installed":{"id":"graphql","name":"GraphQL","version":version},"replaced":null})
    );
    let installed_library = modules.join("graphql").join(library());
    assert!(
        std::fs::symlink_metadata(&installed_library)
            .unwrap()
            .is_file()
    );
    let signature = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(&installed_library)
        .output()
        .unwrap();
    assert!(signature.status.success(), "{signature:?}");
    assert_eq!(hidden(&modules), Vec::<String>::new());
    // This host keeps what it loaded; the next one loads the change.
    assert!(!ids(&host.send(json!({"service":"modules"})).await).contains(&"graphql".into()));
    let next = host.reopen();
    assert!(ids(&next.send(json!({"service":"modules"})).await).contains(&"graphql".into()));
    let graphql = next
        .call(
            "graphql",
            json!({"endpoint":"http://127.0.0.1:9/graphql","query":"{ a }"}),
        )
        .await;
    assert!(graphql.value().get("error").is_some(), "the module runs");

    let again = next.send(install(&file)).await;
    assert_eq!(again.ok()["replaced"], version);
    assert_eq!(hidden(&modules), Vec::<String>::new());

    let removed = next
        .send(json!({"service":"modules","op":"remove","module":"graphql"}))
        .await;
    assert_eq!(removed.ok(), &json!({"removed":"graphql"}));
    assert!(!modules.join("graphql").exists());
    assert_eq!(hidden(&modules), Vec::<String>::new());
    assert!(
        !ids(&next.reopen().send(json!({"service":"modules"})).await).contains(&"graphql".into())
    );
    for (module, error) in [
        ("graphql", "Module \"graphql\" is not installed"),
        ("..", "\"..\" is not a package folder"),
        ("rest/../mysql", "\"rest/../mysql\" is not a package folder"),
        ("", "\"\" is not a package folder"),
    ] {
        let reply = next
            .send(json!({"service":"modules","op":"remove","module":module}))
            .await;
        assert_eq!(reply.error(), error);
    }
    // A development package is a symlink: removing it keeps the build it points to.
    let rest = std::fs::read_link(modules.join("rest").join(library())).unwrap();
    next.send(json!({"service":"modules","op":"remove","module":"rest"}))
        .await
        .ok();
    assert!(!modules.join("rest").exists());
    assert!(rest.is_file());
}

#[tokio::test]
async fn a_module_written_with_the_sdk_installs_from_its_package_file_and_runs() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let host = Host::on(data);

    let installed = host.send(install(&packages::file("echo", "0.1.0"))).await;
    assert_eq!(
        installed.ok(),
        &json!({"installed":{"id":"echo","name":"Echo","version":"0.1.0"},"replaced":null})
    );
    let next = host.reopen();
    let listed = next.send(json!({"service":"modules"})).await;
    let echo = listed.ok()["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == "echo")
        .expect("the installed module is listed");
    assert_eq!(echo["console"]["payload"], "text");
    let reply = next
        .call("echo", json!({"endpoint":"local","text":"hello"}))
        .await;
    assert_eq!(reply.ok()["body"], "hello", "{reply}");
    assert_eq!(reply.ok()["headers"]["x-echo-endpoint"], "local");
    assert_eq!(
        next.call("echo", json!({"endpoint":"local"})).await.error(),
        "text is required"
    );

    let removed = next
        .send(json!({"service":"modules","op":"remove","module":"echo"}))
        .await;
    assert_eq!(removed.ok(), &json!({"removed":"echo"}));
    assert!(!modules.join("echo").exists());
}

#[tokio::test]
async fn an_invalid_package_file_leaves_the_installed_package_alone() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let host = Host::on(data);
    let before = std::fs::read(modules.join("graphql/package.json")).unwrap();

    let misdescribed = host
        .send(install(&packages::file("graphql", "0.0.1")))
        .await;
    assert_eq!(
        misdescribed.error(),
        format!(
            "package.json describes graphql 0.0.1 but the module is graphql {}",
            env!("CARGO_PKG_VERSION")
        )
    );
    let not_base64 = host
        .send(json!({"service":"modules","op":"install","file":"%%"}))
        .await;
    assert!(
        not_base64.error().starts_with("Invalid package file"),
        "{}",
        not_base64.error()
    );
    let garbage = host.send(install(b"not a package")).await;
    assert!(
        garbage.error().starts_with("Invalid package file"),
        "{}",
        garbage.error()
    );

    assert_eq!(
        std::fs::read(modules.join("graphql/package.json")).unwrap(),
        before
    );
    assert!(
        std::fs::symlink_metadata(modules.join("graphql").join(library()))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(hidden(&modules), Vec::<String>::new());
}

#[tokio::test]
async fn the_host_serves_a_package_console_part_by_its_hash() {
    let data = DataDir::new();
    std::fs::write(data.path().join("modules/rest/ui.wasm"), b"\0asm-ui").unwrap();
    let host = Host::on(data);

    let reply = host.send(json!({"service":"modules"})).await;
    assert!(reply.ok().get("failed").is_none());
    let rest = reply.ok()["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == "rest")
        .unwrap()
        .clone();
    let hash = rest["ui"].as_str().unwrap();
    assert_eq!(hash.len(), 64);

    let ui = host
        .send(json!({"service":"modules","op":"ui","module":"rest"}))
        .await;
    assert_eq!(ui.ok()["module"], "rest");
    assert_eq!(ui.ok()["ui"], hash);
    assert_eq!(ui.ok()["wasm"], "AGFzbS11aQ==");

    let built_in = host
        .send(json!({"service":"modules","op":"ui","module":"mysql"}))
        .await;
    assert_eq!(built_in.error(), "Module \"mysql\" has no console part");
}
