#![allow(dead_code)]

use crate::bazel::label::{CanonicalLabel, CanonicalRepo};
use crate::bazel::rule::{
    AttributeValue, DirectAttributeValue, LoadedRuleClass, Rule, RuleAttribute, RuleClass,
    RuleClassId, RuleClassSpec, visit_attribute_dependency_labels,
};
use crate::bazel::target::{GeneratedFile, SourceFile, Target, TargetView};
use futures::future::{BoxFuture, FutureExt};
use starlark::values::OwnedFrozenValue;
use std::borrow::Cow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tokio::io;

pub use bazel_remote_apis::build::bazel::remote::execution::v2::Digest;
pub use bazel_remote_apis::build::bazel::remote::execution::v2::digest_function::Value as DigestFunction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirEntry {
    File(String),
    Directory(String),
}

#[derive(Debug)]
pub struct PackageSource<F: FileStore> {
    path: String,
    build_file_name: String,
    build_file: F::File,
    filestore: F,
}

impl<F: FileStore> PackageSource<F> {
    pub fn new(path: String, build_file_name: String, filestore: F, build_file: F::File) -> Self {
        Self {
            path,
            build_file_name,
            build_file,
            filestore,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn build_file_name(&self) -> &str {
        &self.build_file_name
    }

    pub fn build_file(&self) -> &F::File {
        &self.build_file
    }

    pub fn filestore(&self) -> &F {
        &self.filestore
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageId<'a> {
    pub repository: CanonicalRepo<'a>,
    pub package: Cow<'a, str>,
}

impl<'a> PackageId<'a> {
    pub fn new(repository: CanonicalRepo<'a>, package: impl Into<Cow<'a, str>>) -> Self {
        Self {
            repository,
            package: package.into(),
        }
    }

    pub fn as_borrowed(&self) -> PackageId<'_> {
        PackageId {
            repository: self.repository.as_borrowed(),
            package: Cow::Borrowed(self.package.as_ref()),
        }
    }

    pub fn into_owned(self) -> PackageId<'static> {
        PackageId {
            repository: self.repository.into_owned(),
            package: Cow::Owned(self.package.into_owned()),
        }
    }

    pub fn label<'b>(&'b self, name: &'b str) -> CanonicalLabel<'b> {
        CanonicalLabel::new(self.repository.as_borrowed(), self.package.as_ref(), name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PackageDefaults {
    pub visibility: Vec<CanonicalLabel<'static>>,
    pub testonly: bool,
    pub deprecation: String,
    pub package_metadata: Vec<CanonicalLabel<'static>>,
    pub features: Vec<String>,
}

#[derive(Debug)]
pub struct Package {
    id: PackageId<'static>,
    build_file_name: String,
    defaults: PackageDefaults,
    targets: BTreeMap<String, Target>,
}

impl Package {
    pub fn id(&self) -> &PackageId<'static> {
        &self.id
    }

    pub fn build_file_name(&self) -> &str {
        &self.build_file_name
    }

    pub fn defaults(&self) -> &PackageDefaults {
        &self.defaults
    }

    pub fn target(&self, name: &str) -> Option<TargetView<'_>> {
        self.targets
            .get_key_value(name)
            .map(|(name, target)| TargetView::new(self.id.label(name), target))
    }

    pub(crate) fn target_payload(&self, name: &str) -> Option<&Target> {
        self.targets.get(name)
    }

    pub fn targets(&self) -> impl Iterator<Item = TargetView<'_>> {
        self.targets
            .iter()
            .map(|(name, target)| TargetView::new(self.id.label(name), target))
    }

    pub fn rules(&self) -> impl Iterator<Item = TargetView<'_>> {
        self.targets()
            .filter(|target| matches!(target.target(), Target::Rule(_)))
    }

    pub fn source_files(&self) -> impl Iterator<Item = TargetView<'_>> {
        self.targets()
            .filter(|target| matches!(target.target(), Target::SourceFile(_)))
    }

    pub fn generated_files(&self) -> impl Iterator<Item = TargetView<'_>> {
        self.targets()
            .filter(|target| matches!(target.target(), Target::GeneratedFile(_)))
    }
}

