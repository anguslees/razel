use crate::bazel::config::{
    BuildConfiguration, ConfigurationKey, ConfigurationKind, ConfigurationRequirements,
    ConfigurationValue,
};
use crate::bazel::label::{CanonicalLabel, MAIN_REPO_ROOT, Repo, TargetPatternKind};
use crate::bazel::rule::{
    AttributeDependency, AttributeTransition, AttributeValuePart, ConditionMatch,
    DirectAttributeValue, Rule, RuleAttribute,
};
use crate::bazel::target::Target;
use crate::query::QueryOutput;
use crate::query_syntax::{Expr, SetOp};
use crate::stream_tee::{StreamTee, StreamTeeExt};
use crate::workspace::{ExpandedTarget, ExpandedTargetKind, LoadedTarget, Workspace};
use futures::FutureExt;
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Debug)]
struct Configurations {
    target: Arc<BuildConfiguration>,
    exec: BuildConfiguration,
    target_constraints: BTreeSet<CanonicalLabel<'static>>,
    target_platform_explicit: bool,
}

impl Configurations {
    async fn new(
        workspace: &Arc<Workspace>,
        options: &crate::BuildOptions,
    ) -> anyhow::Result<Self> {
        let target_platform_explicit = options.platform.is_some();
        let mut values = BTreeMap::from([
            (
                ConfigurationKey::Native("cpu".into()),
                ConfigurationValue::String(options.cpu.clone()),
            ),
            (
                ConfigurationKey::Native("compilation_mode".into()),
                ConfigurationValue::String(options.compilation_mode.clone()),
            ),
            (
                ConfigurationKey::Native("force_pic".into()),
                ConfigurationValue::String(
                    options
                        .no_force_pic
                        .map_or(options.force_pic.unwrap_or(false), |disabled| !disabled)
                        .to_string(),
                ),
            ),
        ]);
        for (key, value) in &options.defines {
            values.insert(
                ConfigurationKey::Define(key.clone()),
                ConfigurationValue::String(value.clone()),
            );
        }
        let mut target_constraints = BTreeSet::new();

        if let Some(platform) = &options.platform {
            let label =
                crate::bazel::label::parse_label(platform, &MAIN_REPO_ROOT).map_err(|error| {
                    anyhow::anyhow!("invalid target platform `{platform}`: {error}")
                })?;
            let repository = workspace.main_repo().await?;
            let platform_label = repository
                .resolve_label(label)
                .ok_or_else(|| {
                    anyhow::anyhow!("unknown repository in target platform `{platform}`")
                })?
                .into_owned();
            let actual = resolve_direct_alias(workspace, &platform_label).await?;
            let loaded = workspace
                .load_target(&actual)
                .await?
                .ok_or_else(|| anyhow::anyhow!("target platform `{platform}` does not exist"))?;
            let Some(rule) = loaded.target.as_rule() else {
                anyhow::bail!("target platform `{platform}` is not a platform rule");
            };
            if rule.rule_class().native_name() != Some("platform") {
                anyhow::bail!("target platform `{platform}` is not a platform rule");
            }
            reject_explicit_attribute(rule, "parents", "platform inheritance")?;
            reject_explicit_attribute(rule, "flags", "platform flags")?;
            let mut constraints = BTreeSet::new();
            let mut settings = BTreeSet::new();
            for constraint in crate::bazel::config::label_list(rule, "constraint_values")? {
                let (constraint, setting) =
                    resolve_constraint_value(workspace, &constraint).await?;
                if !settings.insert(setting.clone()) {
                    anyhow::bail!(
                        "target platform `{platform}` contains multiple values for constraint setting `{setting}`"
                    );
                }
                constraints.insert(constraint);
            }
            target_constraints = constraints;
            values.insert(
                ConfigurationKey::Native("platforms".into()),
                ConfigurationValue::List(vec![ConfigurationValue::Label(platform_label)]),
            );
        }

        let target = Arc::new(BuildConfiguration::new(values));
        let exec = target.transition([
            (
                ConfigurationKey::Native("cpu".into()),
                Some(ConfigurationValue::String(options.host_cpu.clone())),
            ),
            (
                ConfigurationKey::Native("compilation_mode".into()),
                Some(ConfigurationValue::String(
                    options.host_compilation_mode.clone(),
                )),
            ),
            (
                ConfigurationKey::Native("force_pic".into()),
                Some(ConfigurationValue::String("false".into())),
            ),
            (
                ConfigurationKey::Native("is_exec_configuration".into()),
                Some(ConfigurationValue::String("true".into())),
            ),
            (ConfigurationKey::Native("platforms".into()), None),
        ]);

        Ok(Self {
            target,
            exec,
            target_constraints,
            target_platform_explicit,
        })
    }

