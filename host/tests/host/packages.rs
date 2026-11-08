//! Packaged modules: the host serves `<data>/modules/<id>/{package.json,module.wasm}` to the
//! App, reports the packages it cannot serve, installs package files and removes packages.

use serde_json::json;
use services_contracts::package::{PACKAGE_FILE, WASM_FILE, archive};
use services_testkit::{DataDir, Host, packages};

fn listed(reply: &services_testkit::Reply) -> Vec<String> {
    reply.ok()["wasm"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn ids(host: &Host) -> Vec<String> {
    listed(&host.send(json!({"service":"modules"})).await)
}

#[tokio::test]
async fn a_broken_misnamed_or_undescribed_package_is_reported_and_skipped() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let describe = |dir: &str, package: serde_json::Value| {
        std::fs::write(modules.join(dir).join(PACKAGE_FILE), package.to_string()).unwrap()
    };
    std::fs::create_dir_all(modules.join("broken")).unwrap();
    std::fs::write(modules.join("broken").join(WASM_FILE), b"\0asm").unwrap();
    describe("broken", json!({"id":"broken","version":"1.0.0"}));
    for (dir, package) in [
        ("second-grpc", "services-module-grpc"),
        ("undescribed", "services-module-artemis"),
    ] {
        std::fs::create_dir_all(modules.join(dir)).unwrap();
        std::os::unix::fs::symlink(packages::wasm(package), modules.join(dir).join(WASM_FILE))
            .unwrap();
    }
    let grpc = std::fs::read_to_string(modules.join("grpc").join(PACKAGE_FILE)).unwrap();
    std::fs::write(modules.join("second-grpc").join(PACKAGE_FILE), grpc).unwrap();
    let host = Host::on(data);

    let reply = host.send(json!({"service":"modules"})).await;
    let ids = listed(&reply);
    assert_eq!(ids.iter().filter(|id| *id == "grpc").count(), 1);
    let mut failed: Vec<_> = reply.ok()["failed"]
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
    failed.sort();
    assert_eq!(failed.len(), 3, "{failed:?}");
    assert_eq!(failed[0].0, "broken");
    assert_eq!(
        failed[1],
        (
            "second-grpc".into(),
            "the folder of module \"grpc\" is named \"second-grpc\"; it must be named by the module id"
                .into()
        )
    );
    assert_eq!(failed[2].0, "undescribed");
    assert!(failed[2].1.contains(PACKAGE_FILE), "{}", failed[2].1);
}

fn hidden(modules: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(modules)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect()
}

/// A checked folder source "Team" offering the package `file`.
async fn team_offering(host: &Host, file: Vec<u8>) -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(folder.path().join("modules")).unwrap();
    std::fs::write(package(&folder), file).unwrap();
    host.send(json!({"service":"sources","op":"save",
        "sources":[{"name":"Team","kind":"folder","path":folder.path()}]}))
        .await
        .ok();
    host.send(json!({"service":"sources","op":"check","force":true}))
        .await
        .ok();
    folder
}

/// A checked folder source "Team" offering the package file of `id` `version`.
async fn team(host: &Host, id: &str, version: &str) -> tempfile::TempDir {
    team_offering(host, packages::file(id, version)).await
}

fn package(team: &tempfile::TempDir) -> std::path::PathBuf {
    team.path().join("modules/offered.module.tgz")
}

fn install(id: &str, version: &str) -> serde_json::Value {
    json!({"service":"modules","op":"install","source":"team","module":id,"version":version})
}