#[derive(Debug)]
struct PendingRule {
    rule_class: PendingRuleClass,
    attributes: Box<[Option<RuleAttribute>]>,
}

#[derive(Debug)]
enum PendingRuleClass {
    Native(&'static RuleClass),
    Starlark(RuleClassId),
}

impl PendingRuleClass {
    fn id(&self) -> &RuleClassId {
        match self {
            Self::Native(rule_class) => &rule_class.id,
            Self::Starlark(id) => id,
        }
    }
}

#[derive(Debug)]
pub struct PackageBuilder<'a> {
    id: PackageId<'a>,
    build_file_name: String,
    defaults: PackageDefaults,
    package_called: bool,
    declarations_started: bool,
    targets: BTreeMap<String, Target>,
    pending_rules: BTreeMap<String, PendingRule>,
    inferred_sources: BTreeSet<String>,
}

impl<'a> PackageBuilder<'a> {
    pub fn new(id: PackageId<'a>, build_file_name: impl Into<String>) -> anyhow::Result<Self> {
        let build_file_name = build_file_name.into();
        validate_package_name(id.package.as_ref())?;
        let mut targets = BTreeMap::new();
        targets.insert(
            build_file_name.clone(),
            Target::SourceFile(SourceFile {
                visibility: Vec::new(),
            }),
        );
        Ok(Self {
            id,
            build_file_name,
            defaults: PackageDefaults::default(),
            package_called: false,
            declarations_started: false,
            targets,
            pending_rules: BTreeMap::new(),
            inferred_sources: BTreeSet::new(),
        })
    }

