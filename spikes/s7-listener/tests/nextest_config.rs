const CONFIG: &str = include_str!("../../../.config/nextest.toml");

#[test]
fn default_excludes_live_model_and_integration_tests() {
    assert!(CONFIG.contains("default-filter = \"not test(/(^|::)(it|live_model)_/)\""));
}

#[test]
fn profile_live_selects_only_live_model_tests() {
    assert!(CONFIG.contains("[profile.live]\ndefault-filter = \"test(/(^|::)live_model_/)\""));
}

#[test]
fn integration_profiles_keep_their_filters_and_serialization() {
    assert!(CONFIG.contains("default-filter = \"test(/(^|::)it_/) and not test(/restart/)\""));
    assert!(CONFIG.contains("default-filter = \"test(/(^|::)it_/) and test(/restart/)\""));
    assert!(CONFIG.contains("filter = \"test(/toxiproxy/) | test(=it_gap_filled_by_replay) | package(agmen-e2e)\"\ntest-group = \"serial\""));
}
