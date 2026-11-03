use std::collections::BTreeSet;

fn declared_methods(release: &serde_json::Value, key: &str) -> BTreeSet<String> {
    let methods: Vec<String> = serde_json::from_value(release["notifications"][key].clone())
        .unwrap_or_else(|error| panic!("release.json notifications.{key} must be a method list: {error}"));
    let unique: BTreeSet<String> = methods.iter().cloned().collect();
    assert_eq!(unique.len(), methods.len(), "release.json notifications.{key} repeats a method");
    unique
}

#[test]
fn declared_notification_policy_equals_the_vendored_server_notification_surface() {
    let release: serde_json::Value =
        serde_json::from_str(include_str!("../managed-cli/release.json")).unwrap();
    let mirrored = declared_methods(&release, "mirrored");
    let opt_out = declared_methods(&release, "opt_out");
    assert!(
        mirrored.is_disjoint(&opt_out),
        "a notification cannot be both mirrored and opted out: {:?}",
        mirrored.intersection(&opt_out).collect::<Vec<_>>()
    );

    let schemas = tempfile::tempdir().unwrap();
    octopus_appserver_proto::generate_json_with_experimental(schemas.path(), true).unwrap();
    let schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(schemas.path().join("ServerNotification.json")).unwrap(),
    )
    .unwrap();
    let vendored: BTreeSet<String> = schema["oneOf"]
        .as_array()
        .expect("ServerNotification schema is a oneOf of notifications")
        .iter()
        .map(|variant| {
            variant["properties"]["method"]["enum"][0]
                .as_str()
                .expect("every notification declares one method")
                .to_string()
        })
        .collect();

    assert_eq!(
        vendored.difference(&mirrored).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "vendored notifications missing from release.json notifications.mirrored"
    );
    assert_eq!(
        mirrored.difference(&vendored).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "release.json notifications.mirrored lists methods the vendored protocol cannot decode"
    );
}
