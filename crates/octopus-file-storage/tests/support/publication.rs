use super::*;
use std::time::Duration;

fn interrupted(path: &Path, operation: &str, before: &str, boundary: Interruption) {
    INTERRUPTION.with(|selected| selected.set(Some(boundary)));
    assert!(std::panic::catch_unwind(|| publish(path, operation, Some(before), b"after")).is_err());
}

#[test]
fn concurrent_predecessor_checks_and_read_guards_cover_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record");
    std::fs::write(&path, b"before").unwrap();
    let before = hash(b"before");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let jobs: Vec<_> = ["first", "second"].into_iter().map(|operation| {
        let (path, before, barrier) = (path.clone(), before.clone(), barrier.clone());
        std::thread::spawn(move || {
            barrier.wait();
            publish(&path, operation, Some(&before), operation.as_bytes()).unwrap()
        })
    }).collect();
    barrier.wait();
    let receipts: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
    assert_eq!(receipts.iter().filter(|receipt| receipt.outcome == PublicationOutcome::Applied).count(), 1);
    assert_eq!(receipts.iter().filter(|receipt| receipt.outcome == PublicationOutcome::NotApplied).count(), 1);
    let guard = ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).unwrap();
    assert_eq!(open_regular(&path).unwrap().try_lock_exclusive().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    guard.verify().unwrap();
    drop(guard);
    open_regular(&path).unwrap().try_lock_exclusive().unwrap();
    let absent = root.path().join("absent");
    let guard = ReadGuard::try_acquire(&[absent], &[]).unwrap();
    assert_eq!(directory_lock(root.path(), true, false).err().unwrap().kind(), io::ErrorKind::WouldBlock);
    drop(guard);
}

#[test]
fn read_guard_retains_missing_parent_absence_under_existing_namespace_ownership() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let interfaces = root.join("interfaces");
    std::fs::create_dir(&interfaces).unwrap();
    let parent = interfaces.join("new-door");
    let paths = [parent.join("tool-door-effect-frontier.jsonl"), parent.join("ledger-cursor.tool-door.json")];
    let guard = ReadGuard::try_acquire(&paths, &[]).unwrap();
    assert!(!parent.exists() && !interfaces.join(COORDINATION_DIRECTORY).exists());
    guard.verify().unwrap();
    assert_eq!(NamespaceGuard::acquire(&[(parent.clone(), true)], false).err().unwrap().kind(), io::ErrorKind::WouldBlock);
    #[cfg(unix)] {
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&interfaces, &alias).unwrap();
        assert!(ReadGuard::try_acquire(&[alias.join("new-door/file")], &[]).is_err());
    }
    std::fs::write(root.join("not-directory"), b"file").unwrap();
    assert!(ReadGuard::try_acquire(&[root.join("not-directory/child/file")], &[]).is_err());
    std::fs::create_dir(&parent).unwrap();
    assert!(guard.verify().is_err());
    drop(guard);
    let guard = ReadGuard::try_acquire(&paths, &[]).unwrap();
    std::fs::write(&paths[0], b"outside protocol").unwrap();
    assert!(guard.verify().is_err());
}

#[test]
fn file_access_projection_is_read_only_and_limits_metadata_to_one_target() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let path = root.join("selected");
    let access = file_publication_access(&path).unwrap();
    let target = Target::coordinates(path.clone());
    let sibling = Target::coordinates(root.join("sibling"));
    let allows = |selectors: &[PublicationFileSelector], path: &Path| selectors.iter().any(|selector| match selector {
        PublicationFileSelector::Exact(exact) => exact == path,
        PublicationFileSelector::Names { directory, prefix, suffix } => path.parent() == Some(directory.as_path())
            && path.file_name().is_some_and(|name| name.as_encoded_bytes().starts_with(prefix.as_encoded_bytes())
                && name.as_encoded_bytes().ends_with(suffix.as_encoded_bytes())),
    });
    assert_eq!(access.directory_creates, [target.root.clone()]);
    assert!(!target.root.exists() && !path.exists());
    for metadata in [target.lock_path(), target.pending(), target.install(), target.stage("op"), target.receipt("op"),
        target.pending().with_extension("123.0.tmp"), target.receipt("op").with_extension("123.1.tmp")]
    { assert!(allows(&access.file_reads, &metadata) && allows(&access.file_writes, &metadata), "{metadata:?}"); }
    let legacy = target.root.join(format!("{}{}.receipt", target.stage_prefix(), hash(b"op")));
    assert!(allows(&access.file_reads, &legacy) && !allows(&access.file_writes, &legacy));
    for metadata in [sibling.lock_path(), sibling.pending(), sibling.install(), sibling.stage("op"), sibling.receipt("op"),
        directory::marker(&sibling.path), reclaim::marker(&sibling.path), target.root.join("history/file-unknown.json"), path]
    { assert!(!allows(&access.file_reads, &metadata) && !allows(&access.file_writes, &metadata), "{metadata:?}"); }
    for endpoint in target.path.ancestors().filter(|path| path.file_name().is_some()) {
        assert!(allows(&access.file_reads, &directory::marker(endpoint)));
        assert!(!allows(&access.file_writes, &directory::marker(endpoint)));
    }
    assert!(file_publication_access(Path::new("relative")).is_err());
    assert!(file_publication_access(&target.root.join("file")).is_err());
}