    pub fn id(&self) -> &PackageId<'a> {
        &self.id
    }

    pub fn set_package_defaults(&mut self, defaults: PackageDefaults) -> anyhow::Result<()> {
        if self.package_called {
            anyhow::bail!("package() can only be called once");
        }
        if self.declarations_started {
            anyhow::bail!("package() must be called before targets or exports_files()");
        }
        self.package_called = true;
        self.defaults = defaults;
        Ok(())
    }

    pub fn export_source_file(
        &mut self,
        name: &str,
        visibility: Vec<CanonicalLabel<'static>>,
    ) -> anyhow::Result<()> {
        self.declarations_started = true;
        validate_target_name(name)?;
        if self.pending_rules.contains_key(name) {
            anyhow::bail!("source file `{name}` collides with an existing rule or output");
        }
        match self.targets.get_mut(name) {
            Some(Target::SourceFile(source)) => {
                for label in visibility {
                    if !source.visibility.contains(&label) {
                        source.visibility.push(label);
                    }
                }
            }
            Some(Target::Rule(_) | Target::GeneratedFile(_)) => {
                anyhow::bail!("source file `{name}` collides with an existing target");
            }
            None => {
                self.targets.insert(
                    name.to_owned(),
                    Target::SourceFile(SourceFile { visibility }),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn instantiate_rule(
        &mut self,
        rule_class: RuleClassSpec<'_>,
        mut explicit: BTreeMap<&str, AttributeValue>,
    ) -> anyhow::Result<()> {
        self.declarations_started = true;
        let schemas = rule_class.attributes();
        let permissive = rule_class.native_permissive();

        for name in explicit.keys() {
            let Some(schema) = schemas.iter().find(|schema| schema.name == *name) else {
                if permissive {
                    continue;
                }
                anyhow::bail!("unknown attribute `{name}`");
            };
            if !schema.public {
                anyhow::bail!("private attribute `{name}` cannot be set by BUILD callers");
            }
        }
        if permissive {
            explicit.retain(|name, _| schemas.iter().any(|schema| schema.name == *name));
        }

        let mut attributes = Vec::with_capacity(schemas.len());
        for schema in schemas {
            if let Some(value) = explicit.remove(schema.name.as_str()) {
                attributes.push(Some(RuleAttribute {
                    value,
                    explicit: true,
                }));
                continue;
            }
            if schema.mandatory {
                anyhow::bail!("missing mandatory attribute `{}`", schema.name);
            }
            if let Some(default) = self.package_default(schema.name.as_ref()) {
                attributes.push(Some(RuleAttribute {
                    value: AttributeValue::direct(default),
                    explicit: false,
                }));
                continue;
            }
            if let Some(default) = &schema.default {
                attributes.push(Some(RuleAttribute {
                    value: AttributeValue::direct(default.clone()),
                    explicit: false,
                }));
                continue;
            }
            if !schema.public {
                anyhow::bail!("private attribute `{}` must have a default", schema.name);
            }
            attributes.push(None);
        }

        let name_index = schemas
            .iter()
            .position(|schema| schema.name == "name")
            .expect("all rule classes have a name attribute");
        let name = attributes[name_index]
            .as_ref()
            .and_then(|attribute| attribute.value.direct_value())
            .and_then(|value| match value {
                DirectAttributeValue::String(name) => Some(name.as_str()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("attribute `name` must be a nonconfigurable string"))?;
        validate_target_name(name)?;
        self.ensure_name_available(name, "rule")?;

        let mut output_names = Vec::new();
        let mut unique_outputs = BTreeSet::new();
        for (schema, attribute) in schemas.iter().zip(attributes.iter()) {
            if schema.dependency != crate::bazel::rule::AttributeDependency::Output {
                continue;
            }
            if let Some(attribute) = attribute
                && let Some(value) = attribute.value.direct_value()
            {
                value.try_visit_labels(|output| {
                    if output.repo != self.id.repository
                        || output.package() != self.id.package.as_ref()
                    {
                        anyhow::bail!("output `{output}` must remain in the rule package");
                    }
                    if output.name() == name {
                        anyhow::bail!(
                            "generated output `{output}` collides with its generating rule"
                        );
                    }
                    if !unique_outputs.insert(output.name()) {
                        anyhow::bail!("generated output `{output}` is declared more than once");
                    }
                    self.ensure_name_available(output.name(), "generated output")?;
                    output_names.push(output.name());
                    Ok(())
                })?;
            }
        }

        let rule_name = name.to_owned();
        for output_name in output_names {
            self.targets.insert(
                output_name.to_owned(),
                Target::GeneratedFile(GeneratedFile {
                    generating_rule: rule_name.clone(),
                }),
            );
        }

        collect_inferred_sources_for_schemas(
            &self.id,
            schemas,
            &attributes,
            &mut self.inferred_sources,
        );

        let rule_class = match rule_class {
            RuleClassSpec::Native(rule_class) => PendingRuleClass::Native(rule_class),
            RuleClassSpec::Starlark { id, .. } => PendingRuleClass::Starlark(id),
        };
        self.pending_rules.insert(
            rule_name,
            PendingRule {
                rule_class,
                attributes: attributes.into_boxed_slice(),
            },
        );
        Ok(())
    }

    pub(crate) fn starlark_rule_classes(&self) -> Vec<&RuleClassId> {
        let mut seen = std::collections::HashSet::new();
        self.pending_rules
            .values()
            .filter_map(|pending| match &pending.rule_class {
                PendingRuleClass::Native(_) => None,
                PendingRuleClass::Starlark(id) if seen.insert(id) => Some(id),
                PendingRuleClass::Starlark(_) => None,
            })
            .collect()
    }

    pub(crate) fn finish(
        mut self,
        starlark_rule_classes: &HashMap<RuleClassId, OwnedFrozenValue>,
    ) -> anyhow::Result<Package> {
        let pending_rules = std::mem::take(&mut self.pending_rules);
        for (name, pending) in pending_rules {
            let rule_class = match pending.rule_class {
                PendingRuleClass::Native(rule_class) => LoadedRuleClass::Native(rule_class),
                PendingRuleClass::Starlark(id) => LoadedRuleClass::Starlark(
                    starlark_rule_classes
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("unresolved Starlark rule class {id:?}"))?,
                ),
            };
            let rule = Rule {
                rule_class,
                attributes: pending.attributes,
            };
            match self.targets.entry(name) {
                Entry::Vacant(entry) => {
                    entry.insert(Target::Rule(rule));
                }
                Entry::Occupied(entry) => {
                    anyhow::bail!(
                        "rule `{}` collides with an existing source target",
                        entry.key()
                    );
                }
            }
        }

        for name in self.inferred_sources {
            if self.targets.contains_key(&name) {
                continue;
            }
            self.targets.insert(
                name,
                Target::SourceFile(SourceFile {
                    visibility: Vec::new(),
                }),
            );
        }

        Ok(Package {
            id: self.id.into_owned(),
            build_file_name: self.build_file_name,
            defaults: self.defaults,
            targets: self.targets,
        })
    }

    fn ensure_name_available(&self, name: &str, kind: &str) -> anyhow::Result<()> {
        if self.targets.contains_key(name) || self.pending_rules.contains_key(name) {
            anyhow::bail!("{kind} `{name}` collides with an existing target");
        }
        Ok(())
    }

    fn package_default(&self, name: &str) -> Option<DirectAttributeValue> {
        match name {
            "visibility" if !self.defaults.visibility.is_empty() => Some(
                DirectAttributeValue::LabelList(self.defaults.visibility.clone()),
            ),
            "testonly" if self.defaults.testonly => Some(DirectAttributeValue::Bool(true)),
            "deprecation" if !self.defaults.deprecation.is_empty() => Some(
                DirectAttributeValue::String(self.defaults.deprecation.clone()),
            ),
            "package_metadata" if !self.defaults.package_metadata.is_empty() => Some(
                DirectAttributeValue::LabelList(self.defaults.package_metadata.clone()),
            ),
            "features" if !self.defaults.features.is_empty() => Some(
                DirectAttributeValue::StringList(self.defaults.features.clone()),
            ),
            _ => None,
        }
    }
}

fn collect_inferred_sources_for_schemas(
    id: &PackageId<'_>,
    schemas: &[crate::bazel::rule::AttributeSchema],
    attributes: &[Option<RuleAttribute>],
    inferred_sources: &mut BTreeSet<String>,
) {
    for (schema, attribute) in schemas.iter().zip(attributes) {
        if let Some(attribute) = attribute {
            visit_attribute_dependency_labels(schema, &attribute.value, |label| {
                if label.repo == id.repository
                    && label.package() == id.package.as_ref()
                    && !inferred_sources.contains(label.name())
                {
                    inferred_sources.insert(label.name().to_owned());
                }
            });
        }
    }
}

fn validate_target_name(name: &str) -> anyhow::Result<()> {
    crate::bazel::label::validate_target_name(name)
        .map_err(|error| anyhow::anyhow!("invalid target name `{name}`: {error}"))
}

fn validate_package_name(name: &str) -> anyhow::Result<()> {
    crate::bazel::label::validate_package_name(name)
        .map_err(|error| anyhow::anyhow!("invalid package name `{name}`: {error}"))
}

pub type BoxAsyncRead = Box<dyn io::AsyncRead + Unpin + Send>;
pub type BoxFile<'a> = Box<DynFile<'a, BoxAsyncRead>>;
pub type BoxFileStore<'a> = std::sync::Arc<DynFileStore<'a, BoxFile<'a>>>;

#[dynosaur::dynosaur(pub DynFile = dyn(box) File)]
pub trait File: Send + Sync + std::fmt::Debug {
    type AsyncRead: io::AsyncRead;

    fn open(&self) -> BoxFuture<'_, Result<Self::AsyncRead, std::io::Error>>;

    fn digest(
        &self,
        digest_function: DigestFunction,
    ) -> BoxFuture<'_, Result<Digest, std::io::Error>>;
}

impl<'a, R: io::AsyncRead> std::fmt::Debug for DynFile<'a, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynFile").finish_non_exhaustive()
    }
}

#[dynosaur::dynosaur(pub DynFileStore = dyn(box) FileStore)]
pub trait FileStore: Send + Sync + std::fmt::Debug {
    type File: crate::bazel::package::File;

    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>>;

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<DirEntry>, std::io::Error>>;
}

impl<'a, F: File> std::fmt::Debug for DynFileStore<'a, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynFileStore").finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct FileAdapter<F>(F);

impl<F: File> File for FileAdapter<F>
where
    F::AsyncRead: Unpin + Send + 'static,
{
    type AsyncRead = BoxAsyncRead;

    fn open(&self) -> BoxFuture<'_, Result<Self::AsyncRead, std::io::Error>> {
        async move {
            let reader = self.0.open().await?;
            Ok(Box::new(reader) as BoxAsyncRead)
        }
        .boxed()
    }

    fn digest(
        &self,
        digest_function: DigestFunction,
    ) -> BoxFuture<'_, Result<Digest, std::io::Error>> {
        self.0.digest(digest_function).boxed()
    }
}

