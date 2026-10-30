use services_native::Runtime;

#[test]
fn runtime_selects_a_tls_provider_before_any_service_connects() {
    let data = std::env::temp_dir().join(format!("services-tls-test-{}", std::process::id()));
    let _first = Runtime::with_data_dir(&data).unwrap();
    let provider = rustls::crypto::CryptoProvider::get_default().unwrap();
    let _second = Runtime::with_data_dir(&data).unwrap();
    assert!(std::ptr::eq(
        provider,
        rustls::crypto::CryptoProvider::get_default().unwrap()
    ));
    let _config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
}
