use starlark::environment::Module as StarlarkModule;
use starlark::{
    collections::SmallMap,
    environment::{Globals, GlobalsBuilder},
    eval::Evaluator,
    syntax::{AstModule, Dialect},
};
use std::{path::Path, sync::LazyLock};

use crate::starlark::globals::module::{ModuleBuilder, ModuleExtra, RepoExtra};

/// MODULE.bazel file
#[derive(Debug)]
#[allow(dead_code)]
pub struct Module {
    pub name: String,
    pub version: String,
    pub repo_name: String,
    pub bazel_deps: Vec<String>,
    pub archive_overrides: Vec<String>,
    pub local_path_overrides: Vec<String>,
    pub git_overrides: Vec<String>,
    pub use_extensions: Vec<String>,
}

impl TryFrom<ModuleBuilder> for Module {
    type Error = anyhow::Error;

    fn try_from(value: ModuleBuilder) -> Result<Self, Self::Error> {
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

pub(crate) async fn eval_module(path: &Path, is_root: bool) -> anyhow::Result<Module> {
    let mut builder = eval_module_include(path, is_root).await?;

    // TODO: parallelise parsing of includes

    let mut includes = builder.includes.clone();
    while let Some(include) = includes.pop() {
        let sub_path: std::path::PathBuf = path.parent().unwrap().join(include);
        let sub_builder = eval_module_include(&sub_path, is_root).await?;
        includes.extend(sub_builder.includes.clone());
        builder.merge(sub_builder);
    }

    Module::try_from(builder)
}

// TODO: move ModuleExtra to the scope-limited (and sync) eval_module() call below,
// and change this into eval_module() -> Result<ModuleBuilder>.
// The includes loop then becomes ModuleBuilder.merge_into() or similar.
async fn eval_module_include(path: &Path, is_root: bool) -> anyhow::Result<ModuleBuilder> {
    let bzl_module = if is_root {
        ModuleExtra::new_root()
    } else {
        ModuleExtra::new()
    };

    // Fetching file contents should be async
    let ast: AstModule =
        AstModule::parse_file(path, &DIALECT_MODULE).map_err(|e| e.into_anyhow())?;

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
#[allow(dead_code)]
pub struct Repo {
    ignore_directories: Vec<String>,
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
