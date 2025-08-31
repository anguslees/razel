use allocative::Allocative;
use derive_more::Display;
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::list::UnpackList;
use starlark::values::none::{NoneOr, NoneType};
use starlark::values::tuple::UnpackTuple;
use starlark::values::{NoSerialize, StarlarkValue, Value, starlark_value};
use starlark::{starlark_module, starlark_simple_value};
use std::cell::{RefCell, RefMut};
use std::default::Default;
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Display, ProvidesStaticType, NoSerialize, Allocative)]
struct ModuleExtensionProxy;
starlark_simple_value!(ModuleExtensionProxy);

#[starlark_value(type = "module_extension_proxy")]
impl<'v> StarlarkValue<'v> for ModuleExtensionProxy {}

#[derive(Debug, Display, ProvidesStaticType, NoSerialize, Allocative)]
struct RepoRuleProxy;
starlark_simple_value!(RepoRuleProxy);

#[starlark_value(type = "repo_rule_proxy")]
impl<'v> StarlarkValue<'v> for RepoRuleProxy {}

#[derive(Debug, Default)]
pub(crate) struct ModuleBuilder {
    is_root_module: bool,
    ignore_dev_dependency: bool,
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) repo_name: Option<String>,
    pub(crate) bazel_deps: Vec<String>,
    pub(crate) archive_overrides: Vec<String>,
    pub(crate) local_path_overrides: Vec<String>,
    pub(crate) git_overrides: Vec<String>,
    pub(crate) use_extensions: Vec<String>,
    pub(crate) includes: Vec<String>,
}

impl ModuleBuilder {
    pub(crate) fn merge(&mut self, other: ModuleBuilder) {
        self.bazel_deps.extend(other.bazel_deps);
        self.archive_overrides.extend(other.archive_overrides);
        self.local_path_overrides.extend(other.local_path_overrides);
        self.git_overrides.extend(other.git_overrides);
        self.use_extensions.extend(other.use_extensions);
        self.includes.extend(other.includes);
    }
}

#[derive(Debug, ProvidesStaticType)]
pub(crate) struct ModuleExtra(RefCell<ModuleBuilder>);

impl ModuleExtra {
    pub fn new() -> Self {
        Self(RefCell::new(ModuleBuilder::default()))
    }

    pub fn new_root() -> Self {
        let mut builder = ModuleBuilder::default();
        builder.is_root_module = true;
        Self(RefCell::new(builder))
    }

    pub fn with_ignore_dev_dependency(mut self, ignore_dev_dependency: bool) -> Self {
        self.0.get_mut().ignore_dev_dependency = ignore_dev_dependency;
        self
    }

    fn from_eval<'a>(eval: &'a Evaluator) -> &'a Self {
        eval.extra.unwrap().downcast_ref::<Self>().unwrap()
    }

    pub fn builder<'a>(&'a self) -> RefMut<'a, ModuleBuilder> {
        self.0.borrow_mut()
    }

    pub fn into_inner(self) -> ModuleBuilder {
        self.0.into_inner()
    }
}

