use tokio::io;

pub use bazel_remote_apis::build::bazel::remote::execution::v2::Digest;
pub use bazel_remote_apis::build::bazel::remote::execution::v2::digest_function::Value as DigestFunction;

// A package is a directory with a BUILD file.
#[derive(Debug)]
pub struct Package<F: FileStore> {
    _build_file: F::File,
    _filestore: F,
}

impl<F: FileStore> Package<F> {
    pub fn new(filestore: F, build_file: F::File) -> Self {
        Self {
            _build_file: build_file,
            _filestore: filestore,
        }
    }

    pub fn sub_packages(&self) -> Vec<String> {
        // TODO: Implement this
        vec![]
    }
}

pub trait File {
    type AsyncRead: io::AsyncRead;

    /// Get the file contents as an async reader.
    async fn open(&self) -> Result<Self::AsyncRead, std::io::Error>;

    /// Get the file digest.
    async fn digest(&self, digest_function: DigestFunction) -> Result<Digest, std::io::Error>;
}

pub trait FileStore {
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