    fn get(&self, kind: ConfigurationKind) -> Option<&BuildConfiguration> {
        match kind {
            ConfigurationKind::Target => Some(&self.target),
            ConfigurationKind::Exec => Some(&self.exec),
            ConfigurationKind::Null => None,
        }
    }

    fn display(&self, kind: ConfigurationKind) -> &str {
        match kind {
            ConfigurationKind::Target => self.target.checksum(),
            ConfigurationKind::Exec => self.exec.checksum(),
            ConfigurationKind::Null => "null",
        }
    }

    fn matches(&self, kind: ConfigurationKind, requirements: &ConfigurationRequirements) -> bool {
        self.get(kind).is_some_and(|configuration| {
            configuration.matches_options(requirements)
                && requirements.constraints.iter().all(|constraint| {
                    kind == ConfigurationKind::Target
                        && self.target_constraints.contains(constraint)
                })
        })
    }
}

#[derive(Debug, Clone)]
struct ConfiguredTarget {
    target: ExpandedTarget,
    configuration: ConfigurationKind,
}

impl ConfiguredTarget {
    fn new(loaded: LoadedTarget, requested: ConfigurationKind) -> Self {
        let configuration = match &loaded.target {
            Target::SourceFile(_) => ConfigurationKind::Null,
            Target::Rule(rule)
                if matches!(
                    rule.rule_class().native_name(),
                    Some("constraint_setting" | "constraint_value")
                ) =>
            {
                ConfigurationKind::Exec
            }
            Target::Rule(_) | Target::GeneratedFile(_) => requested,
        };
        Self {
            target: loaded.expanded,
            configuration,
        }
    }

    fn key(&self) -> (CanonicalLabel<'static>, ConfigurationKind) {
        (self.target.label.clone(), self.configuration)
    }
}

type CqueryResult = Result<ConfiguredTarget, String>;
type CqueryStream<'a> = BoxStream<'a, CqueryResult>;

/// Borrowed state for configured analysis. It holds no `let` bindings, so long-lived traversal
/// streams can copy it without capturing binding replay buffers.
#[derive(Clone, Copy)]
struct Analysis<'a> {
    workspace: &'a Arc<Workspace>,
    configurations: &'a Configurations,
    universe: &'a [&'a str],
}

#[derive(Clone)]
struct CqueryContext<'a> {
    analysis: Analysis<'a>,
    /// Each binding is evaluated once and replayed to every reference. A tee buffers only
    /// results that a lagging reference has not consumed yet.
    variables: Vec<(&'a str, StreamTee<CqueryStream<'a>>)>,
}

