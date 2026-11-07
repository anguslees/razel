use crate::bazel::InvocationOptions;
use crate::bazel::label::MAIN_REPO_ROOT;
use crate::bazel::target::Target;
use crate::query_syntax::{Expr, QuerySyntaxError, SetOp};
use crate::stream_tee::{StreamTee, StreamTeeExt};
use crate::workspace::{ExpandedTarget, ExpandedTargetKind, Workspace};
use clap::ValueEnum;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream, StreamExt};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum QueryOutput {
    #[default]
    Label,
    LabelKind,
}

/// Values of `--order_output`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum OrderOutput {
    /// No particular order: each result is printed as soon as it is found
    No,
    /// Dependency order (not supported yet)
    Deps,
    /// Label order, Bazel's default for the `label` and `label_kind` outputs
    #[default]
    Auto,
    /// Dependency order with ties broken by label (not supported yet)
    Full,
}

/// The order in which an expression yields its results. No expression asks its operands for a
/// stricter order than its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    /// Any order, yielding each result as soon as it is known.
    Any,
    /// Increasing label order, without duplicates.
    Sorted,
}

pub type QueryResult = Result<ExpandedTarget, String>;
pub type QueryStream<'a> = BoxStream<'a, QueryResult>;

#[derive(Debug)]
pub enum QueryError {
    Syntax(QuerySyntaxError),
    UnsupportedOrderOutput(OrderOutput),
    NotInWorkspace(std::io::Error),
    Environment(std::io::Error),
    Evaluation(anyhow::Error),
    Output(std::io::Error),
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => error.fmt(formatter),
            Self::UnsupportedOrderOutput(order) => write!(
                formatter,
                "--order_output={} is not supported yet",
                order.to_possible_value().unwrap().get_name()
            ),
            Self::NotInWorkspace(error) => error.fmt(formatter),
            Self::Environment(error) => error.fmt(formatter),
            Self::Evaluation(error) => write!(formatter, "{error:#}"),
            Self::Output(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Syntax(error) => Some(error),
            Self::UnsupportedOrderOutput(_) => None,
            Self::NotInWorkspace(error) => Some(error),
            Self::Environment(error) => Some(error),
            Self::Evaluation(error) => Some(error.as_ref()),
            Self::Output(error) => Some(error),
        }
    }
}

impl From<QuerySyntaxError> for QueryError {
    fn from(error: QuerySyntaxError) -> Self {
        Self::Syntax(error)
    }
}

impl From<anyhow::Error> for QueryError {
    fn from(error: anyhow::Error) -> Self {
        Self::Evaluation(error)
    }
}

impl From<std::io::Error> for QueryError {
    fn from(error: std::io::Error) -> Self {
        Self::Output(error)
    }
}

pub(crate) fn workspace_error(error: std::io::Error) -> QueryError {
    if error.kind() == std::io::ErrorKind::NotFound {
        QueryError::NotInWorkspace(error)
    } else {
        QueryError::Environment(error)
    }
}

#[derive(Clone)]
pub struct QueryContext<'a> {
    pub workspace: Arc<Workspace>,
    pub variables: HashMap<&'a str, StreamTee<QueryStream<'a>>>,
}

impl<'a> QueryContext<'a> {
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            variables: HashMap::new(),
        }
    }
}