#[test]
fn recovery_precedes_supersession_and_never_executes_another_pending_write() {
    for boundary in [Interruption::Prepared, Interruption::Linked, Interruption::Installed] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        std::fs::write(&path, b"before").unwrap();
        let before = hash(b"before");
        interrupted(&path, "original", &before, boundary);
        let target = Target::open(&path, false).unwrap();
        let pending = std::fs::read(target.pending()).unwrap();
        assert_eq!(read_receipt(&target.path, "original").unwrap(), None);
        assert_eq!(std::fs::read(target.pending()).unwrap(), pending);
        let original_guard = WriteGuard::acquire_operation(&path, false, "original").unwrap();
        assert!(original_guard.reconcile_operation("").is_err());
        let reconciled = original_guard.reconcile_operation("original").unwrap();
        if boundary == Interruption::Installed {
            assert_eq!(reconciled.unwrap().outcome, PublicationOutcome::Applied);
        } else {
            assert_eq!(reconciled, None);
            assert_eq!(std::fs::read(target.pending()).unwrap(), pending);
            assert_eq!(std::fs::read(&path).unwrap(), b"before");
            assert_eq!(target.install().exists(), boundary == Interruption::Linked);
        }
        drop(original_guard);
        let guard = ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).unwrap();
        let expected = if boundary == Interruption::Installed { b"after".as_slice() } else { b"before" };
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        drop(guard);
        assert!(!Target::open(&path, false).unwrap().install().exists());
        assert_eq!(publish(&path, "later", Some(&hash(expected)), b"later").unwrap().outcome, PublicationOutcome::Applied);
        let original = publish(&path, "original", Some(&before), b"after").unwrap();
        assert_eq!(original.outcome, if boundary == Interruption::Installed { PublicationOutcome::Applied } else { PublicationOutcome::NotApplied });
        assert_eq!(std::fs::read(&path).unwrap(), b"later");
        assert!(publish(&path, "original", Some(&before), b"other").is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record");
    std::fs::write(&path, b"before").unwrap();
    interrupted(&path, "foreign-link", &hash(b"before"), Interruption::Linked);
    let target = Target::open(&path, false).unwrap();
    std::fs::remove_file(target.install()).unwrap();
    std::fs::write(target.install(), b"foreign").unwrap();
    assert!(ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).is_err());
    assert_eq!(std::fs::read(target.install()).unwrap(), b"foreign");
}

#[test]
fn removal_recovery_proves_the_original_inode_and_never_replays_for_another_holder() {
    for boundary in [Interruption::Prepared, Interruption::Installed] {
        for resume in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("record");
            std::fs::write(&path, b"before").unwrap();
            let before = hash(b"before");
            let inode = FileIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap();
            INTERRUPTION.with(|selected| selected.set(Some(boundary)));
            assert!(std::panic::catch_unwind(|| remove(&path, "remove", &before)).is_err());
            let target = Target::open(&path, false).unwrap();
            if boundary == Interruption::Installed {
                assert!(!path.exists());
                assert_eq!(FileIdentity::of(&std::fs::metadata(target.stage("remove")).unwrap()).unwrap(), inode);
            }
            if !resume {
                drop(ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).unwrap());
            }
            let applied = resume || boundary == Interruption::Installed;
            let receipt = remove(&path, "remove", &before).unwrap();
            assert_eq!(receipt.outcome, if applied { PublicationOutcome::Applied } else { PublicationOutcome::NotApplied });
            assert_eq!((receipt.after_hash.as_ref(), receipt.bytes, receipt.digest_algorithm), (None, 0, DigestAlgorithm::Sha256));
            assert!(!target.pending().exists() && !target.stage("remove").exists());
            assert_eq!(publish(&path, "later", (!applied).then_some(before.as_str()), b"later").unwrap().outcome, PublicationOutcome::Applied);
            assert_eq!(remove(&path, "remove", &before).unwrap(), receipt);
            assert_eq!(std::fs::read(&path).unwrap(), b"later");
        }
    }
}