impl<'a> CqueryContext<'a> {
    fn eval(&self, expr: &'a Expr<'a>) -> CqueryStream<'a> {
        match expr {
            &Expr::String(pattern) => {
                match crate::bazel::label::parse_target_pattern(pattern, &MAIN_REPO_ROOT) {
                    Ok(pattern) => {
                        let mut targets = self.analysis.universe();
                        let workspace = self.analysis.workspace;
                        async_stream::stream! {
                            let repository = match &pattern.repo {
                                Repo::Canonical(repo) => repo.clone().into_owned(),
                                Repo::Apparent(apparent) => {
                                    let main = match workspace.main_repo().await {
                                        Ok(main) => main,
                                        Err(error) => {
                                            yield Err(error.to_string());
                                            return;
                                        }
                                    };
                                    let Some(repo) = main.resolve_repo(apparent) else {
                                        yield Err(format!("Unknown apparent repository `{apparent}`"));
                                        return;
                                    };
                                    repo.into_owned()
                                }
                            };
                            while let Some(result) = targets.next().await {
                                match result {
                                    Ok(target) => {
                                        let label = &target.target.label;
                                        let package_matches = label.package() == pattern.package
                                            || (pattern.include_subpackages && (
                                                pattern.package.is_empty()
                                                || label.package().strip_prefix(pattern.package.as_ref())
                                                    .is_some_and(|suffix| suffix.starts_with('/'))
                                            ));
                                        let kind_matches = match &pattern.target_kind {
                                            TargetPatternKind::Exact(name) => label.name() == name,
                                            TargetPatternKind::AllRules => matches!(target.target.kind, ExpandedTargetKind::Rule(_)),
                                            TargetPatternKind::AllTargets => true,
                                        };
                                        if label.repo == repository && package_matches && kind_matches {
                                            yield Ok(target);
                                        }
                                    }
                                    Err(error) => yield Err(error.to_string()),
                                }
                            }
                        }
                        .boxed()
                    }
                    Err(error) => stream::once(async move { Err(error.to_string()) }).boxed(),
                }
            }
            Expr::Function("deps", args) => self.eval_deps(args),
            Expr::Function("config", args) => self.eval_config(args),
            Expr::Function(name, _) => {
                let error = format!("Function {name} is not supported by cquery");
                stream::once(async move { Err(error) }).boxed()
            }
            Expr::Let(name, value, body) => {
                let value = self.eval(&value.inner).tee();
                let mut nested = self.clone();
                nested.variables.push((name, value));
                nested.eval(&body.inner)
            }
            Expr::Variable(name) => {
                match self.variables.iter().rev().find(|(bound, _)| bound == name) {
                    Some((_, value)) => value.clone().boxed(),
                    None => {
                        let error = format!("Undefined variable {name}");
                        stream::once(async move { Err(error) }).boxed()
                    }
                }
            }
            Expr::SetOp(SetOp::Union, left, right) => {
                let left = self.eval(&left.inner);
                let right = self.eval(&right.inner);
                async_stream::stream! {
                    let mut seen = HashSet::new();
                    for mut values in [left, right] {
                        while let Some(result) = values.next().await {
                            match result {
                                Ok(target) if seen.insert(target.key()) => yield Ok(target),
                                Ok(_) => {}
                                Err(error) => yield Err(error),
                            }
                        }
                    }
                }
                .boxed()
            }
            Expr::SetOp(SetOp::Intersect, left, right) => {
                eval_set_filter(self.eval(&left.inner), self.eval(&right.inner), true)
            }
            Expr::SetOp(SetOp::Difference, left, right) => {
                eval_set_filter(self.eval(&left.inner), self.eval(&right.inner), false)
            }
        }
    }

    fn eval_deps(&self, args: &'a [chumsky::span::Spanned<Expr<'a>>]) -> CqueryStream<'a> {
        if !(1..=2).contains(&args.len()) {
            return stream::once(async { Err("deps() expects one or two arguments".to_owned()) })
                .boxed();
        }
        let max_depth = match args.get(1).map(|argument| &argument.inner) {
            None => None,
            Some(Expr::String(depth)) => match depth.parse::<usize>() {
                Ok(depth) => Some(depth),
                Err(_) => {
                    return stream::once(async {
                        Err("deps() depth must be a non-negative integer".to_owned())
                    })
                    .boxed();
                }
            },
            Some(_) => {
                return stream::once(async {
                    Err("deps() depth must be a non-negative integer".to_owned())
                })
                .boxed();
            }
        };
        // Bazel's cquery also yields deps() breadth first. Analysis errors and cycles in the
        // closure surface first, from the universe traversal behind the roots' patterns.
        let analysis = self.analysis;
        crate::query::breadth_first(
            self.eval(&args[0].inner),
            max_depth,
            ConfiguredTarget::key,
            move |target| async move { analysis.dependencies(&target).await }.boxed(),
        )
    }

    fn eval_config(&self, args: &'a [chumsky::span::Spanned<Expr<'a>>]) -> CqueryStream<'a> {
        if args.len() != 2 {
            return stream::once(async { Err("config() expects two arguments".to_owned()) })
                .boxed();
        }
        let selector = match &args[1].inner {
            Expr::String(selector) => *selector,
            _ => {
                return stream::once(async {
                    Err("config() configuration must be target, null, or a checksum".to_owned())
                })
                .boxed();
            }
        };
        let configurations = self.analysis.configurations;
        let mut values = self.eval(&args[0].inner);
        let mut universe = self.analysis.universe();
        async_stream::stream! {
            let mut requested = match selector {
                "target" => Some(ConfigurationKind::Target),
                "null" => Some(ConfigurationKind::Null),
                _ => None,
            };
            let mut labels = HashSet::new();
            while let Some(result) = values.next().await {
                match result {
                    Ok(target) => { labels.insert(target.target.label); }
                    Err(error) => yield Err(error),
                }
            }
            let mut found = false;
            while let Some(result) = universe.next().await {
                match result {
                    Ok(target) => {
                        let matches = match selector {
                            "target" => target.configuration == ConfigurationKind::Target,
                            "null" => target.configuration == ConfigurationKind::Null,
                            prefix => target.configuration != ConfigurationKind::Null
                                && configurations.display(target.configuration).starts_with(prefix),
                        };
                        if matches {
                            if requested.is_some_and(|kind| kind != target.configuration) {
                                yield Err(format!("configuration prefix `{selector}` is ambiguous"));
                                return;
                            }
                            requested = Some(target.configuration);
                            if labels.remove(&target.target.label) {
                                found = true;
                                yield Ok(target);
                            }
                        }
                    }
                    Err(error) => yield Err(error),
                }
            }
            if requested.is_none() {
                yield Err(format!("unknown configuration `{selector}`"));
            } else if !found {
                yield Err(format!("no targets found in configuration `{selector}`"));
            }
        }
        .boxed()
    }
}