impl<'a> Expr<'a> {
    /// Evaluates this expression, yielding its results in `order`.
    pub(crate) fn eval(&self, ctx: &QueryContext<'a>, order: Order) -> QueryStream<'a> {
        match self {
            // Patterns expand in label order, which suits every order.
            &Expr::String(pattern) => {
                match crate::bazel::label::parse_target_pattern(pattern, &MAIN_REPO_ROOT) {
                    Ok(pattern) => ctx
                        .workspace
                        .clone()
                        .expand_pattern(pattern)
                        .map(|result| result.map_err(|error| error.to_string()))
                        .boxed(),
                    Err(error) => stream::once(async move { Err(error.to_string()) }).boxed(),
                }
            }
            Expr::Function("deps", args) => eval_deps(args, ctx, order),
            Expr::Function(name, _args) => {
                let error = format!("Function {name} is not supported by query");
                stream::once(async move { Err(error) }).boxed()
            }
            Expr::Let(name, value, body) => {
                // No reference to the binding needs a stricter order than the body, so one
                // evaluation in the body's order serves them all.
                let value = value.inner.eval(ctx, order).tee();
                let mut nested = ctx.clone();
                nested.variables.insert(name, value);
                body.inner.eval(&nested, order)
            }
            Expr::Variable(name) => match ctx.variables.get(name) {
                Some(stream) => stream.clone().boxed(),
                None => {
                    let error = format!("Undefined variable {name}");
                    stream::once(async move { Err(error) }).boxed()
                }
            },
            Expr::SetOp(op, left, right) => {
                let left = left.inner.eval(ctx, order);
                let right = right.inner.eval(ctx, order);
                match (order, op) {
                    (Order::Any, SetOp::Union) => union(left, right),
                    (Order::Any, SetOp::Intersect) => eval_set_filter(left, right, true),
                    (Order::Any, SetOp::Difference) => eval_set_filter(left, right, false),
                    (Order::Sorted, SetOp::Union) => merge_sorted(left, right, |_, _| true),
                    (Order::Sorted, SetOp::Intersect) => {
                        merge_sorted(left, right, |in_left, in_right| in_left && in_right)
                    }
                    (Order::Sorted, SetOp::Difference) => {
                        merge_sorted(left, right, |in_left, in_right| in_left && !in_right)
                    }
                }
            }
        }
    }
}

fn eval_deps<'a>(
    args: &[chumsky::span::Spanned<Expr<'a>>],
    ctx: &QueryContext<'a>,
    order: Order,
) -> QueryStream<'a> {
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
    let workspace = ctx.workspace.clone();
    // Sorted results are sorted after the traversal, so the roots may arrive in any order.
    let targets = breadth_first(
        args[0].inner.eval(ctx, Order::Any),
        max_depth,
        |target: &ExpandedTarget| target.label.clone(),
        move |target| {
            let workspace = workspace.clone();
            async move { unconfigured_dependencies(&workspace, &target).await }.boxed()
        },
    );
    match order {
        Order::Any => targets,
        Order::Sorted => sorted(targets),
    }
}

/// Yields `roots` and their transitive dependencies breadth first: the roots as they arrive, then
/// each further level once the previous one is complete, so every target is found at its minimum
/// depth, which `max_depth` limits. Each target's dependencies are resolved right after it is
/// yielded, while its package is likely still loaded. Only keys and the next level are retained.
pub(crate) fn breadth_first<'a, T, K>(
    roots: BoxStream<'a, Result<T, String>>,
    max_depth: Option<usize>,
    key: impl Fn(&T) -> K + Send + 'a,
    dependencies: impl Fn(T) -> BoxFuture<'a, anyhow::Result<Vec<T>>> + Send + 'a,
) -> BoxStream<'a, Result<T, String>>
where
    T: Clone + Send + 'a,
    K: Eq + std::hash::Hash + Send + 'a,
{
    async_stream::stream! {
        let mut seen = HashSet::new();
        let mut level = roots;
        let mut depth = 0;
        loop {
            let mut next = Vec::new();
            let mut found = HashSet::new();
            while let Some(target) = level.next().await {
                let target = match target {
                    Ok(target) => target,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };
                if !seen.insert(key(&target)) {
                    continue;
                }
                yield Ok(target.clone());
                if max_depth.is_some_and(|max_depth| depth >= max_depth) {
                    continue;
                }
                match dependencies(target).await {
                    Ok(dependencies) => {
                        for dependency in dependencies {
                            let dependency_key = key(&dependency);
                            // A later target of this level may still be found here; it is skipped
                            // when the next level is read.
                            if !seen.contains(&dependency_key) && found.insert(dependency_key) {
                                next.push(dependency);
                            }
                        }
                    }
                    Err(error) => {
                        yield Err(error.to_string());
                        return;
                    }
                }
            }
            if next.is_empty() {
                return;
            }
            level = stream::iter(next.into_iter().map(Ok)).boxed();
            depth += 1;
        }
    }
    .boxed()
}

