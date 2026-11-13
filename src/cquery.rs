use crate::bazel::config::{
    BuildConfiguration, ConfigurationKey, ConfigurationKind, ConfigurationRequirements,
    ConfigurationValue,
};
use crate::bazel::label::{CanonicalLabel, MAIN_REPO_ROOT, Repo, TargetPatternKind};
use crate::bazel::rule::{
    AttributeDependency, AttributeSchema, AttributeTransition, AttributeValue, AttributeValuePart,
    ConditionMatch, DirectAttributeValue, Rule,
};
use crate::bazel::target::Target;
use crate::query::QueryOutput;
use crate::query_syntax::{Expr, SetOp};
use crate::stream_tee::{StreamTee, StreamTeeExt};
use crate::workspace::{ExpandedTarget, ExpandedTargetKind, LoadedTarget, Workspace};
use futures::FutureExt;
use futures::future::join_all;
use futures::stream::{self, BoxStream, StreamExt};
use std::borrow::Cow;
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
                ConfigurationKey::Define(key.clone().into()),
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
            target_constraints = resolve_constraint_values(
                workspace,
                crate::bazel::config::label_list(rule, "constraint_values")?,
                || format!("target platform `{platform}`"),
            )
            .await?;
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

    fn matches(
        &self,
        kind: ConfigurationKind,
        requirements: &ConfigurationRequirements<'_>,
    ) -> bool {
        self.get(kind).is_some_and(|configuration| {
            configuration.matches_options(requirements)
                && requirements.constraints.iter().all(|constraint| {
                    kind == ConfigurationKind::Target
                        && self.target_constraints.contains(constraint)
                })
        })
    }

    /// Matches `conditions` against configuration `kind`. The matches borrow the conditions.
    fn match_conditions<'c>(
        &self,
        kind: ConfigurationKind,
        conditions: &'c BTreeMap<CanonicalLabel<'_>, Condition>,
    ) -> anyhow::Result<BTreeMap<CanonicalLabel<'c>, Option<ConditionMatch<'c>>>> {
        conditions
            .iter()
            .map(|(label, condition)| {
                let requirements = condition.requirements()?;
                let matched = self.matches(kind, &requirements);
                Ok((
                    label.as_borrowed(),
                    matched.then_some(ConditionMatch { requirements }),
                ))
            })
            .collect()
    }
}

/// The target of a select() key after following aliases: a `config_setting` or
/// `constraint_value`. Its requirements borrow from it, so it stays loaded while they are used.
struct Condition {
    target: LoadedTarget,
    /// The `values` entry for `platforms`, as the platform's canonical label.
    platform: Option<String>,
    /// The constraint values, resolved through aliases.
    constraints: BTreeSet<CanonicalLabel<'static>>,
}

impl Condition {
    fn requirements(&self) -> anyhow::Result<ConfigurationRequirements<'_>> {
        let mut requirements = match self.target.target.as_rule() {
            Some(rule) if rule.rule_class().native_name() == Some("config_setting") => {
                ConfigurationRequirements::from_rule(rule)?
            }
            _ => ConfigurationRequirements::default(),
        };
        if let Some(platform) = &self.platform {
            requirements.values.insert("platforms", platform);
        }
        requirements.constraints = self
            .constraints
            .iter()
            .map(CanonicalLabel::as_borrowed)
            .collect();
        Ok(requirements)
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
/// The configured targets of the universe, shared rather than copied between its readers.
type UniverseStream<'a> = BoxStream<'a, Result<Arc<ConfiguredTarget>, String>>;

#[cfg(test)]
thread_local! {
    /// How many configured targets have been analyzed on this thread, so tests can check that
    /// analysis is not repeated.
    static ANALYZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
    /// The universe, traversed and analyzed once however many patterns read it. Each reader has
    /// its own cursor; results stay buffered only until every reader has seen them.
    universe: StreamTee<UniverseStream<'a>>,
    /// Each binding is evaluated once and replayed to every reference. A tee buffers only
    /// results that a lagging reference has not consumed yet.
    variables: Vec<(&'a str, StreamTee<CqueryStream<'a>>)>,
}

impl<'a> CqueryContext<'a> {
    /// A context whose universe is not traversed until a pattern reads it. Every reader must be
    /// created before reading starts, as a new reader starts where this context's reader is.
    fn new(analysis: Analysis<'a>) -> Self {
        Self {
            analysis,
            universe: analysis
                .walk_universe()
                .map(|result| result.map(Arc::new))
                .boxed()
                .tee(),
            variables: Vec::new(),
        }
    }