impl<'a> Analysis<'a> {
    fn universe(&self) -> CqueryStream<'a> {
        let analysis = *self;
        let roots = async_stream::stream! {
            for pattern in analysis.universe {
                let pattern = match crate::bazel::label::parse_target_pattern(pattern, &MAIN_REPO_ROOT) {
                    Ok(pattern) => pattern,
                    Err(error) => {
                        yield Err(error.to_string());
                        return;
                    }
                };
                let mut expanded = analysis.workspace.clone().expand_pattern(pattern);
                while let Some(target) = expanded.next().await {
                    yield match target {
                        // The expander still holds this package, so the lookup reuses it.
                        Ok(target) => analysis
                            .required_target(&target.label)
                            .await
                            .map(|loaded| ConfiguredTarget::new(loaded, ConfigurationKind::Target))
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error.to_string()),
                    };
                }
            }
        }.boxed();
        self.walk(roots)
    }

    /// Walks the configured closure of `roots` depth first, failing on cycles. Each target is
    /// resolved before it is yielded; only traversal keys and the active frontier are retained.
    fn walk(&self, mut roots: CqueryStream<'a>) -> CqueryStream<'a> {
        let analysis = *self;
        async_stream::stream! {
            let mut visited = HashSet::new();
            let mut active = HashSet::new();
            while let Some(root) = roots.next().await {
                let root = match root {
                    Ok(root) => root,
                    Err(error) => { yield Err(error); return; }
                };
                let mut pending = vec![(root, false)];
                while let Some((target, exiting)) = pending.pop() {
                    let key = target.key();
                    if exiting {
                        active.remove(&key);
                        continue;
                    }
                    if active.contains(&key) {
                        yield Err(format!("configured dependency cycle detected at `{}`", key.0));
                        return;
                    }
                    if !visited.insert(key.clone()) {
                        continue;
                    }
                    let dependencies = match analysis.dependencies(&target).await {
                        Ok(dependencies) => dependencies,
                        Err(error) => { yield Err(error.to_string()); return; }
                    };
                    yield Ok(target.clone());
                    active.insert(key);
                    pending.push((target, true));
                    pending.extend(dependencies.into_iter().rev().map(|dependency| (dependency, false)));
                }
            }
        }.boxed()
    }

    async fn dependencies(
        &self,
        target: &ConfiguredTarget,
    ) -> anyhow::Result<Vec<ConfiguredTarget>> {
        let loaded = self
            .workspace
            .load_target(&target.target.label)
            .await?
            .ok_or_else(|| anyhow::anyhow!("target `{}` does not exist", target.target.label))?;
        let mut dependencies = Vec::new();
        // Dependencies are loaded and configured once, after collection.
        let mut depend_on = |label: &CanonicalLabel<'static>, configuration| {
            dependencies.push(ConfiguredTarget {
                target: ExpandedTarget {
                    label: label.clone(),
                    kind: ExpandedTargetKind::SourceFile,
                },
                configuration,
            });
        };
        match &loaded.target {
            Target::Rule(rule) => {
                let attributes = rule
                    .rule_class()
                    .attributes()
                    .iter()
                    .zip(rule.attributes.iter())
                    .filter_map(|(schema, attribute)| Some((schema, attribute.as_ref()?)));
                // As in Bazel, every select() key is analyzed before compatibility is known.
                let mut condition_matches = BTreeMap::new();
                for (schema, attribute) in attributes.clone() {
                    for part in &attribute.value.parts {
                        let AttributeValuePart::Selector(selector) = part else {
                            continue;
                        };
                        for condition in selector.entries.keys() {
                            if !condition_matches.contains_key(condition) {
                                let matched = self
                                    .match_condition(condition, target.configuration)
                                    .await?;
                                condition_matches.insert(condition.clone(), matched);
                            }
                            if schema.selector_conditions_are_dependencies {
                                depend_on(condition, target.configuration);
                            }
                        }
                    }
                }
                let resolve = |schema, attribute: &RuleAttribute| {
                    attribute.value.resolve(schema, |condition| {
                        Ok(condition_matches.get(condition).cloned().flatten())
                    })
                };

                let mut incompatible = false;
                if target.configuration == ConfigurationKind::Target
                    && self.configurations.target_platform_explicit
                    && let Some((schema, attribute)) = attributes
                        .clone()
                        .find(|(schema, _)| schema.name == "target_compatible_with")
                {
                    let mut constraints = Vec::new();
                    resolve(schema, attribute)?
                        .visit_labels(|label| constraints.push(label.clone()));
                    for constraint in &constraints {
                        let (resolved, _) =
                            resolve_constraint_value(self.workspace, constraint).await?;
                        let requirements = ConfigurationRequirements::for_constraint(resolved);
                        incompatible |= !self
                            .configurations
                            .matches(ConfigurationKind::Target, &requirements);
                    }
                    if incompatible {
                        // Bazel skips a directly incompatible target without resolving its other
                        // attributes, so only its select() keys and compatibility constraints
                        // remain dependencies. cquery reports the target instead of failing.
                        for constraint in &constraints {
                            depend_on(constraint, target.configuration);
                        }
                    }
                }

                if !incompatible {
                    self.validate_rule(rule, &target.target.label).await?;
                    for (schema, attribute) in attributes {
                        let resolved = resolve(schema, attribute)?;
                        if schema.dependency == AttributeDependency::Dependency {
                            let dependency_configuration = match schema.transition {
                                AttributeTransition::None => target.configuration,
                                AttributeTransition::Exec => ConfigurationKind::Exec,
                            };
                            resolved
                                .visit_labels(|label| depend_on(label, dependency_configuration));
                        }
                    }
                }
            }
            Target::GeneratedFile(generated) => depend_on(
                &CanonicalLabel::new(
                    target.target.label.repo.clone(),
                    target.target.label.package().to_owned(),
                    generated.generating_rule.clone(),
                ),
                target.configuration,
            ),
            Target::SourceFile(_) => {}
        }

        for dependency in &mut dependencies {
            let loaded = self.required_target(&dependency.target.label).await?;
            *dependency = ConfiguredTarget::new(loaded, dependency.configuration);
        }
        dependencies.sort_by(|left, right| {
            (&left.target.label, left.configuration)
                .cmp(&(&right.target.label, right.configuration))
        });
        dependencies.dedup_by(|left, right| {
            left.target.label == right.target.label && left.configuration == right.configuration
        });
        Ok(dependencies)
    }

    async fn validate_rule(
        &self,
        rule: &Rule,
        label: &CanonicalLabel<'static>,
    ) -> anyhow::Result<()> {
        match rule.rule_class().native_name() {
            Some("config_setting") => {
                let requirements = ConfigurationRequirements::from_rule(rule)?;
                let mut settings = BTreeSet::new();
                for constraint in &requirements.constraints {
                    let (_, setting) = resolve_constraint_value(self.workspace, constraint).await?;
                    if !settings.insert(setting.clone()) {
                        anyhow::bail!(
                            "config_setting `{label}` contains multiple values for constraint setting `{setting}`"
                        );
                    }
                }
            }
            Some("constraint_setting") => {
                reject_explicit_attribute(
                    rule,
                    "default_constraint_value",
                    "default constraint values",
                )?;
                reject_explicit_attribute(
                    rule,
                    "refines_constraint_value",
                    "constraint refinement",
                )?;
            }
            Some("constraint_value") => {
                resolve_constraint_value(self.workspace, label).await?;
            }
            Some("platform") => {
                reject_explicit_attribute(rule, "parents", "platform inheritance")?;
                reject_explicit_attribute(rule, "flags", "platform flags")?;
                let mut settings = BTreeSet::new();
                for constraint in crate::bazel::config::label_list(rule, "constraint_values")? {
                    let (_, setting) =
                        resolve_constraint_value(self.workspace, &constraint).await?;
                    if !settings.insert(setting.clone()) {
                        anyhow::bail!(
                            "platform `{label}` contains multiple values for constraint setting `{setting}`"
                        );
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn required_target(&self, label: &CanonicalLabel<'_>) -> anyhow::Result<LoadedTarget> {
        self.workspace
            .load_target(label)
            .await?
            .ok_or_else(|| anyhow::anyhow!("target `{label}` does not exist"))
    }

    async fn match_condition(
        &self,
        label: &CanonicalLabel<'static>,
        configuration: ConfigurationKind,
    ) -> anyhow::Result<Option<ConditionMatch>> {
        self.match_condition_inner(label, configuration, &mut BTreeSet::new())
            .await
    }

    #[async_recursion::async_recursion]
    async fn match_condition_inner(
        &self,
        label: &CanonicalLabel<'static>,
        configuration: ConfigurationKind,
        aliases: &mut BTreeSet<CanonicalLabel<'static>>,
    ) -> anyhow::Result<Option<ConditionMatch>> {
        let loaded = self
            .workspace
            .load_target(label)
            .await?
            .ok_or_else(|| anyhow::anyhow!("select condition `{label}` does not exist"))?;
        let Some(rule) = loaded.target.as_rule() else {
            anyhow::bail!("select condition `{label}` is not a config_setting or constraint_value");
        };
        let requirements = match rule.rule_class().native_name() {
            Some("alias") => {
                if !aliases.insert(label.clone()) {
                    anyhow::bail!("alias cycle detected at select condition `{label}`");
                }
                let schema = rule
                    .rule_class()
                    .attributes()
                    .iter()
                    .find(|schema| schema.name == "actual")
                    .ok_or_else(|| {
                        anyhow::anyhow!("alias select condition `{label}` has no actual attribute")
                    })?;
                let attribute = rule.attribute("actual").ok_or_else(|| {
                    anyhow::anyhow!("alias select condition `{label}` has no actual target")
                })?;
                let mut conditions = BTreeSet::new();
                for part in &attribute.parts {
                    if let AttributeValuePart::Selector(selector) = part {
                        conditions.extend(selector.entries.keys().cloned());
                    }
                }
                let mut condition_matches = BTreeMap::new();
                for condition in conditions {
                    let mut condition_aliases = aliases.clone();
                    condition_matches.insert(
                        condition.clone(),
                        self.match_condition_inner(
                            &condition,
                            configuration,
                            &mut condition_aliases,
                        )
                        .await?,
                    );
                }
                let actual = attribute.resolve(schema, |condition| {
                    Ok(condition_matches.get(condition).cloned().flatten())
                })?;
                let DirectAttributeValue::Label(actual) = actual else {
                    anyhow::bail!("alias select condition `{label}` has an invalid actual target");
                };
                return self
                    .match_condition_inner(&actual, configuration, aliases)
                    .await;
            }
            Some("config_setting") => {
                let mut requirements = ConfigurationRequirements::from_rule(rule)?;
                if let Some(platform) = requirements.values.get_mut("platforms") {
                    let repository = self.workspace.main_repo().await?;
                    let parsed = crate::bazel::label::parse_label(platform, &MAIN_REPO_ROOT)
                        .map_err(|error| anyhow::anyhow!("invalid platforms condition: {error}"))?;
                    *platform = repository
                        .resolve_label(parsed)
                        .ok_or_else(|| {
                            anyhow::anyhow!("unknown repository in platforms condition")
                        })?
                        .to_string();
                }
                let mut constraints = BTreeSet::new();
                let mut settings = BTreeSet::new();
                for constraint in requirements.constraints {
                    let (constraint, setting) =
                        resolve_constraint_value(self.workspace, &constraint).await?;
                    if !settings.insert(setting.clone()) {
                        anyhow::bail!(
                            "config_setting `{label}` contains multiple values for constraint setting `{setting}`"
                        );
                    }
                    constraints.insert(constraint);
                }
                requirements.constraints = constraints;
                requirements
            }
            Some("constraint_value") => {
                let (constraint, _) = resolve_constraint_value(self.workspace, label).await?;
                ConfigurationRequirements::for_constraint(constraint)
            }
            _ => anyhow::bail!(
                "select condition `{label}` is not a config_setting or constraint_value"
            ),
        };
        Ok(self
            .configurations
            .matches(configuration, &requirements)
            .then_some(ConditionMatch { requirements }))
    }
}

fn collect_target_patterns<'a>(expr: &Expr<'a>, patterns: &mut Vec<&'a str>) {
    match expr {
        Expr::String(pattern) => patterns.push(pattern),
        Expr::Function("deps" | "config", args) => {
            if let Some(argument) = args.first() {
                collect_target_patterns(&argument.inner, patterns);
            }
        }
        Expr::Function(_, _) | Expr::Variable(_) => {}
        Expr::Let(_, value, body) => {
            collect_target_patterns(&value.inner, patterns);
            collect_target_patterns(&body.inner, patterns);
        }
        Expr::SetOp(_, left, right) => {
            collect_target_patterns(&left.inner, patterns);
            collect_target_patterns(&right.inner, patterns);
        }
    }
}

fn reject_explicit_attribute(rule: &Rule, attribute: &str, feature: &str) -> anyhow::Result<()> {
    if rule.is_attribute_explicit(attribute) {
        anyhow::bail!("{feature} are not supported by cquery yet");
    }
    Ok(())
}

async fn resolve_direct_alias(
    workspace: &Arc<Workspace>,
    label: &CanonicalLabel<'static>,
) -> anyhow::Result<CanonicalLabel<'static>> {
    let mut label = label.clone();
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(label.clone()) {
            anyhow::bail!("alias cycle detected at `{label}`");
        }
        let loaded = workspace
            .load_target(&label)
            .await?
            .ok_or_else(|| anyhow::anyhow!("target `{label}` does not exist"))?;
        let Some(rule) = loaded.target.as_rule() else {
            return Ok(label);
        };
        if rule.rule_class().native_name() != Some("alias") {
            return Ok(label);
        }
        let actual = rule
            .attribute("actual")
            .and_then(|attribute| attribute.direct_value())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "configurable aliases in platform and constraint attributes are not supported yet"
                )
            })?;
        let DirectAttributeValue::Label(actual) = actual else {
            anyhow::bail!("alias `{label}` has an invalid actual target");
        };
        label = actual.clone();
    }
}