async fn unconfigured_dependencies(
    workspace: &Arc<Workspace>,
    target: &ExpandedTarget,
) -> anyhow::Result<Vec<ExpandedTarget>> {
    let labels = unconfigured_dependency_labels(workspace, target).await?;
    let mut dependencies = Vec::with_capacity(labels.len());
    for label in labels {
        let dependency = workspace
            .load_target(&label)
            .await?
            .ok_or_else(|| anyhow::anyhow!("target `{label}` does not exist"))?;
        dependencies.push(dependency.expanded);
    }
    Ok(dependencies)
}

async fn unconfigured_dependency_labels(
    workspace: &Arc<Workspace>,
    target: &ExpandedTarget,
) -> anyhow::Result<Vec<crate::bazel::label::CanonicalLabel<'static>>> {
    let loaded = workspace
        .load_target(&target.label)
        .await?
        .ok_or_else(|| anyhow::anyhow!("target `{}` does not exist", target.label))?;
    let mut labels = Vec::new();
    match &loaded.target {
        Target::Rule(rule) => rule.visit_dependency_labels(|label| labels.push(label.clone())),
        Target::GeneratedFile(generated) => labels.push(crate::bazel::label::CanonicalLabel::new(
            target.label.repo.clone(),
            target.label.package().to_owned(),
            generated.generating_rule.clone(),
        )),
        Target::SourceFile(_) => {}
    }
    labels.sort();
    labels.dedup();
    Ok(labels)
}

/// Yields the results of `left`, then those of `right` that `left` lacked.
fn union<'a>(left: QueryStream<'a>, right: QueryStream<'a>) -> QueryStream<'a> {
    async_stream::stream! {
        let mut seen = HashSet::new();
        for mut values in [left, right] {
            while let Some(result) = values.next().await {
                match result {
                    Ok(target) if seen.insert(target.label.clone()) => yield Ok(target),
                    Ok(_) => {}
                    Err(error) => yield Err(error),
                }
            }
        }
    }
    .boxed()
}

/// Merges two label-sorted streams, keeping each label for which `keep(in_left, in_right)` holds.
/// Both streams are read to the end, so neither stream's errors are skipped.
fn merge_sorted<'a>(
    mut left: QueryStream<'a>,
    mut right: QueryStream<'a>,
    keep: fn(bool, bool) -> bool,
) -> QueryStream<'a> {
    async_stream::stream! {
        let (mut next_left, mut next_right) = (None, None);
        let (mut pull_left, mut pull_right) = (true, true);
        loop {
            if pull_left {
                next_left = match left.next().await.transpose() {
                    Ok(next) => next,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };
            }
            if pull_right {
                next_right = match right.next().await.transpose() {
                    Ok(next) => next,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };
            }
            let ordering = match (&next_left, &next_right) {
                (None, None) => return,
                (Some(a), Some(b)) => a.label.cmp(&b.label),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
            };
            // Advance each stream holding the smallest label; an ended stream is never polled again.
            (pull_left, pull_right) = (ordering.is_le(), ordering.is_ge());
            let target = if pull_left { next_left.take() } else { next_right.take() };
            if keep(pull_left, pull_right) && let Some(target) = target {
                yield Ok(target);
            }
        }
    }
    .boxed()
}