#[tokio::test]
async fn a_package_file_installs_replaces_and_is_removed_for_the_next_host() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    std::fs::remove_dir_all(modules.join("artemis")).unwrap();
    let host = Host::on(data);
    let version = packages::version("artemis");
    let _team = team(&host, "artemis", version).await;

    let installed = host.send(install("artemis", version)).await;
    assert_eq!(
        installed.ok(),
        &json!({"installed":{"id":"artemis","name":"Artemis","version":version},"replaced":null})
    );
    let installed_module = modules.join("artemis").join(WASM_FILE);
    assert!(
        std::fs::symlink_metadata(&installed_module)
            .unwrap()
            .is_file()
    );
    assert_eq!(hidden(&modules), Vec::<String>::new());
    // This host keeps what it loaded; the next one loads the change.
    assert!(!ids(&host).await.contains(&"artemis".into()));
    let next = host.reopen();
    assert!(ids(&next).await.contains(&"artemis".into()));
    let served = next
        .send(json!({"service":"modules","op":"wasm","module":"artemis"}))
        .await;
    assert!(served.ok()["wasm"].as_str().unwrap().starts_with("AGFzbQ"));

    let again = next.send(install("artemis", version)).await;
    assert_eq!(again.ok()["replaced"], version);
    assert_eq!(hidden(&modules), Vec::<String>::new());

    let removed = next
        .send(json!({"service":"modules","op":"remove","module":"artemis"}))
        .await;
    assert_eq!(removed.ok(), &json!({"removed":"artemis"}));
    assert!(!modules.join("artemis").exists());
    assert_eq!(hidden(&modules), Vec::<String>::new());
    assert!(!ids(&next.reopen()).await.contains(&"artemis".into()));
    for (module, error) in [
        ("artemis", "Module \"artemis\" is not installed"),
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
    let grpc = std::fs::read_link(modules.join("grpc").join(WASM_FILE)).unwrap();
    next.send(json!({"service":"modules","op":"remove","module":"grpc"}))
        .await
        .ok();
    assert!(!modules.join("grpc").exists());
    assert!(grpc.is_file());
}

#[tokio::test]
async fn a_module_written_with_the_sdk_installs_from_its_package_file_and_is_served() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let host = Host::on(data);
    let _team = team(&host, "echo", "0.1.0").await;

    let installed = host.send(install("echo", "0.1.0")).await;
    assert_eq!(
        installed.ok(),
        &json!({"installed":{"id":"echo","name":"Echo","version":"0.1.0"},"replaced":null})
    );
    let next = host.reopen();
    let listed = next.send(json!({"service":"modules"})).await;
    let echo = listed.ok()["wasm"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == "echo")
        .expect("the installed module is listed")
        .clone();
    assert_eq!(echo["kind"], "service");
    assert_eq!(echo["version"], "0.1.0");
    let served = next
        .send(json!({"service":"modules","op":"wasm","module":"echo"}))
        .await;
    assert_eq!(served.ok()["digest"], echo["digest"]);

    let removed = next
        .send(json!({"service":"modules","op":"remove","module":"echo"}))
        .await;
    assert_eq!(removed.ok(), &json!({"removed":"echo"}));
    assert!(!modules.join("echo").exists());
}

#[tokio::test]
async fn removing_a_module_with_its_data_deletes_its_history_and_hints() {
    const ARTEMIS: &str = "019a1234-5678-7890-abcd-0123456789a1";
    const MYSQL: &str = "019a1234-5678-7890-abcd-0123456789a2";
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let host = Host::on(data);
    for (service, check) in [("artemis", ARTEMIS), ("mysql", MYSQL)] {
        host.runtime()
            .history
            .save(services_native::core::history::Snapshot {
                check_id: check.into(),
                session_id: check.into(),
                title: "#check kept".into(),
                service: service.into(),
                started: 1,
                ended: 2,
                duration: 1,
                status: "OK".into(),
                text: "#check kept".into(),
            })
            .await
            .unwrap();
    }
    let total = |reply: services_testkit::Reply| reply.ok()["total"].clone();
    let check = |check: &str| json!({"checkId":check});

    let kept = host
        .send(json!({"service":"modules","op":"remove","module":"mysql"}))
        .await;
    assert_eq!(kept.ok(), &json!({"removed":"mysql"}));
    assert_eq!(total(host.op("history", "list", check(MYSQL)).await), 1);

    let removed = host
        .send(json!({"service":"modules","op":"remove","module":"artemis","forget":true}))
        .await;
    assert_eq!(removed.ok(), &json!({"removed":"artemis","forgotten":true}));
    assert!(!modules.join("artemis").exists());
    assert_eq!(total(host.op("history", "list", check(ARTEMIS)).await), 0);
    assert_eq!(total(host.op("history", "list", check(MYSQL)).await), 1);
}

