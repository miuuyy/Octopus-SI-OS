use super::*;
use std::ffi::OsString;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicationFileSelector {
    Exact(PathBuf),
    Names { directory: PathBuf, prefix: OsString, suffix: OsString },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePublicationAccess {
    pub directory_reads: Vec<PathBuf>,
    pub directory_creates: Vec<PathBuf>,
    pub file_reads: Vec<PublicationFileSelector>,
    pub file_writes: Vec<PublicationFileSelector>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryPublicationAccess {
    pub directory_reads: Vec<PathBuf>,
    pub directory_tree_reads: Vec<PathBuf>,
    pub directory_creates: Vec<PathBuf>,
    pub file_reads: Vec<PublicationFileSelector>,
    pub file_writes: Vec<PublicationFileSelector>,
}

pub fn publication_ancestor_directories(path: &Path) -> io::Result<Vec<PathBuf>> {
    namespace::validate_absolute(path)?;
    let mut directories = BTreeSet::new();
    for parent in path.parent().into_iter().flat_map(Path::ancestors) {
        directories.insert(parent.to_path_buf());
        directories.insert(parent.join(COORDINATION_DIRECTORY));
        directories.insert(parent.join(COORDINATION_DIRECTORY).join("history"));
    }
    Ok(directories.into_iter().collect())
}

pub fn file_publication_access(path: &Path) -> io::Result<FilePublicationAccess> {
    namespace::validate_absolute(path)?;
    if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
        return Err(invalid("publication access requires an ordinary absolute file path"));
    }
    let history = history::file_access(path)?;
    let mut directories: BTreeSet<_> = history.ancestors.into_iter().collect();
    if history.writable_parent {
        directories.extend(path.parent().unwrap().ancestors().map(Path::to_path_buf));
    }
    let mut directory_reads = directories.clone();
    let mut file_reads: BTreeSet<_> = history.files.into_iter().map(PublicationFileSelector::Exact).collect();
    for endpoint in path.ancestors().filter(|endpoint| endpoint.file_name().is_some()) {
        if directories.contains(endpoint.parent().unwrap()) {
            file_reads.insert(PublicationFileSelector::Exact(directory::marker(endpoint)));
            file_reads.insert(PublicationFileSelector::Exact(reclaim::marker(endpoint)));
        }
    }
    for ancestor in &directories { directory_reads.insert(ancestor.join(COORDINATION_DIRECTORY)); }
    directory_reads.extend(history.directories);
    let mut access = FilePublicationAccess { directory_reads: directory_reads.into_iter().collect(),
        file_reads: Vec::new(), ..FilePublicationAccess::default() };
    if history.writable_parent {
        let target = Target::coordinates(path.to_path_buf());
        let names = |prefix: OsString, suffix: &str| PublicationFileSelector::Names {
            directory: target.root.clone(), prefix, suffix: suffix.into(),
        };
        let writes = [
            PublicationFileSelector::Exact(target.lock_path()),
            PublicationFileSelector::Exact(target.pending()),
            PublicationFileSelector::Exact(target.install()),
            names(target.stage_prefix().into(), ".bytes"),
            names(target.stage_prefix().into(), ".preimage"),
            names(target.receipt_prefix().into(), ".receipt"),
            names(record_temporary_prefix(&target.pending()), ".tmp"),
            names(target.receipt_prefix().into(), ".tmp"),
        ];
        file_reads.extend(writes.iter().cloned());
        file_reads.insert(names(target.stage_prefix().into(), ".receipt"));
        access.directory_creates.push(target.root);
        access.file_writes = BTreeSet::from(writes).into_iter().collect();
    }
    access.file_reads = file_reads.into_iter().collect();
    Ok(access)
}

pub fn directory_publication_access(path: &Path) -> io::Result<DirectoryPublicationAccess> {
    namespace::validate_absolute(path)?;
    if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
        return Err(invalid("directory publication access requires an ordinary absolute directory path"));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => (),
        Ok(_) => return Err(invalid("directory publication access requires a directory or absent endpoint")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let history = history::directory_access(path)?;
    let mut directories: BTreeSet<_> = history.ancestors.into_iter().collect();
    if history.writable_parent {
        directories.extend(path.parent().unwrap().ancestors().map(Path::to_path_buf));
        directories.insert(path.to_path_buf());
    }
    let mut directory_reads = directories.clone();
    let mut file_reads: BTreeSet<_> = history.files.into_iter().map(PublicationFileSelector::Exact).collect();
    for endpoint in path.ancestors().filter(|endpoint| endpoint.file_name().is_some()) {
        if directories.contains(endpoint.parent().unwrap()) {
            file_reads.insert(PublicationFileSelector::Exact(directory::marker(endpoint)));
            file_reads.insert(PublicationFileSelector::Exact(reclaim::marker(endpoint)));
        }
    }
    for ancestor in &directories {
        let coord = ancestor.join(COORDINATION_DIRECTORY);
        directory_reads.insert(coord.join("history"));
        directory_reads.insert(coord);
    }
    directory_reads.extend(history.directories);
    let mut access = DirectoryPublicationAccess {
        directory_reads: directory_reads.into_iter().collect(),
        ..DirectoryPublicationAccess::default()
    };
    if history.writable_parent {
        let parent_coord = path.parent().unwrap().join(COORDINATION_DIRECTORY);
        let parent_history = parent_coord.join("history");
        let marker_path = directory::marker(path);
        let names = |directory: &Path, prefix: OsString, suffix: &str| PublicationFileSelector::Names {
            directory: directory.to_path_buf(),
            prefix,
            suffix: suffix.into(),
        };
        let writes = [
            PublicationFileSelector::Exact(marker_path.clone()),
            names(&parent_coord, record_temporary_prefix(&marker_path), ".tmp"),
            names(&parent_coord, "dir-".into(), ".lock"),
            names(&parent_coord, "dir-".into(), ".plan"),
            names(&parent_coord, "dir-".into(), ".prepared"),
            names(&parent_coord, "dir-".into(), ".tmp"),
            names(&parent_history, "directory-".into(), ".json"),
            names(&parent_history, "directory-".into(), ".tmp"),
            names(&parent_history, "file-".into(), ".json"),
            names(&parent_history, "file-".into(), ".tmp"),
            names(&parent_history, "reclaim-".into(), ".json"),
            names(&parent_history, "reclaim-".into(), ".tmp"),
        ];
        file_reads.extend(writes.iter().cloned());
        access.directory_tree_reads.push(path.join(COORDINATION_DIRECTORY));
        access.directory_creates = vec![parent_coord, parent_history];
        access.file_writes = BTreeSet::from(writes).into_iter().collect();
    }
    access.file_reads = file_reads.into_iter().collect();
    Ok(access)
}
