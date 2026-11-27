#![cfg(all(target_os = "linux", feature = "ledger-wake"))]

use octopus_interfaces::ledger_wake::{HostedLedgerWake, LedgerWakeHost, LedgerWakeSource};
use std::{fs, time::Duration};
use tokio_util::sync::CancellationToken;

fn inotify_instances() -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .filter(|target| target.as_os_str() == "anon_inode:inotify")
        .count()
}

#[tokio::test]
async fn concurrent_lives_share_one_watch_and_recheck_after_subscribing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("movements.jsonl");
    fs::write(&path, b"").unwrap();
    let original = inotify_instances();
    assert!(LedgerWakeHost::new(None).request().is_err());
    let mut lives: Vec<_> = (0..200).map(|_| LedgerWakeHost::new(Some(&path))).collect();
    for life in &mut lives {
        life.request().unwrap();
        assert!(life.request().is_err());
        life.changed().await.unwrap();
    }
    assert_eq!(inotify_instances(), original + 1);

    let hosted = HostedLedgerWake::default();
    let source = LedgerWakeSource::Host(hosted.clone());
    let mut input = source.subscribe(&path).unwrap();
    let mut control = source.subscribe(&path).unwrap();
    let cancel = CancellationToken::new();
    fs::write(&path, b"row\n").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        let (a, b, ()) = tokio::join!(
            input.wait(&cancel, &path),
            control.wait(&cancel, &path),
            async {
                hosted.requested().await;
                for life in &mut lives {
                    life.request().unwrap();
                    life.changed().await.unwrap();
                }
                hosted.acknowledge().unwrap();
            }
        );
        a.unwrap();
        b.unwrap();
    })
    .await
    .unwrap();
    drop(lives);
    tokio::time::timeout(Duration::from_secs(5), async {
        while inotify_instances() != original {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the last subscriber releases the inotify watch");
}