/// Yields `results` in label order. On failure, the results found before the error precede it.
fn sorted<'a>(mut results: QueryStream<'a>) -> QueryStream<'a> {
    async_stream::stream! {
        let mut targets = Vec::new();
        let mut failure = None;
        while let Some(result) = results.next().await {
            match result {
                Ok(target) => targets.push(target),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        targets.sort_unstable_by(|a, b| a.label.cmp(&b.label));
        targets.dedup_by(|a, b| a.label == b.label);
        for target in targets {
            yield Ok(target);
        }
        if let Some(error) = failure {
            yield Err(error);
        }
    }
    .boxed()
}

fn eval_set_filter<'a>(
    mut left: QueryStream<'a>,
    mut right: QueryStream<'a>,
    include_matches: bool,
) -> QueryStream<'a> {
    async_stream::stream! {
        let mut right_labels = HashSet::new();
        while let Some(result) = right.next().await {
            match result {
                Ok(target) => {
                    right_labels.insert(target.label);
                }
                Err(error) => yield Err(error),
            }
        }
        let mut seen = HashSet::new();
        while let Some(result) = left.next().await {
            match result {
                Ok(target)
                    if right_labels.contains(&target.label) == include_matches
                        && seen.insert(target.label.clone()) =>
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

pub async fn query<W>(
    out: &mut W,
    options: Arc<InvocationOptions>,
    output: QueryOutput,
    order_output: OrderOutput,
    query: &str,
) -> Result<(), QueryError>
where
    W: AsyncWrite + Unpin,
{
    let order = match order_output {
        OrderOutput::No => Order::Any,
        OrderOutput::Auto => Order::Sorted,
        // Placeholder. Bazel sorts these once, at output time, over the graph of edges that
        // evaluation followed. Supporting them means evaluating in `Order::Any` while recording
        // those edges, then sorting the results topologically.
        OrderOutput::Deps | OrderOutput::Full => {
            return Err(QueryError::UnsupportedOrderOutput(order_output));
        }
    };
    let ast = crate::query_syntax::parse(query)?;

    let workspace = Workspace::new(".", options)
        .await
        .map_err(workspace_error)?;
    let context = QueryContext::new(workspace);
    let mut result = ast.inner.eval(&context, order);
    while let Some(target) = result.next().await {
        let target = target.map_err(|error| anyhow::anyhow!("Query evaluation error: {error}"))?;
        match output {
            QueryOutput::Label => {}
            QueryOutput::LabelKind => match &target.kind {
                ExpandedTargetKind::Rule(rule_class) => {
                    out.write_all(rule_class.as_bytes()).await?;
                    out.write_all(b" rule ").await?;
                }
                ExpandedTargetKind::SourceFile => {
                    out.write_all(b"source file ").await?;
                }
                ExpandedTargetKind::GeneratedFile => {
                    out.write_all(b"generated file ").await?;
                }
            },
        }
        write_label(out, &target.label).await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
    }
    Ok(())
}

pub(crate) async fn write_label<W>(
    out: &mut W,
    label: &crate::bazel::label::CanonicalLabel<'_>,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if !label.repo.as_str().is_empty() {
        out.write_all(b"@@").await?;
        out.write_all(label.repo.as_str().as_bytes()).await?;
    }
    out.write_all(b"//").await?;
    out.write_all(label.package().as_bytes()).await?;
    out.write_all(b":").await?;
    out.write_all(label.name().as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_errors_distinguish_absence_from_environment_failures() {
        assert!(matches!(
            workspace_error(std::io::Error::from(std::io::ErrorKind::NotFound)),
            QueryError::NotInWorkspace(_)
        ));
        assert!(matches!(
            workspace_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            QueryError::Environment(_)
        ));
    }

    fn results(targets: &[(&str, &str)]) -> Vec<QueryResult> {
        targets
            .iter()
            .map(|(package, name)| {
                Ok(ExpandedTarget {
                    label: crate::bazel::label::CanonicalLabel::new(
                        crate::bazel::label::MAIN_REPO,
                        package.to_string(),
                        name.to_string(),
                    ),
                    kind: ExpandedTargetKind::SourceFile,
                })
            })
            .collect()
    }

    async fn labels(stream: QueryStream<'_>) -> Vec<Result<(String, String), String>> {
        stream
            .map(|result| {
                result.map(|target| {
                    let label = target.label;
                    (label.package().to_owned(), label.name().to_owned())
                })
            })
            .collect()
            .await
    }

    fn expected(targets: &[(&str, &str)]) -> Vec<Result<(String, String), String>> {
        targets
            .iter()
            .map(|(package, name)| Ok((package.to_string(), name.to_string())))
            .collect()
    }

    #[tokio::test]
    async fn sorted_set_operations_merge_in_label_order() {
        // Package paths compare as whole strings, so `a-c` sorts between `a` and `a/b`.
        let left = [("a", "x"), ("a-c", "x"), ("a/b", "x")];
        let right = [("a", "y"), ("a-c", "x"), ("b", "x")];
        let merge = |keep| {
            merge_sorted(
                stream::iter(results(&left)).boxed(),
                stream::iter(results(&right)).boxed(),
                keep,
            )
        };
        assert_eq!(
            labels(merge(|_, _| true)).await,
            expected(&[
                ("a", "x"),
                ("a", "y"),
                ("a-c", "x"),
                ("a/b", "x"),
                ("b", "x")
            ])
        );
        assert_eq!(
            labels(merge(|in_left, in_right| in_left && in_right)).await,
            expected(&[("a-c", "x")])
        );
        assert_eq!(
            labels(merge(|in_left, in_right| in_left && !in_right)).await,
            expected(&[("a", "x"), ("a/b", "x")])
        );

        // An error after the last kept label still ends the merge in failure.
        let mut late_error = results(&[("b", "x")]);
        late_error.push(Err("late error".to_owned()));
        assert_eq!(
            labels(merge_sorted(
                stream::iter(results(&[("a", "x")])).boxed(),
                stream::iter(late_error).boxed(),
                |in_left, in_right| in_left && in_right,
            ))
            .await,
            vec![Err("late error".to_owned())]
        );
    }

    #[tokio::test]
    async fn breadth_first_finds_each_target_at_its_minimum_depth() {
        let edges = HashMap::from([
            ("a", vec!["b", "c"]),
            ("c", vec!["b", "d"]),
            ("x", vec!["a"]),
        ]);
        let traverse = |roots: &[&'static str], max_depth| {
            let edges = edges.clone();
            breadth_first(
                stream::iter(roots.iter().map(|root| Ok(*root)).collect::<Vec<_>>()).boxed(),
                max_depth,
                |name: &&'static str| *name,
                move |name| {
                    let dependencies = edges.get(name).cloned().unwrap_or_default();
                    async move { Ok(dependencies) }.boxed()
                },
            )
            .map(|result| result.unwrap())
            .collect::<Vec<_>>()
        };
        // `b` is found through `a` before `c`, although `c` also depends on it.
        assert_eq!(traverse(&["a"], None).await, ["a", "b", "c", "d"]);
        assert_eq!(traverse(&["a"], Some(0)).await, ["a"]);
        // Depth counts from the nearest root, including a root found earlier as a dependency.
        assert_eq!(
            traverse(&["x", "c"], Some(1)).await,
            ["x", "c", "a", "b", "d"]
        );
        assert_eq!(traverse(&["x", "a"], Some(1)).await, ["x", "a", "b", "c"]);
    }

    #[tokio::test]
    async fn recursive_patterns_expand_in_label_order() {
        use assert_fs::prelude::*;

        // Sorted set operations rely on this order, which differs from that of printed labels.
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("MODULE.bazel")
            .write_str("module(name = \"order\")\n")
            .unwrap();
        for package in ["", "a", "a-c", "a/b", "b"] {
            temp.child(format!("{package}/BUILD.bazel").trim_start_matches('/'))
                .write_str("filegroup(name = \"z\")\nfilegroup(name = \"y\")\n")
                .unwrap();
        }
        let workspace = Workspace::new(
            temp.path(),
            Arc::new(InvocationOptions {
                ignore_dev_dependency: false,
            }),
        )
        .await
        .unwrap();
        let labels = Expr::String("//...")
            .eval(&QueryContext::new(workspace), Order::Sorted)
            .map(|result| result.unwrap().label)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(labels.len(), 10);
        assert!(labels.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