#[test]
fn removal_requires_membership_and_preserves_algorithm_and_completed_receipts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record");
    std::fs::write(&path, b"before").unwrap();
    let before = DigestAlgorithm::Blake3.digest(b"before");
    assert!(WriteGuard::acquire(&path, false).unwrap().remove("remove", &before, DigestAlgorithm::Blake3).is_err());
    assert_eq!(remove(&path, "conflict", &hash(b"wrong")).unwrap().outcome, PublicationOutcome::NotApplied);
    let guard = WriteGuard::acquire(&path, true).unwrap();
    let receipt = guard.remove("remove", &before, DigestAlgorithm::Blake3).unwrap();
    assert_eq!((receipt.outcome.clone(), receipt.digest_algorithm, receipt.before_hash.as_deref()), (PublicationOutcome::Applied, DigestAlgorithm::Blake3, Some(before.as_str())));
    drop(guard);
    publish(&path, "later", None, b"after").unwrap();
    interrupted(&path, "unknown", &hash(b"after"), Interruption::Installed);
    let target = Target::open(&path, false).unwrap();
    let predecessor = target.predecessor("unknown");
    assert_eq!(std::fs::read(&predecessor).unwrap(), b"after");
    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"after").unwrap();
    std::fs::rename(foreign, &path).unwrap();
    assert_eq!(publish(&path, "unknown", Some(&hash(b"after")), b"after").unwrap().outcome, PublicationOutcome::Uncertain);
    let guard = WriteGuard::acquire(&path, true).unwrap();
    assert_eq!(guard.reconcile_operation("remove").unwrap(), Some(receipt.clone()));
    assert!(guard.reconcile_operation("next").is_err());
    assert_eq!(guard.remove("remove", &before, DigestAlgorithm::Blake3).unwrap(), receipt);
    assert!(guard.remove("remove", &hash(b"before"), DigestAlgorithm::Sha256).is_err());
    assert!(guard.remove("next", &hash(b"after"), DigestAlgorithm::Sha256).is_err());
}

#[test]
fn removal_uncertainty_rejects_absence_and_equal_bytes_without_exact_inode_proof() {
    for mode in ["absent", "foreign_tombstone", "recreated", "foreign_predecessor"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        std::fs::write(&path, b"before").unwrap();
        let before = hash(b"before");
        let boundary = if mode == "absent" || mode == "foreign_predecessor" { Interruption::Prepared } else { Interruption::Installed };
        INTERRUPTION.with(|selected| selected.set(Some(boundary)));
        assert!(std::panic::catch_unwind(|| remove(&path, "remove", &before)).is_err());
        let target = Target::open(&path, false).unwrap();
        if mode == "absent" {
            std::fs::remove_file(&path).unwrap();
        } else {
            let foreign = root.path().join("foreign");
            std::fs::write(&foreign, b"before").unwrap();
            std::fs::rename(foreign, if mode == "foreign_tombstone" { target.stage("remove") } else { path.clone() }).unwrap();
        }
        let receipt = remove(&path, "remove", &before).unwrap();
        assert_eq!(receipt.outcome, PublicationOutcome::Uncertain);
        assert!(target.pending().exists());
        assert!(publish(&path, "next", (!matches!(mode, "absent" | "foreign_tombstone")).then_some(before.as_str()), b"next").is_err());
        assert!(ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).is_err());
    }
}