    fn eval(&self, expr: &'a Expr<'a>) -> CqueryStream<'a> {
        match expr {
            &Expr::String(pattern) => {
                match crate::bazel::label::parse_target_pattern(pattern, &MAIN_REPO_ROOT) {
                    Ok(pattern) => {
                        let mut targets = self.universe.clone();
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
                                            // Copy only the targets that match.
                                            yield Ok(Arc::unwrap_or_clone(target));
                                        }
                                    }
                                    Err(error) => yield Err(error),
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
        let mut universe = self.universe.clone();
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
                                yield Ok(Arc::unwrap_or_clone(target));
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
    /// Traverses and analyzes the configured closure of the universe's patterns.
    fn walk_universe(&self) -> CqueryStream<'a> {
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
                // The expander fails for an exact pattern whose target does not exist.
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
        #[cfg(test)]
        ANALYZED.set(ANALYZED.get() + 1);
        let loaded = self.required_target(&target.target.label).await?;
        let configuration = target.configuration;
        // The dependencies to load and their configurations. Labels borrow from the target where
        // possible.
        let mut requests = Vec::new();
        let mut validated_rule = None;
        match &loaded.target {
            Target::Rule(rule) => {
                let attributes = rule
                    .rule_class()
                    .attributes()
                    .iter()
                    .zip(rule.attributes.iter())
                    .filter_map(|(schema, attribute)| Some((schema, attribute.as_ref()?)));
                // As in Bazel, every select() key is analyzed before compatibility is known.
                let mut condition_labels = BTreeSet::new();
                for (schema, attribute) in attributes.clone() {
                    for part in &attribute.value.parts {
                        let AttributeValuePart::Selector(selector) = part else {
                            continue;
                        };
                        for condition in selector.entries.keys() {
                            condition_labels.insert(condition.as_borrowed());
                            if schema.selector_conditions_are_dependencies {
                                requests.push((condition.as_borrowed(), configuration));
                            }
                        }
                    }
                }
                let conditions = self
                    .load_conditions(condition_labels, configuration, &BTreeSet::new())
                    .await?;
                let matches = self
                    .configurations
                    .match_conditions(configuration, &conditions)?;

                let mut incompatible = false;
                if configuration == ConfigurationKind::Target
                    && self.configurations.target_platform_explicit
                    && let Some((schema, attribute)) = attributes
                        .clone()
                        .find(|(schema, _)| schema.name == "target_compatible_with")
                {
                    let mut constraints = Vec::new();
                    visit_resolved_labels(
                        resolve_attribute(schema, &attribute.value, &matches)?,
                        |label| constraints.push(label),
                    );
                    let resolved =
                        join_all(constraints.iter().map(|constraint| {
                            resolve_constraint_value(self.workspace, constraint)
                        }))
                        .await;
                    for result in resolved {
                        let (constraint, _) = result?;
                        let requirements = ConfigurationRequirements::for_constraint(constraint);
                        incompatible |= !self
                            .configurations
                            .matches(ConfigurationKind::Target, &requirements);
                    }
                    if incompatible {
                        // Bazel skips a directly incompatible target without resolving its other
                        // attributes, so only its select() keys and compatibility constraints
                        // remain dependencies. cquery reports the target instead of failing.
                        requests.extend(
                            constraints
                                .into_iter()
                                .map(|constraint| (constraint, configuration)),
                        );
                    }
                }

                if !incompatible {
                    validated_rule = Some(rule);
                    for (schema, attribute) in attributes {
                        let resolved = resolve_attribute(schema, &attribute.value, &matches)?;
                        if schema.dependency == AttributeDependency::Dependency {
                            let dependency_configuration = match schema.transition {
                                AttributeTransition::Target => configuration,
                                AttributeTransition::Exec => ConfigurationKind::Exec,
                            };
                            visit_resolved_labels(resolved, |label| {
                                requests.push((label, dependency_configuration));
                            });
                        }
                    }
                }
            }
            Target::GeneratedFile(generated) => requests.push((
                target
                    .target
                    .label
                    .as_borrowed()
                    .same_package_label(generated.generating_rule.as_str()),
                configuration,
            )),
            Target::SourceFile(_) => {}
        }

        requests.sort_unstable();
        requests.dedup();
        // Validate the rule while its dependencies load, and load them concurrently.
        let validation = async {
            match validated_rule {
                Some(rule) => self.validate_rule(rule, &target.target.label).await,
                None => Ok(()),
            }
        };
        let loading = join_all(
            requests
                .iter()
                .map(|(label, _)| self.required_target(label)),
        );
        let (validation, loaded) = futures::join!(validation, loading);
        validation?;
        let mut dependencies = requests
            .iter()
            .zip(loaded)
            .map(|((_, configuration), loaded)| Ok(ConfiguredTarget::new(loaded?, *configuration)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        // Source files and constraints ignore the requested configuration, so different requests
        // can give the same configured target. Requests are sorted by label, so these are adjacent.
        dependencies.dedup_by(|left, right| {
            left.target.label == right.target.label && left.configuration == right.configuration
        });
        Ok(dependencies)
    }

    async fn validate_rule(&self, rule: &Rule, label: &CanonicalLabel<'_>) -> anyhow::Result<()> {
        match rule.rule_class().native_name() {
            Some("config_setting") => {
                let requirements = ConfigurationRequirements::from_rule(rule)?;
                resolve_constraint_values(self.workspace, &requirements.constraints, || {
                    format!("config_setting `{label}`")
                })
                .await?;
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
                resolve_constraint_values(
                    self.workspace,
                    crate::bazel::config::label_list(rule, "constraint_values")?,
                    || format!("platform `{label}`"),
                )
                .await?;
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

    /// Loads the targets of select() keys `labels` concurrently, following aliases. `aliases`
    /// are those already followed, to detect cycles.
    async fn load_conditions<'l>(
        &self,
        labels: BTreeSet<CanonicalLabel<'l>>,
        configuration: ConfigurationKind,
        aliases: &BTreeSet<CanonicalLabel<'static>>,
    ) -> anyhow::Result<BTreeMap<CanonicalLabel<'l>, Condition>> {
        let loaded = join_all(labels.iter().map(|label| {
            let mut aliases = aliases.clone();
            async move {
                self.load_condition(label, configuration, &mut aliases)
                    .await
            }
        }))
        .await;
        labels
            .into_iter()
            .zip(loaded)
            .map(|(label, condition)| Ok((label, condition?)))
            .collect()
    }

    /// Loads the target of select() key `label`, following aliases.
    #[async_recursion::async_recursion]
    async fn load_condition<'l>(
        &self,
        label: &CanonicalLabel<'l>,
        configuration: ConfigurationKind,
        aliases: &mut BTreeSet<CanonicalLabel<'static>>,
    ) -> anyhow::Result<Condition> {
        let target = self
            .workspace
            .load_target(label)
            .await?
            .ok_or_else(|| anyhow::anyhow!("select condition `{label}` does not exist"))?;
        let Some(rule) = target.target.as_rule() else {
            anyhow::bail!("select condition `{label}` is not a config_setting or constraint_value");
        };
        let (platform, constraints) = match rule.rule_class().native_name() {
            Some("alias") => {
                if !aliases.insert(label.clone().into_owned()) {
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
                let mut labels = BTreeSet::new();
                for part in &attribute.parts {
                    if let AttributeValuePart::Selector(selector) = part {
                        labels.extend(selector.entries.keys().map(CanonicalLabel::as_borrowed));
                    }
                }
                let conditions = self.load_conditions(labels, configuration, aliases).await?;
                let matches = self
                    .configurations
                    .match_conditions(configuration, &conditions)?;
                let actual = resolve_attribute(schema, attribute, &matches)?;
                let DirectAttributeValue::Label(actual) = &*actual else {
                    anyhow::bail!("alias select condition `{label}` has an invalid actual target");
                };
                return self.load_condition(actual, configuration, aliases).await;
            }
            Some("config_setting") => {
                let requirements = ConfigurationRequirements::from_rule(rule)?;
                let platform = match requirements.values.get("platforms") {
                    Some(platform) => {
                        let repository = self.workspace.main_repo().await?;
                        let parsed = crate::bazel::label::parse_label(platform, &MAIN_REPO_ROOT)
                            .map_err(|error| {
                                anyhow::anyhow!("invalid platforms condition: {error}")
                            })?;
                        let canonical = repository.resolve_label(parsed).ok_or_else(|| {
                            anyhow::anyhow!("unknown repository in platforms condition")
                        })?;
                        Some(canonical.to_string())
                    }
                    None => None,
                };
                let constraints =
                    resolve_constraint_values(self.workspace, &requirements.constraints, || {
                        format!("config_setting `{label}`")
                    })
                    .await?;
                (platform, constraints)
            }
            Some("constraint_value") => {
                let (constraint, _) = resolve_constraint_value(self.workspace, label).await?;
                (None, BTreeSet::from([constraint]))
            }
            _ => anyhow::bail!(
                "select condition `{label}` is not a config_setting or constraint_value"
            ),
        };
        Ok(Condition {
            target,
            platform,
            constraints,
        })
    }
}

/// Resolves the select()s of `value` with the conditions in `matches`.
fn resolve_attribute<'v>(
    schema: &'v AttributeSchema,
    value: &'v AttributeValue,
    matches: &BTreeMap<CanonicalLabel<'_>, Option<ConditionMatch<'_>>>,
) -> anyhow::Result<Cow<'v, DirectAttributeValue>> {
    value.resolve(schema, |condition| {
        Ok(matches.get(&condition).and_then(Option::as_ref))
    })
}

/// Calls `visitor` with each label of a resolved value, borrowing labels from borrowed values.
fn visit_resolved_labels<'v>(
    value: Cow<'v, DirectAttributeValue>,
    mut visitor: impl FnMut(CanonicalLabel<'v>),
) {
    match value {
        Cow::Borrowed(value) => value.visit_labels(|label| visitor(label.as_borrowed())),
        // A concatenated label list is the only owned value with labels; move them out.
        Cow::Owned(DirectAttributeValue::LabelList(labels)) => labels.into_iter().for_each(visitor),
        Cow::Owned(value) => value.visit_labels(|label| visitor(label.clone())),
    }
}

/// Resolves `constraints` concurrently through aliases, failing if two are values of the same
/// constraint setting. `owner` describes the target that lists them, for that error.
async fn resolve_constraint_values<'l, 'k: 'l>(
    workspace: &Arc<Workspace>,
    constraints: impl IntoIterator<Item = &'l CanonicalLabel<'k>>,
    owner: impl FnOnce() -> String,
) -> anyhow::Result<BTreeSet<CanonicalLabel<'static>>> {
    let resolved = join_all(
        constraints
            .into_iter()
            .map(|constraint| resolve_constraint_value(workspace, constraint)),
    )
    .await;
    let mut values = BTreeSet::new();
    let mut settings = BTreeSet::new();
    for result in resolved {
        let (value, setting) = result?;
        if settings.contains(&setting) {
            anyhow::bail!(
                "{} contains multiple values for constraint setting `{setting}`",
                owner()
            );
        }
        settings.insert(setting);
        values.insert(value);
    }
    Ok(values)
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
    label: &CanonicalLabel<'_>,
) -> anyhow::Result<CanonicalLabel<'static>> {
    let mut label = label.clone().into_owned();
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
    label: &CanonicalLabel<'_>,
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
    let context = CqueryContext::new(Analysis {
        workspace: &workspace,
        configurations: &configurations,
        universe: &universe,
    });
    let mut result = context.eval(&ast.inner);
    // Every pattern now has its own reader of the universe. Dropping the context's reader, which
    // never advances, lets the universe buffer release results that every pattern has read.
    drop(context);
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

