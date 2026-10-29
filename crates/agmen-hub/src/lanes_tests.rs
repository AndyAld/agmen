use super::*;

#[test]
fn lane_files_parse_as_json_and_toml() {
    let json_type = HeaderMap::from_iter([(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8".parse().unwrap(),
    )]);
    let file = parse_lane_file(
        &json_type,
        r#"{"lanes":[{"name":"L-UI","globs":["ui/**"],"owner":"andy/sol"}]}"#,
    )
    .unwrap();
    assert_eq!(file.lanes[0].owner.as_deref(), Some("andy/sol"));
    let toml_type =
        HeaderMap::from_iter([(header::CONTENT_TYPE, "application/toml".parse().unwrap())]);
    let file = parse_lane_file(
        &toml_type,
        "[[lanes]]\nname = \"L-UI\"\nglobs = [\"ui/**\"]\n",
    )
    .unwrap();
    assert_eq!(file.lanes[0].name.as_str(), "L-UI");
    assert!(parse_lane_file(&toml_type, "[[lanes]]\nname = \"ui\"\nglobs = []\n").is_err());
    assert!(parse_lane_file(&json_type, r#"{"lanes":[],"extra":1}"#).is_err());
    assert!(parse_lane_file(&HeaderMap::new(), "{}").is_err());
}
