use super::*;

#[test]
fn read_evidence_and_paged_listing_respect_exact_room_roots() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let roots = BTreeMap::from([
        ("absent".into(), outside.path().join("absent")),
        ("documents".into(), root.path().to_path_buf()),
    ]);
    let body = b"{\"checkpoint\":\"projected\"}\n";
    let file = root.path().join("a.json");
    std::fs::write(&file, body).unwrap();
    std::fs::write(root.path().join("b.json"), b"{}").unwrap();
    std::fs::write(outside.path().join("private.json"), b"private").unwrap();
    let config = serde_json::json!({"operation":"read","max_bytes":1024});
    let value = execute(&config, &serde_json::json!({"path":file}), &roots).unwrap();
    assert_eq!(value["content_hash"], ContentHash::of_bytes(body).to_hex());
    assert_eq!(value["content"], std::str::from_utf8(body).unwrap());
    let selected = execute(&config, &serde_json::json!({"path":file,"json_pointers":["/checkpoint"]}), &roots).unwrap();
    assert_eq!(selected["selected"]["/checkpoint"], "projected");
    assert_eq!(selected["content_hash"], value["content_hash"]);
    assert!(selected.get("content").is_none());
    for pointer in ["checkpoint", "/missing", "/bad~2escape"] {
        assert!(execute(&config, &serde_json::json!({"path":file,"json_pointers":[pointer]}), &roots).is_err());
    }
    assert!(execute(
        &config,
        &serde_json::json!({"path":outside.path().join("private.json")}),
        &roots
    )
    .unwrap_err()
    .contains("outside"));
    assert!(execute(
        &serde_json::json!({"operation":"read","max_bytes":2}),
        &serde_json::json!({"path":file}),
        &roots
    )
    .unwrap_err()
    .contains("max_bytes"));
    let list = serde_json::json!({"operation":"list","max_entries":10});
    let first = execute(
        &list,
        &serde_json::json!({"path":root.path(),"limit":1}),
        &roots,
    )
    .unwrap();
    assert_eq!(first["entries"][0]["name"], "a.json");
    assert_eq!(first["next_after"], "a.json");
    let second = execute(
        &list,
        &serde_json::json!({"path":root.path(),"limit":1,"after":"a.json"}),
        &roots,
    )
    .unwrap();
    assert_eq!(second["entries"][0]["name"], "b.json");
    assert!(second["next_after"].is_null());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(execute(
            &config,
            &serde_json::json!({"path":root.path().join("escape/private.json")}),
            &roots
        )
        .unwrap_err()
        .contains("outside"));
    }
}