#[test]
fn original_prepared_publication_resumes_its_exact_stage_and_uncertainty_stays_closed() {
    for installed in [false, true] {
        let legacy = tempfile::tempdir().unwrap();
        let path = legacy.path().join("record");
        std::fs::write(&path, b"before").unwrap();
        interrupted(&path, "legacy", &hash(b"before"), if installed { Interruption::Installed } else { Interruption::Prepared });
        let target = Target::open(&path, false).unwrap();
        let mut pending: PendingPublication = read_record(&target.pending()).unwrap().unwrap();
        pending.schema_version = 1;
        std::fs::write(target.pending(), serde_json::to_vec(&pending).unwrap()).unwrap();
        std::fs::remove_file(target.predecessor("legacy")).unwrap();
        assert_eq!(publish(&path, "legacy", Some(&hash(b"before")), b"after").unwrap().outcome,
            if installed { PublicationOutcome::Applied } else { PublicationOutcome::Uncertain });
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record");
    std::fs::write(&path, b"before").unwrap();
    let before = hash(b"before");
    interrupted(&path, "original", &before, Interruption::Prepared);
    let target = Target::open(&path, false).unwrap();
    let inode = FileIdentity::of(&std::fs::metadata(target.stage("original")).unwrap()).unwrap();
    assert_eq!(publish(&path, "original", Some(&before), b"after").unwrap().outcome, PublicationOutcome::Applied);
    assert_eq!(FileIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap(), inode);
    assert!(!target.predecessor("original").exists());

    interrupted(&path, "unknown", &hash(b"after"), Interruption::Installed);
    let retained = target.predecessor("unknown");
    assert_eq!(FileIdentity::of(&std::fs::metadata(&retained).unwrap()).unwrap(), inode);
    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"after").unwrap();
    assert_ne!(FileIdentity::of(&std::fs::metadata(&foreign).unwrap()).unwrap(), inode);
    std::fs::rename(foreign, &path).unwrap();
    let uncertain = publish(&path, "unknown", Some(&hash(b"after")), b"after").unwrap();
    assert_eq!(uncertain.outcome, PublicationOutcome::Uncertain);
    assert_eq!(publish(&path, "original", Some(&before), b"after").unwrap().outcome, PublicationOutcome::Applied);
    assert!(publish(&path, "original", Some(&before), b"different request").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"after");
    assert!(publish(&path, "next", Some(&hash(b"after")), b"next").is_err());
    assert!(ReadGuard::try_acquire(&[path], &[]).is_err());
}

#[test]
fn retained_stage_import_preserves_identity_and_refuses_unknown_old_effects() {
    for mode in ["before", "installed", "unknown"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        let stage = root.path().join("old-stage");
        std::fs::write(&stage, b"after").unwrap();
        match mode {
            "installed" => std::fs::hard_link(&stage, &path).unwrap(),
            "before" => std::fs::write(&path, b"before").unwrap(),
            _ => std::fs::write(&path, b"after").unwrap(),
        }
        let guard = WriteGuard::acquire(&path, false).unwrap();
        let receipt = guard.publish_retained("retained", Some(&hash(b"before")), b"after", DigestAlgorithm::Sha256, &stage).unwrap();
        if mode == "unknown" {
            assert_eq!(receipt.outcome, PublicationOutcome::Uncertain);
        } else {
            assert_eq!(receipt.outcome, PublicationOutcome::Applied);
            assert_eq!(FileIdentity::of(&std::fs::metadata(&path).unwrap()).unwrap(), FileIdentity::of(&std::fs::metadata(&stage).unwrap()).unwrap());
        }
    }
}

#[test]
fn reading_unpublished_files_creates_no_coordination_files_and_detects_foreign_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record");
    std::fs::write(&path, b"before").unwrap();
    let guard = ReadGuard::try_acquire(std::slice::from_ref(&path), &[]).unwrap();
    assert!(!root.path().join(COORDINATION_DIRECTORY).exists());
    std::fs::write(path, b"changed outside the protocol").unwrap();
    assert!(guard.verify().is_err());
}

fn directory_move(source: &Path, destination: &Path) -> DirectoryMove {
    DirectoryMove { source: source.into(), destination: destination.into(), expected: snapshot_directory(source, DigestAlgorithm::Sha256).unwrap() }
}