impl<F: FileStore + ?Sized> FileStore for std::sync::Arc<F> {
    type File = F::File;

    fn read_file(&self, path: &str) -> BoxFuture<'_, Result<Self::File, std::io::Error>> {
        (**self).read_file(path)
    }

    fn read_dir(&self, path: &str) -> BoxFuture<'_, Result<Vec<DirEntry>, std::io::Error>> {
        (**self).read_dir(path)
    }
}

#[derive(Debug)]
pub struct TypeErasingFileStore<F>(pub F);

impl<F: FileStore> FileStore for TypeErasingFileStore<F>
where
    F::File: 'static,
    <<F as FileStore>::File as File>::AsyncRead: Unpin + Send + 'static,
{
    type File = BoxFile<'static>;

    fn read_file(&self, path: &str) -> BoxFuture<'_, std::io::Result<Self::File>> {
        let path = path.to_string();
        async move {
            self.0
                .read_file(&path)
                .await
                .map(|file| DynFile::new_box(FileAdapter(file)))
        }
        .boxed()
    }

    fn read_dir(&self, path: &str) -> BoxFuture<'_, std::io::Result<Vec<DirEntry>>> {
        self.0.read_dir(path).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bazel::label::MAIN_REPO;
    use crate::bazel::rule::{
        AttributeDependency, AttributeSchema, AttributeType, RuleClassSpec, native_rule_class,
    };
    use std::sync::LazyLock;

    fn label(name: &str) -> CanonicalLabel<'static> {
        CanonicalLabel::new(MAIN_REPO, "", name.to_owned())
    }

    fn explicit(
        name: &str,
        values: impl IntoIterator<Item = (&'static str, DirectAttributeValue)>,
    ) -> BTreeMap<&'static str, AttributeValue> {
        let mut attributes = BTreeMap::from([(
            "name",
            AttributeValue::direct(DirectAttributeValue::String(name.to_owned())),
        )]);
        attributes.extend(
            values
                .into_iter()
                .map(|(name, value)| (name, AttributeValue::direct(value))),
        );
        attributes
    }

    fn source_rule_class() -> &'static RuleClass {
        static RULE_CLASS: LazyLock<RuleClass> = LazyLock::new(|| {
            native_rule_class(
                "source_rule",
                vec![AttributeSchema::new(
                    "srcs",
                    AttributeType::LabelList,
                    Some(DirectAttributeValue::LabelList(Vec::new())),
                    AttributeDependency::Dependency,
                )],
            )
        });
        &RULE_CLASS
    }

    fn output_rule_class() -> &'static RuleClass {
        static RULE_CLASS: LazyLock<RuleClass> = LazyLock::new(|| {
            native_rule_class(
                "output_rule",
                vec![AttributeSchema::new(
                    "outs",
                    AttributeType::OutputList,
                    Some(DirectAttributeValue::OutputList(Vec::new())),
                    AttributeDependency::Output,
                )],
            )
        });
        &RULE_CLASS
    }

    #[test]
    fn builder_registers_build_and_missing_sources() {
        let mut builder =
            PackageBuilder::new(PackageId::new(MAIN_REPO, ""), "BUILD.bazel").unwrap();
        builder
            .instantiate_rule(
                RuleClassSpec::Native(source_rule_class()),
                explicit(
                    "consumer",
                    [(
                        "srcs",
                        DirectAttributeValue::LabelList(vec![label("missing.txt")]),
                    )],
                ),
            )
            .unwrap();
        let package = builder.finish(&HashMap::new()).unwrap();
        assert!(matches!(
            package.target("BUILD.bazel").map(|target| target.target()),
            Some(Target::SourceFile(_))
        ));
        assert!(matches!(
            package.target("consumer").map(|target| target.target()),
            Some(Target::Rule(_))
        ));
        assert!(matches!(
            package.target("missing.txt").map(|target| target.target()),
            Some(Target::SourceFile(_))
        ));
        let target = package.target("consumer").unwrap();
        let label = target.label();
        assert!(matches!(&label.package, std::borrow::Cow::Borrowed(_)));
        assert!(matches!(&label.target, std::borrow::Cow::Borrowed(_)));
        assert!(std::ptr::eq(
            label.package().as_ptr(),
            package.id().package.as_ptr()
        ));
        assert!(std::ptr::eq(label.name().as_ptr(), target.name().as_ptr()));
        assert_eq!(package.rules().count(), 1);
        assert_eq!(package.source_files().count(), 2);
    }

    #[test]
    fn borrowed_package_id_is_owned_at_finish() {
        let repository = String::from("canonical");
        let package_name = String::from("nested/package");
        let id = PackageId::new(
            CanonicalRepo::new(repository.as_str()),
            package_name.as_str(),
        );
        assert!(matches!(&id.package, std::borrow::Cow::Borrowed(_)));

        let package = PackageBuilder::new(id, "BUILD.bazel")
            .unwrap()
            .finish(&HashMap::new())
            .unwrap();
        drop(repository);
        drop(package_name);
        assert_eq!(package.id().repository.as_str(), "canonical");
        assert_eq!(package.id().package, "nested/package");
    }

    #[test]
    fn package_builder_rejects_leading_and_trailing_slashes() {
        for package in ["/invalid", "invalid/"] {
            assert!(
                PackageBuilder::new(PackageId::new(MAIN_REPO, package), "BUILD.bazel").is_err()
            );
        }
    }

    #[test]
    fn forward_output_reference_does_not_create_source() {
        let mut builder =
            PackageBuilder::new(PackageId::new(MAIN_REPO, ""), "BUILD.bazel").unwrap();
        builder
            .instantiate_rule(
                RuleClassSpec::Native(source_rule_class()),
                explicit(
                    "consumer",
                    [(
                        "srcs",
                        DirectAttributeValue::LabelList(vec![label("generated.txt")]),
                    )],
                ),
            )
            .unwrap();
        builder
            .instantiate_rule(
                RuleClassSpec::Native(output_rule_class()),
                explicit(
                    "producer",
                    [(
                        "outs",
                        DirectAttributeValue::OutputList(vec![label("generated.txt")]),
                    )],
                ),
            )
            .unwrap();
        let package = builder.finish(&HashMap::new()).unwrap();
        let target = package.target("generated.txt").unwrap();
        let Target::GeneratedFile(generated) = target.target() else {
            panic!("expected generated file target");
        };
        assert_eq!(generated.generating_rule, "producer");
        assert_eq!(target.generating_rule().unwrap(), label("producer"));
        assert_eq!(package.generated_files().count(), 1);
    }

    #[test]
    fn builder_rejects_exact_name_collisions() {
        let mut builder =
            PackageBuilder::new(PackageId::new(MAIN_REPO, ""), "BUILD.bazel").unwrap();
        builder
            .export_source_file("exported.txt", Vec::new())
            .unwrap();
        assert!(
            builder
                .export_source_file("bad target", Vec::new())
                .is_err()
        );
        assert!(
            builder
                .instantiate_rule(
                    RuleClassSpec::Native(source_rule_class()),
                    explicit("exported.txt", []),
                )
                .is_err()
        );

        builder
            .instantiate_rule(
                RuleClassSpec::Native(source_rule_class()),
                explicit("duplicate", []),
            )
            .unwrap();
        assert!(
            builder
                .instantiate_rule(
                    RuleClassSpec::Native(source_rule_class()),
                    explicit("duplicate", []),
                )
                .is_err()
        );
    }

    #[test]
    fn failed_rule_does_not_leave_generated_targets() {
        let mut builder =
            PackageBuilder::new(PackageId::new(MAIN_REPO, ""), "BUILD.bazel").unwrap();
        let result = builder.instantiate_rule(
            RuleClassSpec::Native(output_rule_class()),
            explicit(
                "producer",
                [(
                    "outs",
                    DirectAttributeValue::OutputList(vec![
                        label("would_be_orphaned.txt"),
                        label("BUILD.bazel"),
                    ]),
                )],
            ),
        );
        assert!(result.is_err());
        assert!(!builder.targets.contains_key("would_be_orphaned.txt"));
    }

    #[test]
    fn mandatory_attributes_require_explicit_values_even_with_defaults() {
        static RULE_CLASS: LazyLock<RuleClass> = LazyLock::new(|| {
            let mut mandatory = AttributeSchema::new(
                "required",
                AttributeType::String,
                Some(DirectAttributeValue::String("default".to_owned())),
                AttributeDependency::NoDependency,
            );
            mandatory.mandatory = true;
            native_rule_class("mandatory_rule", vec![mandatory])
        });
        let mut builder =
            PackageBuilder::new(PackageId::new(MAIN_REPO, ""), "BUILD.bazel").unwrap();
        assert!(
            builder
                .instantiate_rule(RuleClassSpec::Native(&RULE_CLASS), explicit("missing", []),)
                .is_err()
        );
    }

    #[test]
    fn selector_conditions_and_none_defaults_are_dependencies() {
        let condition = label("condition");
        let fallback = label("fallback.txt");
        static RULE_CLASS: LazyLock<RuleClass> = LazyLock::new(|| {
            let scalar = AttributeSchema::new(
                "message",
                AttributeType::String,
                Some(DirectAttributeValue::String("default".to_owned())),
                AttributeDependency::NoDependency,
            );
            let fallback_schema = AttributeSchema::new(
                "fallback",
                AttributeType::Label,
                Some(DirectAttributeValue::Label(label("fallback.txt"))),
                AttributeDependency::Dependency,
            );
            native_rule_class("selector_rule", vec![scalar, fallback_schema])
        });
        let selector = |value| AttributeValue {
            parts: vec![crate::bazel::rule::AttributeValuePart::Selector(
                crate::bazel::rule::Selector {
                    entries: BTreeMap::from([(condition.clone(), value)]),
                    default: None,
                    no_match_error: String::new(),
                },
            )],
        };
        let mut attributes = explicit("selected", []);
        attributes.insert(
            "message",
            selector(Some(DirectAttributeValue::String("selected".to_owned()))),
        );
        attributes.insert("fallback", selector(None));

        let mut builder =
            PackageBuilder::new(PackageId::new(MAIN_REPO, ""), "BUILD.bazel").unwrap();
        builder
            .instantiate_rule(RuleClassSpec::Native(&RULE_CLASS), attributes)
            .unwrap();
        let package = builder.finish(&HashMap::new()).unwrap();
        assert!(matches!(
            package
                .target(condition.name())
                .map(|target| target.target()),
            Some(Target::SourceFile(_))
        ));
        assert!(matches!(
            package
                .target(fallback.name())
                .map(|target| target.target()),
            Some(Target::SourceFile(_))
        ));
    }
}
