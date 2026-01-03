#![allow(dead_code, unused)]

use crate::bazel::package::{Digest, DigestFunction, File, FileStore, Package};
use std::collections::HashMap;

pub struct Repository<F: FileStore> {
    repo_name: String,
    canonical_name: String,
    repositories: HashMap<String, String>,
    files: F,
}

impl<F: FileStore + Clone> Repository<F> {
    pub fn new(repo_name: String, canonical_name: String, files: F) -> Self {
        Self {
            repo_name,
            canonical_name,
            repositories: HashMap::new(),
            files,
        }
    }

    pub fn canonical_name(&self) -> &str {
        &self.canonical_name
    }

    pub fn add_repository(&mut self, name: String, canonical_name: String) {
        self.repositories.insert(name, canonical_name);
    }

    pub async fn read_package(&self, pkg: &str) -> Result<Package<F>, std::io::Error> {
        // Bazel looks for BUILD.bazel first, then BUILD. It's an error if both exist.
        // We read both in parallel to maximize performance.
        let build_bazel_path = format!("{pkg}/BUILD.bazel");
        let build_path = format!("{pkg}/BUILD");

        let (build_bazel_result, build_result) = tokio::join!(
            self.read_file(&build_bazel_path),
            self.read_file(&build_path)
        );

        match (build_bazel_result, build_result) {
            // Success case 1: BUILD.bazel exists, BUILD does not.
            (Ok(file), Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Package::new(self.files.clone(), file))
            }
            // Success case 2: BUILD.bazel does not exist, BUILD does.
            (Err(e), Ok(file)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Package::new(self.files.clone(), file))
            }
            // Error case 1: Both exist.
            (Ok(_), Ok(_)) => Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Package '{pkg}' contains both BUILD and BUILD.bazel files."),
            )),
            // Error case 2: Neither exists.
            (Err(e1), Err(e2))
                if e1.kind() == std::io::ErrorKind::NotFound
                    && e2.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Package '{pkg}' not found: missing BUILD or BUILD.bazel file."),
                ))
            }
            // Propagate other errors. If BUILD.bazel read had an error, propagate it first.
            (Err(e), _) => Err(e),
            // Otherwise, propagate the error from reading BUILD.
            (_, Err(e)) => Err(e),
        }
    }

    pub async fn read_file(&self, path: &str) -> Result<F::File, std::io::Error> {
        self.files.read_file(path).await
    }

    pub async fn read_dir(&self, path: &str) -> Result<Vec<String>, std::io::Error> {
        self.files.read_dir(path).await
    }
}

#[derive(Debug)]
pub struct LocalFile {
    path: std::path::PathBuf,
}

impl LocalFile {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl File for LocalFile {
    type AsyncRead = tokio::fs::File;

    async fn open(&self) -> Result<Self::AsyncRead, std::io::Error> {
        tokio::fs::File::open(&self.path).await
    }

    async fn digest(&self, _digest_function: DigestFunction) -> Result<Digest, std::io::Error> {
        // TODO: Implement actual digest calculation
        Ok(Digest {
            hash: "dummy_hash".to_string(),
            size_bytes: 0,
        })
    }
}

pub struct LocalFileStore {
    root: std::path::PathBuf,
}

impl LocalFileStore {
    #[allow(dead_code)]
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl FileStore for LocalFileStore {
    type File = LocalFile;

    async fn read_file(&self, path: &str) -> Result<Self::File, std::io::Error> {
        let full_path = self.root.join(path);
        Ok(LocalFile::new(full_path))
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<String>, std::io::Error> {
        let full_path = self.root.join(path);
        let mut entries = tokio::fs::read_dir(full_path).await?;
        let mut results = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let file_name = entry.file_name().to_string_lossy().to_string();
            results.push(file_name);
        }

        Ok(results)
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::{self};

    pub struct InMemoryFile {
        content: Vec<u8>,
    }

    impl InMemoryFile {
        pub fn new(content: Vec<u8>) -> Self {
            Self { content }
        }
    }

    impl File for InMemoryFile {
        type AsyncRead = std::io::Cursor<Vec<u8>>;

        async fn open(&self) -> Result<Self::AsyncRead, std::io::Error> {
            Ok(std::io::Cursor::new(self.content.clone()))
        }

        async fn digest(&self, _digest_function: DigestFunction) -> Result<Digest, std::io::Error> {
            // TODO: Implement actual digest calculation for in-memory files
            Ok(Digest {
                hash: "dummy_hash".to_string(),
                size_bytes: self.content.len() as i64,
            })
        }
    }

    pub struct InMemoryFileStore {
        files: HashMap<String, Vec<u8>>,
    }

    impl InMemoryFileStore {
        #[allow(dead_code)]
        pub fn new(files: HashMap<String, Vec<u8>>) -> Self {
            Self { files }
        }
    }

    impl FileStore for InMemoryFileStore {
        type File = InMemoryFile;

        async fn read_file(&self, path: &str) -> Result<Self::File, std::io::Error> {
            self.files
                .get(path)
                .map(|content| InMemoryFile::new(content.clone()))
                .ok_or_else(|| std::io::Error::new(io::ErrorKind::NotFound, "File not found"))
        }

        async fn read_dir(&self, path: &str) -> Result<Vec<String>, std::io::Error> {
            let dir_path = std::path::Path::new(path);
            let results: std::collections::HashSet<_> = self
                .files
                .keys()
                .filter_map(|file_path_str| {
                    std::path::Path::new(file_path_str)
                        .strip_prefix(dir_path)
                        .ok()
                        .and_then(|p| p.components().next())
                        .and_then(|c| match c {
                            std::path::Component::Normal(name) => {
                                Some(name.to_string_lossy().into_owned())
                            }
                            _ => None,
                        })
                })
                .collect();
            Ok(results.into_iter().collect())
        }
    }

    #[tokio::test]
    async fn test_in_memory_file_store_read_dir() {
        let files = HashMap::from([
            ("a/b".to_string(), vec![]),
            ("a/c".to_string(), vec![]),
            ("a/d/e".to_string(), vec![]),
            ("f".to_string(), vec![]),
        ]);

        let store = InMemoryFileStore::new(files);

        // Test root directory
        let mut root_entries = store.read_dir("").await.unwrap();
        root_entries.sort();
        assert_eq!(root_entries, vec!["a", "f"]);

        // Test subdirectory "a" (with and without trailing slash)
        for path in ["a", "a/"] {
            let mut entries = store.read_dir(path).await.unwrap();
            entries.sort();
            assert_eq!(entries, vec!["b", "c", "d"], "Failed for path: {path}");
        }

        // Test deeper subdirectory "a/d"
        let mut ad_entries = store.read_dir("a/d").await.unwrap();
        ad_entries.sort();
        assert_eq!(ad_entries, vec!["e"]);

        // Test non-existent directory
        let none_entries = store.read_dir("nonexistent").await.unwrap();
        assert!(none_entries.is_empty());
    }
}