#[test]
fn directory_publication_waits_for_child_writer_and_admission_ownership() {
    for writer in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let source = root.join("source");
        std::fs::create_dir_all(source.join("child")).unwrap();
        let file = source.join("child/file");
        std::fs::write(&file, b"value").unwrap();
        let movement = directory_move(&source, &root.join("destination"));
        let write = writer.then(|| WriteGuard::acquire(&file, false).unwrap());
        let read = (!writer).then(|| ReadGuard::try_acquire(&[file], &[]).unwrap());
        assert_eq!(NamespaceGuard::acquire(&[(source.clone(), true)], false).err().unwrap().kind(), io::ErrorKind::WouldBlock);
        let (send, receive) = std::sync::mpsc::channel();
        let job = std::thread::spawn(move || send.send(publish_directory_moves("move", &[movement])).unwrap());
        assert!(receive.recv_timeout(Duration::from_millis(30)).is_err());
        assert!(source.exists());
        drop(write);
        drop(read);
        assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap().unwrap().receipts[0].outcome, PublicationOutcome::Applied);
        job.join().unwrap();
    }
}

#[test]
fn directory_recovery_keeps_original_inode_and_reports_a_partial_repair() {
    for boundary in [Interruption::DirectoryPrepared(0), Interruption::DirectoryInstalled(0)] {
        for resume in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(temporary.path()).unwrap();
            let source = root.join("source");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("file"), b"value").unwrap();
            let movement = directory_move(&source, &root.join("destination"));
            INTERRUPTION.with(|selected| selected.set(Some(boundary)));
            assert!(std::panic::catch_unwind(|| publish_directory_moves("move", std::slice::from_ref(&movement))).is_err());
            if !resume { drop(ReadGuard::try_acquire(&[], std::slice::from_ref(&source)).unwrap()); }
            let result = publish_directory_moves("move", std::slice::from_ref(&movement)).unwrap();
            let applied = resume || boundary == Interruption::DirectoryInstalled(0);
            assert_eq!(result.receipts[0].outcome, if applied { PublicationOutcome::Applied } else { PublicationOutcome::NotApplied });
            let retained = if applied { &movement.destination } else { &source };
            assert_eq!(FileIdentity::of(&std::fs::metadata(retained).unwrap()).unwrap(), movement.expected.identity);
            assert_eq!(publish_directory_moves("move", &[movement]).unwrap(), result);
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    for name in ["live", "stage"] { std::fs::create_dir(root.join(name)).unwrap(); }
    let moves = [directory_move(&root.join("live"), &root.join("backup")), directory_move(&root.join("stage"), &root.join("live"))];
    INTERRUPTION.with(|selected| selected.set(Some(Interruption::DirectoryPrepared(1))));
    assert!(std::panic::catch_unwind(|| publish_directory_moves("repair", &moves)).is_err());
    drop(ReadGuard::try_acquire(&[], &[root.join("live")]).unwrap());
    let result = publish_directory_moves("repair", &moves).unwrap();
    assert_eq!(result.receipts.iter().map(|receipt| receipt.outcome.clone()).collect::<Vec<_>>(), [PublicationOutcome::Applied, PublicationOutcome::NotApplied]);
    assert!(result.error.is_some() && !root.join("live").exists() && root.join("stage").exists() && root.join("backup").exists());
}

#[test]
fn directory_replacement_supports_file_directory_transitions_with_exact_manifests() {
    for live_directory in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let live = root.join("live");
        let stage = root.join("stage");
        for path in [&live, &stage] { std::fs::create_dir(path).unwrap(); }
        let (directory, file) = if live_directory { (&live, &stage) } else { (&stage, &live) };
        std::fs::create_dir(directory.join("assets")).unwrap();
        std::fs::write(directory.join("assets/item.json"), b"nested").unwrap();
        std::fs::write(file.join("assets"), b"flat").unwrap();
        let moves = [directory_move(&live, &root.join("backup")), directory_move(&stage, &live)];
        let result = publish_directory_moves("replace", &moves).unwrap();
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(result.receipts.iter().map(|receipt| receipt.outcome.clone()).collect::<Vec<_>>(), [PublicationOutcome::Applied, PublicationOutcome::Applied]);
        assert_eq!(snapshot_directory(&live, DigestAlgorithm::Sha256).unwrap(), moves[1].expected);
        assert_eq!(snapshot_directory(&moves[0].destination, DigestAlgorithm::Sha256).unwrap(), moves[0].expected);
        let mut wrong = moves[0].expected.clone();
        wrong.identity = moves[1].expected.identity.clone();
        let refused = publish_directory_moves("wrong-manifest", &[DirectoryMove { source: live.clone(), destination: root.join("refused"), expected: wrong }]).unwrap();
        assert_eq!(refused.receipts[0].outcome, PublicationOutcome::NotApplied);
        assert!(refused.error.is_some() && !root.join("refused").exists());
        assert_eq!(snapshot_directory(&live, DigestAlgorithm::Sha256).unwrap(), moves[1].expected);
    }
}