#[tokio::test]
async fn an_invalid_package_file_leaves_the_installed_package_alone() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let host = Host::on(data);
    let before = std::fs::read(modules.join("artemis").join(PACKAGE_FILE)).unwrap();

    let staged = tempfile::tempdir().unwrap();
    std::fs::write(
        staged.path().join(PACKAGE_FILE),
        json!({"id":"artemis","kind":"service","name":"Artemis","version":"0.0.1",
            "abi":99,"drivers":{"http":"1"}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(staged.path().join(WASM_FILE), b"\0asm").unwrap();
    let team = team_offering(&host, archive::pack(staged.path()).unwrap()).await;
    let unsupported = host.send(install("artemis", "0.0.1")).await;
    assert_eq!(
        unsupported.error(),
        format!(
            "Artemis was built for module ABI 99, the App runs ABI {}",
            services_sdk::ABI
        )
    );
    // The file changed after the check.
    std::fs::write(package(&team), "not a package").unwrap();
    let garbage = host.send(install("artemis", "0.0.1")).await;
    assert!(
        garbage.error().starts_with("Invalid package file"),
        "{}",
        garbage.error()
    );

    assert_eq!(
        std::fs::read(modules.join("artemis").join(PACKAGE_FILE)).unwrap(),
        before
    );
    assert!(
        std::fs::symlink_metadata(modules.join("artemis").join(WASM_FILE))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(hidden(&modules), Vec::<String>::new());
}

#[tokio::test]
async fn the_host_lists_and_serves_wasm_modules_it_can_run_and_reports_the_rest() {
    let data = DataDir::new();
    let modules = data.path().join("modules");
    let place = |id: &str, manifest: serde_json::Value| {
        std::fs::create_dir_all(modules.join(id)).unwrap();
        std::fs::write(modules.join(id).join(PACKAGE_FILE), manifest.to_string()).unwrap();
        std::fs::write(modules.join(id).join(WASM_FILE), b"\0asm").unwrap();
    };
    let manifest = |id: &str, abi: u32, driver: &str| {
        json!({"id":id,"kind":"service","name":id.to_uppercase(),"version":"1.0.0",
            "abi":abi,"drivers":{driver:"1"},"order":7})
    };
    place("wasm-ok", manifest("wasm-ok", 1, "http"));
    place("wasm-old", manifest("wasm-old", 99, "http"));
    place("wasm-kafka", manifest("wasm-kafka", 1, "kafka"));
    let host = Host::on(data);

    let reply = host.send(json!({"service":"modules"})).await;
    let wasm: Vec<_> = reply.ok()["wasm"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|module| module["id"].as_str().unwrap().starts_with("wasm-"))
        .collect();
    assert_eq!(wasm.len(), 1, "{reply:?}");
    assert_eq!(wasm[0]["id"], "wasm-ok");
    assert_eq!(wasm[0]["order"], 7);
    assert_eq!(wasm[0]["digest"].as_str().unwrap().len(), 64);
    let failed: Vec<_> = reply.ok()["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| failure["module"].as_str().unwrap())
        .collect();
    assert!(failed.contains(&"wasm-old") && failed.contains(&"wasm-kafka"));

    let served = host
        .send(json!({"service":"modules","op":"wasm","module":"wasm-ok"}))
        .await;
    assert_eq!(served.ok()["wasm"], "AGFzbQ==");
    assert_eq!(served.ok()["digest"], wasm[0]["digest"]);
    let missing = host
        .send(json!({"service":"modules","op":"wasm","module":"kafka"}))
        .await;
    assert_eq!(missing.error(), "Module \"kafka\" is not a wasm module");
}