async fn resolve_constraint_value(
    workspace: &Arc<Workspace>,
    label: &CanonicalLabel<'static>,
) -> anyhow::Result<(CanonicalLabel<'static>, CanonicalLabel<'static>)> {
    let label = resolve_direct_alias(workspace, label).await?;
    let loaded = workspace
        .load_target(&label)
        .await?
        .ok_or_else(|| anyhow::anyhow!("constraint value `{label}` does not exist"))?;
    let Some(rule) = loaded.target.as_rule() else {
        anyhow::bail!("target `{label}` is not a constraint_value");
    };
    if rule.rule_class().native_name() != Some("constraint_value") {
        anyhow::bail!("target `{label}` is not a constraint_value");
    }
    let setting = rule
        .attribute("constraint_setting")
        .and_then(|attribute| attribute.direct_value())
        .ok_or_else(|| anyhow::anyhow!("constraint_value `{label}` has no constraint_setting"))?;
    let DirectAttributeValue::Label(setting) = setting else {
        anyhow::bail!("constraint_value `{label}` has an invalid constraint_setting");
    };
    let setting = resolve_direct_alias(workspace, setting).await?;
    let loaded_setting = workspace
        .load_target(&setting)
        .await?
        .ok_or_else(|| anyhow::anyhow!("constraint setting `{setting}` does not exist"))?;
    let Some(setting_rule) = loaded_setting.target.as_rule() else {
        anyhow::bail!("target `{setting}` is not a constraint_setting");
    };
    if setting_rule.rule_class().native_name() != Some("constraint_setting") {
        anyhow::bail!("target `{setting}` is not a constraint_setting");
    }
    reject_explicit_attribute(
        setting_rule,
        "default_constraint_value",
        "default constraint values",
    )?;
    reject_explicit_attribute(
        setting_rule,
        "refines_constraint_value",
        "constraint refinement",
    )?;
    Ok((label, setting))
}

