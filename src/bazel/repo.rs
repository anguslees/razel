use tokio::io;

pub type Digest = bazel_remote_apis::build::bazel::remote::execution::v2::Digest;
pub type DigestFunction =
    bazel_remote_apis::build::bazel::remote::execution::v2::digest_function::Value;

pub trait File {
    type AsyncRead: io::AsyncRead;

    /// Get the file contents as an async reader.
    async fn open(&self) -> Result<Self::AsyncRead, std::io::Error>;

    /// Get the file digest.
    async fn digest(&self, digest_function: DigestFunction) -> Result<Digest, std::io::Error>;
}

pub trait Repository {
    type File: File;

    /// Read a file from the repository.
    ///
    /// The path is relative to the repository root.
    async fn read_file(&self, path: &str) -> Result<Self::File, std::io::Error>;

    /// Read a directory within the repository.
    ///
    /// The path is relative to the repository root.
    async fn read_dir(&self, path: &str) -> Result<Vec<String>, std::io::Error>;
}

pub struct TokioFile {
    path: std::path::PathBuf,
}

impl TokioFile {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl File for TokioFile {
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

pub struct TokioRepository {
    root: std::path::PathBuf,
}

impl TokioRepository {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl Repository for TokioRepository {
    type File = TokioFile;

    async fn read_file(&self, path: &str) -> Result<Self::File, std::io::Error> {
        let full_path = self.root.join(path);
        Ok(TokioFile::new(full_path))
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
    use std::{collections::HashMap, sync::Arc};
    use tokio::io::{self, AsyncReadExt};

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

    pub struct InMemoryRepository {
        files: HashMap<String, Vec<u8>>,
    }

    impl InMemoryRepository {
        pub fn new(files: HashMap<String, Vec<u8>>) -> Self {
            Self { files }
        }
    }

    impl Repository for InMemoryRepository {
        type File = InMemoryFile;

        async fn read_file(&self, path: &str) -> Result<Self::File, std::io::Error> {
            self.files
                .get(path)
                .map(|content| InMemoryFile::new(content.clone()))
                .ok_or_else(|| std::io::Error::new(io::ErrorKind::NotFound, "File not found"))
        }

        async fn read_dir(&self, path: &str) -> Result<Vec<String>, std::io::Error> {
            let mut results = Vec::new();
            for file_path in self.files.keys() {
                if let Some(entry) = file_path.strip_prefix(path) {
                    if let Some(part) = entry.split('/').next() {
                        results.push(part.to_string());
                    }
                }
            }
            Ok(results)
        }
    }
}
