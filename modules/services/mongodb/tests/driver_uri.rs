use mongodb::options::{AuthMechanism, ClientOptions};
use services_module_mongodb::config::Connection;
use std::path::PathBuf;

#[tokio::test]
async fn the_driver_accepts_the_x509_uri_the_module_builds() {
    let connection = Connection::parse(
        "mongodb://db.example.net:27017/app?retryWrites=true",
        "Auth: x509\nCertificate: /keys/my client.pem",
        "",
    )
    .unwrap();
    let options = ClientOptions::parse(&connection.uri).await.unwrap();
    let credential = options.credential.expect("a credential");
    assert_eq!(credential.mechanism, Some(AuthMechanism::MongoDbX509));
    assert_eq!(credential.source.as_deref(), Some("$external"));
    assert_eq!(credential.username, None);
    let tls = match options.tls {
        Some(mongodb::options::Tls::Enabled(tls)) => tls,
        other => panic!("TLS is on with a certificate: {other:?}"),
    };
    assert_eq!(
        tls.cert_key_file_path,
        Some(PathBuf::from("/keys/my client.pem"))
    );
}