    async fn test_workspace(
        packages: &[(&str, &str)],
    ) -> (assert_fs::TempDir, Arc<Workspace>, Configurations) {
        let root = assert_fs::TempDir::new().unwrap();
        std::fs::write(
            root.path().join("MODULE.bazel"),
            "module(name = 'cquery_test')",
        )
        .unwrap();
        for (package, source) in packages {
            let path = root.path().join(package);
            std::fs::create_dir_all(&path).unwrap();
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
        (root, workspace, configurations)
    }

    /// Evaluates `source` like `cquery()`, returning each result's label up to the first error.
    async fn evaluate(
        workspace: &Arc<Workspace>,
        configurations: &Configurations,
        source: &str,
    ) -> Vec<Result<String, String>> {
        let expression = crate::query_syntax::parse(source).unwrap();
        let mut universe = Vec::new();
        collect_target_patterns(&expression.inner, &mut universe);
        let context = CqueryContext::new(Analysis {
            workspace,
            configurations,
            universe: &universe,
        });
        let mut results = context.eval(&expression.inner);
        drop(context);
        let mut labels = Vec::new();
        while let Some(result) = results.next().await {
            let failed = result.is_err();
            labels.push(result.map(|target| target.target.label.to_string()));
            if failed {
                break;
            }
        }
        labels
    }

    #[tokio::test]
    async fn exact_patterns_require_their_target_while_wildcards_may_match_nothing() {
        let (_root, workspace, configurations) =
            test_workspace(&[("", "filegroup(name = 'a')"), ("empty", "")]).await;
        assert_eq!(
            evaluate(
                &workspace,
                &configurations,
                "//:a + //empty:all + //empty/..."
            )
            .await,
            [Ok("@@//:a".to_owned())]
        );
        assert_eq!(
            evaluate(&workspace, &configurations, "//:a + //:missing").await,
            [
                Ok("@@//:a".to_owned()),
                Err(
                    "no such target '//:missing': target 'missing' not declared in package ''"
                        .to_owned()
                ),
            ]
        );
        assert_eq!(
            evaluate(&workspace, &configurations, "//empty:missing").await,
            [Err(
                "no such target '//empty:missing': target 'missing' not declared in package \
                 'empty'"
                    .to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn every_pattern_shares_one_analysis_of_the_universe() {
        let targets = (0..32)
            .map(|index| format!("filegroup(name = 't{index}', srcs = [':common'])\n"))
            .collect::<String>();
        let build = format!("{targets}filegroup(name = 'common')\n");
        let (_root, workspace, configurations) = test_workspace(&[("", &build)]).await;
        let source = (0..32)
            .map(|index| format!("//:t{index}"))
            .collect::<Vec<_>>()
            .join(" + ");

        ANALYZED.set(0);
        let results = evaluate(&workspace, &configurations, &source).await;
        assert_eq!(results.len(), 32);
        assert!(results.iter().all(Result::is_ok));
        // The 32 roots and their shared dependency, each analyzed once rather than once per
        // pattern.
        assert_eq!(ANALYZED.get(), 33);
    }

    #[tokio::test]
    async fn universe_loading_is_lazy_and_cancellable() {
        let (root, workspace, configurations) = test_workspace(&[
            ("a", "filegroup(name = 'early')"),
            ("z", "fail('late package error')"),
        ])
        .await;
        let analysis = Analysis {
            workspace: &workspace,
            configurations: &configurations,
            universe: &["//..."],
        };
        let mut targets = analysis.walk_universe();
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
        let mut targets = analysis.walk_universe();
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