fn eval_set_filter<'a>(
    mut left: CqueryStream<'a>,
    mut right: CqueryStream<'a>,
    include_matches: bool,
) -> CqueryStream<'a> {
    async_stream::stream! {
        let mut right_keys = HashSet::new();
        while let Some(result) = right.next().await {
            match result {
                Ok(target) => {
                    right_keys.insert(target.key());
                }
                Err(error) => yield Err(error),
            }
        }
        let mut seen = HashSet::new();
        while let Some(result) = left.next().await {
            match result {
                Ok(target)
                    if right_keys.contains(&target.key()) == include_matches
                        && seen.insert(target.key()) =>
                {
                    yield Ok(target);
                }
                Ok(_) => {}
                Err(error) => yield Err(error),
            }
        }
    }
    .boxed()
}

/// Streams results in evaluation order, which Bazel's cquery, having no `--order_output`, also
/// prints: patterns in the order the universe is traversed, and `deps()` breadth first.
pub async fn cquery<W>(
    out: &mut W,
    invocation_options: Arc<crate::bazel::InvocationOptions>,
    options: &crate::BuildOptions,
    output: QueryOutput,
    query: &str,
) -> Result<(), crate::query::QueryError>
where
    W: AsyncWrite + Unpin,
{
    let ast = crate::query_syntax::parse(query)?;
    let workspace = Workspace::new(".", invocation_options)
        .await
        .map_err(crate::query::workspace_error)?;
    let configurations = Configurations::new(&workspace, options).await?;
    let mut universe = Vec::new();
    collect_target_patterns(&ast.inner, &mut universe);
    let context = CqueryContext {
        analysis: Analysis {
            workspace: &workspace,
            configurations: &configurations,
            universe: &universe,
        },
        variables: Vec::new(),
    };
    let mut result = context.eval(&ast.inner);
    while let Some(target) = result.next().await {
        let target = target.map_err(|error| anyhow::anyhow!("Cquery evaluation error: {error}"))?;
        if output == QueryOutput::LabelKind {
            match &target.target.kind {
                ExpandedTargetKind::Rule(rule_class) => {
                    out.write_all(rule_class.as_bytes()).await?;
                    out.write_all(b" rule ").await?;
                }
                ExpandedTargetKind::SourceFile => out.write_all(b"source file ").await?,
                ExpandedTargetKind::GeneratedFile => out.write_all(b"generated file ").await?,
            }
        }
        crate::query::write_label(out, &target.target.label).await?;
        out.write_all(b" (").await?;
        out.write_all(configurations.display(target.configuration).as_bytes())
            .await?;
        out.write_all(b")\n").await?;
        out.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn universe_loading_is_lazy_and_cancellable() {
        let root = assert_fs::TempDir::new().unwrap();
        std::fs::write(
            root.path().join("MODULE.bazel"),
            "module(name = 'streaming')",
        )
        .unwrap();
        for (package, source) in [
            ("a", "filegroup(name = 'early')"),
            ("z", "fail('late package error')"),
        ] {
            let path = root.path().join(package);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("BUILD.bazel"), source).unwrap();
        }
        let workspace = Workspace::new(
            root.path(),
            Arc::new(crate::bazel::InvocationOptions {
                ignore_dev_dependency: false,
            }),
        )
        .await
        .unwrap();
        let target = Arc::new(BuildConfiguration::new([]));
        let configurations = Configurations {
            exec: target.transition([]),
            target,
            target_constraints: BTreeSet::new(),
            target_platform_explicit: false,
        };
        let analysis = Analysis {
            workspace: &workspace,
            configurations: &configurations,
            universe: &["//..."],
        };
        let mut targets = analysis.universe();
        let first = targets.next().await.unwrap().unwrap();
        assert_eq!(first.target.label.package(), "a");
        assert_eq!(first.target.label.name(), "early");
        drop(targets);

        // The later package was never evaluated, including after cancellation.
        std::fs::write(
            root.path().join("z/BUILD.bazel"),
            "filegroup(name = 'late')",
        )
        .unwrap();
        let mut targets = analysis.universe();
        assert_eq!(
            targets.next().await.unwrap().unwrap().target.label.name(),
            "early"
        );
        assert_eq!(
            targets.next().await.unwrap().unwrap().target.label.name(),
            "late"
        );
        assert!(targets.next().await.is_none());
    }
}