#[test]
fn foreign_equal_directory_fences_both_endpoints_and_descendants() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let source = root.join("source");
    let destination = root.join("destination");
    let initial = root.join("initial");
    std::fs::create_dir(&initial).unwrap();
    std::fs::write(initial.join("file"), b"value").unwrap();
    let earlier = directory_move(&initial, &source);
    let completed = publish_directory_moves("earlier", std::slice::from_ref(&earlier)).unwrap();
    let movement = directory_move(&source, &destination);
    INTERRUPTION.with(|selected| selected.set(Some(Interruption::DirectoryInstalled(0))));
    assert!(std::panic::catch_unwind(|| publish_directory_moves("move", std::slice::from_ref(&movement))).is_err());
    std::fs::rename(&destination, root.join("original-inode")).unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("file"), b"value").unwrap();
    assert_eq!(publish_directory_moves("move", &[movement]).unwrap().receipts[0].outcome, PublicationOutcome::Uncertain);
    assert_eq!(publish_directory_moves("earlier", &[earlier]).unwrap(), completed);
    for endpoint in [&source, &destination] {
        assert!(ReadGuard::try_acquire(&[], &[endpoint.clone()]).is_err());
        assert!(publish(&endpoint.join("new/file"), "new", None, b"new").is_err());
        assert!(create_dir_all(&endpoint.join("new")).is_err());
        assert!(!endpoint.join("new").exists());
    }
}

#[test]
fn prepared_directory_with_exact_source_refuses_a_replaced_parent() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let parent = root.join("parent");
    let source = parent.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let movement = directory_move(&source, &parent.join("destination"));
    INTERRUPTION.with(|selected| selected.set(Some(Interruption::DirectoryPrepared(0))));
    assert!(std::panic::catch_unwind(|| publish_directory_moves("move", std::slice::from_ref(&movement))).is_err());
    let old_parent = root.join("old-parent");
    std::fs::rename(&parent, &old_parent).unwrap();
    std::fs::create_dir(&parent).unwrap();
    std::fs::rename(old_parent.join("source"), &source).unwrap();
    std::fs::rename(old_parent.join(COORDINATION_DIRECTORY), parent.join(COORDINATION_DIRECTORY)).unwrap();
    assert_eq!(FileIdentity::of(&std::fs::metadata(&source).unwrap()).unwrap(), movement.expected.identity);
    assert_eq!(publish_directory_moves("move", &[movement]).unwrap().receipts[0].outcome, PublicationOutcome::Uncertain);
    assert!(source.exists() && !parent.join("destination").exists());
}

