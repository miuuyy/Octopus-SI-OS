use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn api_key_auth(key: &str) -> AuthDotJson {
    serde_json::from_value(serde_json::json!({ "OPENAI_API_KEY": key }))
        .expect("api-key auth.json must parse")
}

#[cfg(unix)]
#[test]
fn writers_through_a_linked_home_serialize_on_the_store_auth_lock() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    let curated = root.path().join("curated");
    std::fs::create_dir_all(&store).expect("store dir");
    std::fs::create_dir_all(&curated).expect("curated dir");
    for name in [AUTH_FILE_NAME, AUTH_LOCK_FILE_NAME] {
        std::os::unix::fs::symlink(store.join(name), curated.join(name)).expect("auth link");
    }
    FileAuthStorage::new(store.clone())
        .save(&api_key_auth("before"))
        .expect("seed store");

    let other_writer = open_lock_file(&store).expect("store lock file");
    other_writer.lock().expect("other writer holds auth.lock");

    let (done_tx, done_rx) = mpsc::channel();
    let writer = std::thread::spawn({
        let curated = curated.clone();
        move || {
            let storage = FileAuthStorage::new(curated);
            let _lock = storage.lock_for_write().expect("lock_for_write");
            storage.save(&api_key_auth("after")).expect("save");
            done_tx.send(()).expect("signal");
        }
    });

    assert!(
        done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "a write through the linked home must wait for the store's auth.lock holder"
    );
    assert_eq!(
        FileAuthStorage::new(store.clone()).load().expect("load"),
        Some(api_key_auth("before"))
    );

    drop(other_writer);
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the write proceeds once auth.lock is released");
    writer.join().expect("writer thread");
    assert!(
        std::fs::symlink_metadata(curated.join(AUTH_FILE_NAME))
            .expect("curated auth link")
            .file_type()
            .is_symlink(),
        "a write must follow the curated link, never replace it"
    );
    assert_eq!(
        FileAuthStorage::new(store).load().expect("load"),
        Some(api_key_auth("after"))
    );
}
