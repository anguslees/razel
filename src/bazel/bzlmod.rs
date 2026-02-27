use crate::bazel::package::{BoxFileStore, File, FileStore};
use crate::starlark::globals::module::{ModuleBuilder, ModuleExtra, RepoExtra};
use allocative::Allocative;
use starlark::environment::Module as StarlarkModule;
use starlark::{
    collections::SmallMap,
    environment::{Globals, GlobalsBuilder},
    eval::Evaluator,
    syntax::{AstModule, Dialect},
};
use std::{path::Path, sync::LazyLock};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Allocative)]
pub struct BazelDep {
    pub name: String,
    pub version: String,
    pub repo_name: String,
    // TODO: max_compatibility_level, dev_dependency
}

/// MODULE.bazel file
///
/// A Bazel project that can have multiple versions, each of which can have dependencies on other modules.
///
/// In a local Bazel workspace, a module is represented by a `Repository`.
///
/// See https://bazel.build/external/module
#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub version: String,
    pub repo_name: String,
    pub bazel_deps: Vec<BazelDep>,
    #[allow(dead_code)]
    pub archive_overrides: Vec<String>,
    #[allow(dead_code)]
    pub local_path_overrides: Vec<String>,
    #[allow(dead_code)]
    pub git_overrides: Vec<String>,
    #[allow(dead_code)]
    pub use_extensions: Vec<String>,
}

impl<'a> TryFrom<ModuleBuilder<'a>> for Module {
    type Error = anyhow::Error;

    fn try_from(value: ModuleBuilder<'a>) -> Result<Self, Self::Error> {
        let name = value
            .name
            .ok_or_else(|| anyhow::anyhow!("Module name is required"))?;
        let repo_name = value.repo_name.unwrap_or_else(|| name.clone());
        let version = value
            .version
            .ok_or_else(|| anyhow::anyhow!("Module version is required"))?;

        Ok(Self {
            name,
            version,
            repo_name,
            bazel_deps: value.bazel_deps,
            archive_overrides: value.archive_overrides,
            local_path_overrides: value.local_path_overrides,
            git_overrides: value.git_overrides,
            use_extensions: value.use_extensions,
        })
    }
}

const DIALECT_MODULE: Dialect = Dialect {
    enable_load: false,
    ..Dialect::Standard
};

static MODULE_GLOBALS: LazyLock<Globals> = LazyLock::new(|| {
    GlobalsBuilder::standard()
        .with(crate::starlark::globals::module::module_bazel)
        .build()
});

pub(crate) async fn eval_module(
    files: &BoxFileStore<'static>,
    path: &str,
    is_root: bool,
) -> anyhow::Result<Module> {
    let mut builder = eval_module_include(files, path, is_root).await?;

    // TODO: parallelise parsing of includes.

    let mut includes = builder.includes.clone();
    while let Some(include) = includes.pop() {
        // Handle include path relative to the current file's directory.
        // Since path is relative to repo root, we need to join them.
        let current_dir = Path::new(path).parent().unwrap_or(Path::new(""));
        let sub_path_buf = current_dir.join(&include);
        let sub_path = sub_path_buf.to_str().ok_or_else(|| {
            anyhow::anyhow!("Invalid unicode in include path: {:?}", sub_path_buf)
        })?;

        let sub_builder = eval_module_include(files, sub_path, is_root).await?;
        includes.extend(sub_builder.includes.clone());
        builder.merge(sub_builder);
    }

    Module::try_from(builder)
}

// TODO: move ModuleExtra to the scope-limited (and sync) eval_module() call below,
// and change this into eval_module() -> Result<ModuleBuilder>.
// The includes loop then becomes ModuleBuilder.merge_into() or similar.
async fn eval_module_include(
    files: &BoxFileStore<'static>,
    path: &str,
    is_root: bool,
) -> anyhow::Result<ModuleBuilder<'static>> {
    let files_owned = files.clone();

    let bzl_module = if is_root {
        ModuleExtra::new_root(files_owned)
    } else {
        ModuleExtra::new(files_owned)
    };

    // Fetch file contents
    let file = files.read_file(path).await?;
    let mut content = String::new();
    (*file).open().await?.read_to_string(&mut content).await?;

    let ast: AstModule =
        AstModule::parse(path, content, &DIALECT_MODULE).map_err(|e| e.into_anyhow())?;

    let module = StarlarkModule::new();

    {
        let mut eval = Evaluator::new(&module);
        eval.extra = Some(&bzl_module);
        eval.eval_module(ast, &MODULE_GLOBALS)
            .map_err(|e| e.into_anyhow())?;
    }
    println!("MODULE.bazel defined module name {bzl_module:?}");

    Ok(bzl_module.into_inner())
}

/// REPO.bazel
///
/// The `REPO.bazel` file is used to mark the topmost boundary of the directory tree that constitutes a repo. It doesn't need to contain anything to serve as a repo boundary file; however, it can also be used to specify some common attributes for all build targets inside the repo.
///
/// See https://bazel.build/rules/lib/globals/repo
#[allow(dead_code)]
pub struct Repo {
    /// The list of directories to ignore in this repository.
    ignore_directories: Vec<String>,

    /// Declares metadata that applies to every rule in the repository. It must be called at most once per `REPO.bazel` file. If called, it must be the first call in the `REPO.bazel` file.
    // TODO: this type is incorrect.  Needs to be **kwargs (string->any), or explicitly enumerate the args.
    repo: SmallMap<String, String>,
}

#[allow(dead_code)]
static REPO_GLOBALS: LazyLock<Globals> = LazyLock::new(|| {
    GlobalsBuilder::standard()
        .with(crate::starlark::globals::module::repo_bazel)
        .build()
});

#[allow(dead_code)]
pub(crate) async fn eval_repo(path: &Path) -> anyhow::Result<Module> {
    let repo_bazel = RepoExtra::new();

    // TODO: update this to use FileStore if needed, or keeping Path is fine for now if it's separate
    let ast: AstModule =
        AstModule::parse_file(path, &DIALECT_MODULE).map_err(|e| e.into_anyhow())?;

    let module = StarlarkModule::new();

    {
        let mut eval = Evaluator::new(&module);
        eval.extra = Some(&repo_bazel);
        eval.eval_module(ast, &REPO_GLOBALS)
            .map_err(|e| e.into_anyhow())?;
    }

    todo!()
}