#[allow(unused)] // for now
#[starlark_module]
pub(crate) fn module_bazel(builder: &mut GlobalsBuilder) {
    /// Declares that a module depends on an archive at a remote location.
    /// https://bazel.build/rules/lib/globals/module#archive_override
    fn archive_override(
        module_name: &str,
        #[starlark(kwargs)] _kwargs: Value,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module {
            bzl_module.archive_overrides.push(module_name.to_string());
            todo!();
        }
        Ok(NoneType)
    }

    /// Declares that a module depends on another module.
    /// https://bazel.build/rules/lib/globals/module#bazel_dep
    fn bazel_dep(
        name: &str,
        #[starlark(default = "")] version: &str,
        #[starlark(default=-1)] max_compatibility_level: i32,
        #[starlark(default=NoneOr::Other(""))] repo_name: NoneOr<&str>,
        #[starlark(default = false)] dev_dependency: bool,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module || (dev_dependency && !bzl_module.ignore_dev_dependency) {
            todo!();
        }
        Ok(NoneType)
    }

    /// Declares that a module depends on a commit from a git repository.
    /// https://bazel.build/rules/lib/globals/module#git_override
    fn git_override(
        module_name: &str,
        #[starlark(kwargs)] kwargs: Value,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module {
            bzl_module.git_overrides.push(module_name.to_string());
            todo!();
        }
        Ok(NoneType)
    }

    fn include(
        #[starlark(require = pos)] label: String,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        bzl_module.includes.push(label);
        Ok(NoneType)
    }

    fn inject_repo(
        extension_proxy: Value,
        #[starlark(args)] args: UnpackTuple<Value>,
        #[starlark(kwargs)] kwargs: SmallMap<&str, &str>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module && !bzl_module.ignore_dev_dependency {
            todo!();
        }
        Ok(NoneType)
    }

    /// Declares that a module depends on a module in the local filesystem.
    /// https://bazel.build/rules/lib/globals/module#local_path_override
    fn local_path_override(
        module_name: &str,
        path: &str,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module {
            todo!();
        }
        Ok(NoneType)
    }

    fn module(
        #[starlark(default = String::from(""))] name: String,
        #[starlark(default = String::from(""))] version: String,
        #[starlark(default = 0)] _compatibility_level: i32,
        #[starlark(default = String::from(""))] repo_name: String,
        #[starlark(default=UnpackList::default())] _bazel_compatibility: UnpackList<String>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.name.is_some() {
            return Err(starlark::Error::new_native(anyhow::anyhow!(
                "module() can only be called once"
            )));
        }
        bzl_module.name = Some(name);
        bzl_module.version = Some(version);
        bzl_module.repo_name = Some(repo_name);
        Ok(NoneType)
    }

    fn multiple_version_override(
        module_name: &str,
        versions: UnpackList<&str>,
        #[starlark(default = "")] registry: &str,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module {
            todo!();
        }
        Ok(NoneType)
    }

    fn override_repo(
        extension_proxy: Value,
        #[starlark(args)] args: UnpackTuple<Value>,
        #[starlark(kwargs)] kwargs: SmallMap<&str, &str>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module && !bzl_module.ignore_dev_dependency {
            todo!();
        }
        Ok(NoneType)
    }

    fn register_execution_platforms(
        #[starlark(default = false)] dev_dependency: bool,
        #[starlark(args)] platform_labels: UnpackTuple<&str>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module || (dev_dependency && !bzl_module.ignore_dev_dependency) {
            todo!();
        }
        Ok(NoneType)
    }

    fn register_toolchains(
        #[starlark(default = false)] dev_dependency: bool,
        #[starlark(args)] toolchain_labels: UnpackTuple<&str>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module || (dev_dependency && !bzl_module.ignore_dev_dependency) {
            todo!();
        }
        Ok(NoneType)
    }

    fn single_version_override(
        module_name: &str,
        #[starlark(default = "")] version: &str,
        #[starlark(default = "")] registry: &str,
        #[starlark(default=UnpackList::default())] patches: UnpackList<&str>,
        #[starlark(default=UnpackList::default())] patch_cmds: UnpackList<&str>,
        #[starlark(default = 0)] patch_strip: i32,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if bzl_module.is_root_module {
            todo!();
        }
        Ok(NoneType)
    }

    /// Uses an extension from another module.
    /// https://bazel.build/rules/lib/globals/module#use_extension
    fn use_extension(
        extension_bzl_file: &str,
        extension_name: &str,
        #[starlark(default = false)] dev_dependency: bool,
        #[starlark(default = false)] isolate: bool,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneOr<ModuleExtensionProxy>> {
        let mut bzl_module = ModuleExtra::from_eval(eval).builder();
        if !bzl_module.is_root_module && (!dev_dependency || bzl_module.ignore_dev_dependency) {
            // "usage of module extension is ignored"
            return Ok(NoneOr::None);
        }
        if isolate {
            todo!()
        }
        todo!();
        Ok(NoneOr::Other(ModuleExtensionProxy {}))
    }

    fn use_repo(
        extension_proxy: &ModuleExtensionProxy,
        #[starlark(args)] args: UnpackTuple<&str>,
        #[starlark(kwargs)] kwargs: SmallMap<&str, &str>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        todo!();
        Ok(NoneType)
    }

    fn use_repo_rule(
        repo_rule_bzl_file: &str,
        repo_rule_name: &str,
        eval: &mut Evaluator,
    ) -> starlark::Result<RepoRuleProxy> {
        todo!();
        Ok(RepoRuleProxy {})
    }
}

#[derive(Debug, Default)]
pub(crate) struct RepoBuilder {
    default_metadata: Option<SmallMap<String, String>>,
    ignore_directories: Vec<String>,
}

#[derive(Debug, ProvidesStaticType)]
pub(crate) struct RepoExtra(Mutex<RepoBuilder>);

impl RepoExtra {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(RepoBuilder::default()))
    }

    fn from_eval<'a>(eval: &'a Evaluator) -> &'a Self {
        eval.extra.unwrap().downcast_ref::<Self>().unwrap()
    }

    fn builder(&self) -> MutexGuard<RepoBuilder> {
        self.0.lock().unwrap()
    }
}

#[allow(unused)] // for now
#[starlark_module]
pub(crate) fn repo_bazel(builder: &mut GlobalsBuilder) {
    fn ignore_directories(
        mut dirs: UnpackList<String>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut repo = RepoExtra::from_eval(eval).builder();
        repo.ignore_directories.append(&mut dirs.items);
        Ok(NoneType)
    }

    fn repo(
        #[starlark(kwargs)] kwargs: SmallMap<String, String>,
        eval: &mut Evaluator,
    ) -> starlark::Result<NoneType> {
        let mut repo = RepoExtra::from_eval(eval).builder();
        if repo.default_metadata.is_some() {
            return Err(starlark::Error::new_native(anyhow::anyhow!(
                "repo() can only be called once"
            )));
        }
        repo.default_metadata = Some(kwargs);
        Ok(NoneType)
    }
}