#[test]
fn original_file_and_directory_history_survives_reclaim_and_later_ancestor_removal() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let outer = root.join("outer");
    let source = outer.join("tree");
    let file = source.join("child/file");
    let original = WriteGuard::acquire_creating_operation(&file, "native-original").unwrap()
        .publish("native-original", None, b"value", DigestAlgorithm::Blake3).unwrap();
    publish(&source.join("child/sibling"), "sibling-original", None, b"sibling").unwrap();
    let movement = directory_move(&source, &outer.join("dead"));
    let result = publish_directory_moves("remove-tree", std::slice::from_ref(&movement)).unwrap();
    reclaim_directory(&movement.destination, &result.receipts[0], &movement.expected).unwrap();
    assert!(WriteGuard::acquire(&file, true).is_err());
    assert_eq!(WriteGuard::acquire_creating_operation(&file, "native-original").unwrap()
        .publish("native-original", None, b"value", DigestAlgorithm::Blake3).unwrap(), original);
    assert!(!source.exists());
    let ancestor = directory_move(&outer, &root.join("outer-dead"));
    let parent_result = publish_directory_moves("remove-outer", std::slice::from_ref(&ancestor)).unwrap();
    assert_eq!(parent_result.receipts[0].outcome, PublicationOutcome::Applied, "{parent_result:?}");
    reclaim_directory(&ancestor.destination, &parent_result.receipts[0], &ancestor.expected).unwrap();
    assert_eq!(read_receipt(&file, "native-original").unwrap(), Some(original.clone()));
    assert_eq!(read_receipt(&file, "unknown").unwrap(), None);
    assert!(!outer.exists());
    assert!(read_receipt(Path::new("relative/file"), "native-original").is_err());
    assert!(read_receipt(&root.join("../file"), "native-original").is_err());
    assert!(read_receipt(&root.join(COORDINATION_DIRECTORY).join("file"), "native-original").is_err());
    assert_eq!(publish_directory_moves("remove-tree", std::slice::from_ref(&movement)).unwrap(), result);
    reclaim_directory(&movement.destination, &result.receipts[0], &movement.expected).unwrap();
    assert_eq!(WriteGuard::acquire_operation(&file, true, "native-original").unwrap()
        .publish("native-original", None, b"value", DigestAlgorithm::Blake3).unwrap(), original);
    assert!(!outer.exists());
    std::fs::write(&outer, b"foreign parent").unwrap();
    let access = file_publication_access(&file).unwrap();
    assert!(access.file_writes.is_empty() && access.directory_creates.is_empty());
    let retained: Vec<_> = access.file_reads.iter().filter_map(|selector| match selector {
        PublicationFileSelector::Exact(path) if path.parent().and_then(Path::file_name).is_some_and(|name| name == "history") => Some(path),
        _ => None,
    }).collect();
    assert_eq!(retained.len(), 1);
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(retained[0]).unwrap()).unwrap();
    assert_eq!(record["original"]["receipt"]["path"], serde_json::to_value(&file).unwrap());
    assert_eq!(read_receipt(&file, "native-original").unwrap(), Some(original.clone()));
    assert_eq!(read_receipt(&file, "unknown").unwrap(), None);
    for creating in [false, true] {
        let guard = if creating { WriteGuard::acquire_creating_operation(&file, "native-original") }
            else { WriteGuard::acquire_operation(&file, true, "native-original") }.unwrap();
        assert_eq!(guard.publish("native-original", None, b"value", DigestAlgorithm::Blake3).unwrap(), original);
        assert!(guard.publish("new-operation", None, b"value", DigestAlgorithm::Blake3).is_err());
    }
    assert_eq!(std::fs::read(&outer).unwrap(), b"foreign parent");
    std::fs::remove_file(&outer).unwrap();
    #[cfg(unix)] {
        let foreign = root.join("foreign-history");
        std::fs::create_dir_all(foreign.join("tree/child")).unwrap();
        std::fs::write(foreign.join("tree/child/file"), b"foreign file").unwrap();
        std::fs::write(foreign.join(COORDINATION_DIRECTORY), b"foreign metadata").unwrap();
        std::os::unix::fs::symlink(&foreign, &outer).unwrap();
        let aliased_access = file_publication_access(&file).unwrap();
        assert_eq!(aliased_access, access);
        assert!(!aliased_access.directory_reads.iter().any(|path| path.starts_with(&foreign) || path.starts_with(&outer)));
        assert_eq!(read_receipt(&file, "native-original").unwrap(), Some(original.clone()));
        assert_eq!(read_receipt(&file, "unknown").unwrap(), None);
        assert_eq!(WriteGuard::acquire_creating_operation(&file, "native-original").unwrap()
            .publish("native-original", None, b"value", DigestAlgorithm::Blake3).unwrap(), original);
        assert_eq!(std::fs::read(foreign.join("tree/child/file")).unwrap(), b"foreign file");
        assert_eq!(std::fs::read(foreign.join(COORDINATION_DIRECTORY)).unwrap(), b"foreign metadata");
        assert_eq!(std::fs::read_link(&outer).unwrap(), foreign);
        std::fs::remove_file(&outer).unwrap();
    }
    publish(&file, "successor", None, b"successor").unwrap();
    assert_eq!(WriteGuard::acquire_operation(&file, true, "native-original").unwrap()
        .publish("native-original", None, b"value", DigestAlgorithm::Blake3).unwrap(), original);
    assert_eq!(std::fs::read(file).unwrap(), b"successor");
}

