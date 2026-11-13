use crate::descriptor::NetworkCeiling;
use crate::{AbsPath, WorkerEnvError};

#[derive(Clone, Debug)]
pub struct ProjectedSandbox {
    writable_roots: Vec<AbsPath>,
    read_only_subpaths: Vec<AbsPath>,
    readable_roots: Vec<AbsPath>,
    network: NetworkCeiling,
}

impl ProjectedSandbox {
    pub fn from_parts(
        writable_roots: Vec<AbsPath>,
        read_only_subpaths: Vec<AbsPath>,
        readable_roots: Vec<AbsPath>,
        network: NetworkCeiling,
    ) -> Self {
        Self {
            writable_roots,
            read_only_subpaths,
            readable_roots,
            network,
        }
    }

    pub fn writable_roots(&self) -> &[AbsPath] {
        &self.writable_roots
    }
    pub fn read_only_subpaths(&self) -> &[AbsPath] {
        &self.read_only_subpaths
    }
    pub fn readable_roots(&self) -> &[AbsPath] {
        &self.readable_roots
    }
    pub fn network(&self) -> NetworkCeiling {
        self.network
    }

    pub fn grants_whole_host(&self) -> bool {
        self.network == NetworkCeiling::Full
            && self.read_only_subpaths.is_empty()
            && self.writable_roots.iter().any(|root| root.as_path() == std::path::Path::new("/"))
    }

    pub fn authorize_write(&self, path: &AbsPath) -> Result<(), WorkerEnvError> {
        let inside_writable = self.writable_roots.iter().any(|r| path.is_within(r));
        if !inside_writable {
            return Err(WorkerEnvError::WriteOutsideRoots(path.as_str().to_string()));
        }
        let inside_protected = self.read_only_subpaths.iter().any(|r| path.is_within(r));
        if inside_protected {
            return Err(WorkerEnvError::WriteOutsideRoots(format!(
                "{} is under a protected read-only subpath",
                path.as_str()
            )));
        }
        Ok(())
    }

    pub fn authorize_read(&self, path: &AbsPath) -> Result<(), WorkerEnvError> {
        let ok = self
            .readable_roots
            .iter()
            .chain(self.writable_roots.iter())
            .any(|r| path.is_within(r));
        if ok {
            Ok(())
        } else {
            Err(WorkerEnvError::ReadOutsideRoots(path.as_str().to_string()))
        }
    }
}
