use super::*;

#[test]
fn writer_candidates_refresh_exact_generations_on_directory_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("calls");
    let mut index = WriterCallIndex::default();
    assert!(index.candidates(&root, 4).unwrap().is_empty());
    std::fs::create_dir(&root).unwrap();
    let previous = root.join("previous.artifact.json");
    let current = root.join("current.artifact.json");
    let ignored = root.join("unrelated.artifact.json");
    for path in [&previous, &current, &ignored] {
        std::fs::write(path, b"{}").unwrap();
    }
    assert_eq!(index.candidates(&root, 4).unwrap().len(), 3);
    index.record(previous.clone(), Some(3));
    index.record(current.clone(), Some(4));
    index.record(ignored, None);
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![current.clone()]);
    assert_eq!(index.candidates(&root, 3).unwrap(), vec![previous]);
    let fingerprint = index.directories[&root].fingerprint.clone();
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![current.clone()]);
    assert_eq!(index.directories[&root].fingerprint, fingerprint);
    std::fs::remove_file(&current).unwrap();
    let fresh = root.join("fresh.artifact.json");
    std::fs::write(&fresh, b"{}").unwrap();
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![fresh.clone()]);
    assert!(!index.generations.contains_key(&current));
    index.record(fresh.clone(), Some(4));
    assert_eq!(index.candidates(&root, 4).unwrap(), vec![fresh]);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(index.candidates(&root, 4).unwrap().is_empty());
    std::fs::write(&root, b"file").unwrap();
    assert!(index.candidates(&root, 4).is_err());
}

#[test]
fn writer_candidates_are_the_same_for_any_order_and_repetition_of_what_was_recorded() {
    use crate::property_check::check;
    use proptest::prelude::*;
    check(64, prop::collection::btree_map(0u8..12, 0u8..5, 1..10), |files| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("calls");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("unrelated.txt"), b"{}").unwrap();
        let path = |name: &u8| root.join(format!("call-{name}.artifact.json"));
        for name in files.keys() {
            std::fs::write(path(name), b"{}").unwrap();
        }
        let recorded = files.iter().filter(|(_, state)| **state != 0)
            .map(|(name, state)| (path(name), state.checked_sub(2).map(u64::from))).collect::<Vec<_>>();
        let mut first = WriterCallIndex::default();
        prop_assert_eq!(first.candidates(&root, 0).unwrap().len(), files.len());
        for (path, generation) in &recorded {
            first.record(path.clone(), *generation);
        }
        let mut second = WriterCallIndex::default();
        for (path, generation) in recorded.iter().rev().chain(recorded.iter().rev()) {
            second.record(path.clone(), *generation);
        }
        for generation in 0..3u64 {
            let expected = files.iter().filter(|(_, state)| **state == 0 || u64::from(**state) == generation + 2)
                .map(|(name, _)| path(name)).collect::<BTreeSet<_>>();
            let candidates = first.candidates(&root, generation).unwrap();
            prop_assert_eq!(&candidates, &second.candidates(&root, generation).unwrap());
            prop_assert_eq!(candidates.len(), expected.len());
            prop_assert_eq!(candidates.iter().cloned().collect::<BTreeSet<_>>(), expected);
            let fingerprint = first.directories[&root].fingerprint.clone();
            prop_assert!(fingerprint.is_some());
            prop_assert_eq!(&candidates, &first.candidates(&root, generation).unwrap());
            prop_assert_eq!(&fingerprint, &first.directories[&root].fingerprint);
            prop_assert_eq!(&fingerprint, &second.directories[&root].fingerprint);
        }
        Ok(())
    });
}
