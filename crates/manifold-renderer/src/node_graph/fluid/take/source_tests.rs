use super::*;

use std::sync::atomic::{AtomicU64, Ordering};

struct Directory(Arc<PathBuf>);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "manifold-physics-take-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(Arc::new(path))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(self.0.as_ref()).unwrap();
    }
}

fn identity(value: u8) -> Hash {
    [value; 32]
}

fn request(source_identity: Option<Hash>) -> Request {
    let mut request = super::tests::request();
    request.source_identity = source_identity;
    request
}

#[test]
fn source_identity_validates_origin_only_takes() {
    let directory = Directory::new();
    let source = identity(1);
    let mut input = request(Some(source));
    input.count = 0;
    Writer::create(Arc::clone(&directory.0), &input).unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert!(replay.validate_source_identity(source).is_ok());
    assert!(replay.validate_source_identity(identity(2)).is_err());

    let directory = Directory::new();
    let mut input = request(None);
    input.count = 0;
    Writer::create(Arc::clone(&directory.0), &input).unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert!(
        replay
            .validate_source_identity(identity(1))
            .unwrap_err()
            .contains("missing")
    );
}

#[test]
fn source_identity_tracks_the_last_committed_batch() {
    let directory = Directory::new();
    let first = identity(3);
    let second = identity(4);
    let mut input = request(Some(first));
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer.append(&input, 6, 6, None).unwrap();
    input.start_tick = 6;
    input.source_identity = Some(second);
    writer.append(&input, 6, 12, None).unwrap();

    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert!(replay.validate_source_identity(second).is_ok());
    assert!(
        replay
            .validate_source_identity(first)
            .unwrap_err()
            .contains("changed")
    );
}

#[test]
fn source_identity_is_snapshotted_at_replay_open() {
    let directory = Directory::new();
    let first = identity(5);
    let second = identity(6);
    let mut input = request(Some(first));
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer.append(&input, 6, 6, None).unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();

    input.start_tick = 6;
    input.source_identity = Some(second);
    writer.append(&input, 6, 12, None).unwrap();

    assert!(replay.validate_source_identity(first).is_ok());
    assert!(replay.validate_source_identity(second).is_err());
}

#[test]
fn source_identity_presence_cannot_change_during_recording() {
    let directory = Directory::new();
    let source = identity(7);
    let mut input = request(Some(source));
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    input.source_identity = None;
    assert!(
        writer
            .append(&input, 6, 6, None)
            .unwrap_err()
            .contains("presence")
    );

    let directory = Directory::new();
    let mut input = request(None);
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    input.source_identity = Some(source);
    assert!(
        writer
            .append(&input, 6, 6, None)
            .unwrap_err()
            .contains("presence")
    );
}

#[test]
fn source_identity_change_commits_a_zero_tick_handoff() {
    let directory = Directory::new();
    let first = identity(10);
    let second = identity(11);
    let mut input = request(Some(first));
    input.count = 0;
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    input.source_identity = Some(second);
    writer.append(&input, 0, 0, None).unwrap();

    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert!(replay.validate_source_identity(second).is_ok());
    let (progress, _): (Progress, _) = read_record(&directory.0.join(PROGRESS)).unwrap();
    assert_eq!(progress.records, 1);
}

#[test]
fn source_identity_rejects_malformed_batch_provenance() {
    let directory = Directory::new();
    let input = request(Some(identity(8)));
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer.append(&input, 6, 6, None).unwrap();

    let path = batch_path(&directory.0, 0);
    let (mut batch, _): (Batch, _) = read_record(&path).unwrap();
    batch.source_identity = None;
    fs::remove_file(&path).unwrap();
    let hash = write_new(&path, &batch).unwrap();
    let progress_path = directory.0.join(PROGRESS);
    let (mut progress, _): (Progress, _) = read_record(&progress_path).unwrap();
    progress.last_batch_hash = hash;
    publish_progress(&directory.0, &progress).unwrap();

    assert!(
        FluidTakeReplay::open(directory.0.as_ref())
            .err()
            .unwrap()
            .contains("source identity presence")
    );
}

#[test]
fn source_identity_decodes_legacy_records_without_provenance() {
    for version in [LEGACY_VERSION, TIMED_LEGACY_VERSION] {
        let directory = Directory::new();
        let input = request(None);
        let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
        writer.append(&input, 6, 6, None).unwrap();

        let header_path = directory.0.join(HEADER);
        let (mut header, _): (Header, _) = read_record(&header_path).unwrap();
        header.version = version;
        let mut header_record = serde_json::to_value(&header).unwrap();
        header_record
            .as_object_mut()
            .unwrap()
            .remove("sourceIdentity");
        fs::remove_file(&header_path).unwrap();
        let header_hash = write_new(&header_path, &header_record).unwrap();

        let batch_path = batch_path(&directory.0, 0);
        let (mut batch, _): (Batch, _) = read_record(&batch_path).unwrap();
        batch.header_hash = header_hash;
        batch.previous_hash = header_hash;
        let mut batch_record = serde_json::to_value(&batch).unwrap();
        batch_record
            .as_object_mut()
            .unwrap()
            .remove("sourceIdentity");
        fs::remove_file(&batch_path).unwrap();
        let batch_hash = write_new(&batch_path, &batch_record).unwrap();

        let progress_path = directory.0.join(PROGRESS);
        let (mut progress, _): (Progress, _) = read_record(&progress_path).unwrap();
        progress.header_hash = header_hash;
        progress.last_batch_hash = batch_hash;
        publish_progress(&directory.0, &progress).unwrap();

        let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
        assert!(
            replay
                .validate_source_identity(identity(9))
                .unwrap_err()
                .contains("missing")
        );
    }
}