#[test]
fn tombstone_reclaim_resumes_its_exact_frontier_and_preserves_foreign_successors() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let source = root.join("source");
    std::fs::create_dir_all(source.join("child")).unwrap();
    std::fs::write(source.join("child/file"), b"value").unwrap();
    let movement = directory_move(&source, &root.join("dead"));
    let result = publish_directory_moves("remove", std::slice::from_ref(&movement)).unwrap();
    let proof = &result.receipts[0];
    let alias = root.join("alias");
    std::fs::hard_link(movement.destination.join("child/file"), &alias).unwrap();
    let reader = ReadGuard::try_acquire(std::slice::from_ref(&alias), &[]).unwrap();
    assert_eq!(namespace::TreeFiles::acquire(&[(movement.destination.join("child/file"), movement.expected.entries.iter().find(|entry| entry.kind == DirectoryEntryKind::File).unwrap().identity.clone())], false).err().unwrap().kind(), io::ErrorKind::WouldBlock);
    drop(reader);
    INTERRUPTION.with(|selected| selected.set(Some(Interruption::Reclaim(0))));
    assert!(std::panic::catch_unwind(|| reclaim_directory(&movement.destination, proof, &movement.expected)).is_err());
    assert!(ReadGuard::try_acquire(&[], std::slice::from_ref(&movement.destination)).is_err());
    assert!(publish(&movement.destination.join("new"), "new", None, b"new").is_err());
    reclaim_directory(&movement.destination, proof, &movement.expected).unwrap();
    assert!(!movement.destination.exists());
    std::fs::create_dir_all(movement.destination.join("child")).unwrap();
    std::fs::write(movement.destination.join("child/file"), b"foreign").unwrap();
    reclaim_directory(&movement.destination, proof, &movement.expected).unwrap();
    assert_eq!(std::fs::read(movement.destination.join("child/file")).unwrap(), b"foreign");
    assert_eq!(std::fs::read(alias).unwrap(), b"value");
}

#[test]
fn moved_file_history_does_not_claim_or_block_a_new_path_operation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let source = root.join("source");
    let original_file = source.join("file");
    let original = publish(&original_file, "same-id", None, b"before").unwrap();
    let target = Target::open(&original_file, false).unwrap();
    std::fs::rename(target.receipt("same-id"), target.root.join(format!("{}-{}.receipt", target.key, hash(b"same-id")))).unwrap();
    let movement = directory_move(&source, &root.join("destination"));
    publish_directory_moves("move", std::slice::from_ref(&movement)).unwrap();
    let current_file = movement.destination.join("file");
    assert_eq!(publish(&current_file, "same-id", Some(&hash(b"before")), b"after").unwrap().outcome, PublicationOutcome::Applied);
    assert_eq!(publish(&original_file, "same-id", None, b"before").unwrap(), original);
    assert_eq!(std::fs::read(current_file).unwrap(), b"after");
    assert!(!source.exists());
}

#[cfg(unix)]
#[test]
fn directory_endpoints_reject_aliased_ancestors_without_rebasing_selected_effects() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let selected = root.join("selected");
    let outside = root.join("outside");
    std::fs::create_dir_all(selected.join("source")).unwrap();
    std::fs::create_dir_all(outside.join("foreign")).unwrap();
    std::fs::write(selected.join("source/file"), b"selected").unwrap();
    std::fs::write(outside.join("foreign/file"), b"outside").unwrap();
    let movement = directory_move(&selected.join("source"), &selected.join("dead"));
    let proof = publish_directory_moves("move", std::slice::from_ref(&movement)).unwrap().receipts.remove(0);
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    assert!(snapshot_directory(&alias.join("foreign"), DigestAlgorithm::Sha256).is_err());
    let mut aliased = directory_move(&outside.join("foreign"), &root.join("destination"));
    aliased.source = alias.join("foreign");
    assert!(publish_directory_moves("alias-source", &[aliased]).is_err());
    let destination = DirectoryMove { source: outside.join("foreign"), destination: alias.join("new"), expected: snapshot_directory(&outside.join("foreign"), DigestAlgorithm::Sha256).unwrap() };
    assert!(publish_directory_moves("alias-destination", &[destination]).is_err());
    let original_parent = FileIdentity::of(&std::fs::metadata(&selected).unwrap()).unwrap();
    std::fs::rename(&selected, root.join("held-selection")).unwrap();
    std::os::unix::fs::symlink(&outside, &selected).unwrap();
    assert!(reclaim_directory(&movement.destination, &proof, &movement.expected).is_err());
    assert!(native::pinned_directory(&selected, &original_parent).is_err());
    assert!(root.join("held-selection/dead/file").exists());
    assert_eq!(std::fs::read(outside.join("foreign/file")).unwrap(), b"outside");
    assert!(!outside.join("new").exists() && !root.join("destination").exists());
}
