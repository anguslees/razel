use crate::bazel::InvocationOptions;
use crate::bazel::label::MAIN_REPO_ROOT;
use crate::stream_tee::{StreamTee, StreamTeeExt};
use crate::workspace::{ExpandedTarget, ExpandedTargetKind, Workspace};
use chumsky::prelude::*;
use chumsky::span::{SimpleSpan, Spanned};
use clap::ValueEnum;
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::{HashMap, HashSet};
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

pub type QueryResult = Result<ExpandedTarget, String>;
pub type QueryStream<'a> = BoxStream<'a, QueryResult>;

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

#[derive(Debug, Clone, PartialEq)]
pub enum Expr<'a> {
    String(&'a str),
    Int(i64),
    Function(&'a str, Vec<Spanned<Expr<'a>>>),
    SetOp(SetOp, Box<Spanned<Expr<'a>>>, Box<Spanned<Expr<'a>>>),
    Let(&'a str, Box<Spanned<Expr<'a>>>, Box<Spanned<Expr<'a>>>),
    Variable(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    Union,
    Difference,
    Intersect,
}

#[allow(clippy::let_and_return)]
pub fn parser<'a>() -> impl Parser<'a, &'a str, Spanned<Expr<'a>>, extra::Err<Rich<'a, char>>> {
    recursive(|expr| {
        let paren_expr = expr
            .clone()
            .delimited_by(just('(').padded(), just(')').padded())
            .map(|s: Spanned<Expr<'a>>| s.inner);

        let ident = text::ident().to_slice();
        // Unquoted words: alphanumeric and */@.-_:$~[]
        // Unquoted words may not start with - or *
        // Unquoted words may not contain + unless it starts with @@
        let unquoted_word = any::<&str, extra::Err<Rich<'a, char>>>()
            .filter(|c: &char| c.is_ascii_alphanumeric() || "*/@.-_:$~[]+".contains(*c))
            .repeated()
            .at_least(1)
            .to_slice()
            .try_map(|s: &str, span| {
                if s.starts_with('-') || s.starts_with('*') {
                    Err(Rich::custom(
                        span,
                        "Unquoted word cannot start with '-' or '*'",
                    ))
                } else if s.contains('+') && !s.starts_with("@@") {
                    Err(Rich::custom(
                        span,
                        "Unquoted word cannot contain '+' unless it starts with '@@'",
                    ))
                } else {
                    Ok(s)
                }
            });

        // Functions: e.g. deps(//foo), kind("*.cc", deps(//bar))
        let int_arg = unquoted_word
            .try_map(|s: &str, span| {
                s.parse::<i64>()
                    .map(Expr::Int)
                    .map_err(|e| Rich::custom(span, format!("Invalid integer: {}", e)))
            })
            .map_with(|ast, e| Spanned {
                inner: ast,
                span: e.span(),
            });

        let function = ident
            .then(
                choice((int_arg, expr.clone()))
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>()
                    .delimited_by(just('(').padded(), just(')').padded()),
            )
            .map(|(name, args)| Expr::Function(name, args));

        // Variable ref: e.g. $foo
        let variable = just('$').ignore_then(ident).map(Expr::Variable);

        // Let binding: let name = expr in expr
        let let_binding = text::keyword("let")
            .padded()
            .ignore_then(ident)
            .then_ignore(just('=').padded())
            .then(expr.clone())
            .then_ignore(text::keyword("in").padded())
            .then(expr.clone())
            .map(|((name, val), body)| Expr::Let(name, Box::new(val), Box::new(body)));

        // Strings
        let string_literal = just('"')
            .ignore_then(none_of('"').repeated().to_slice())
            .then_ignore(just('"'));

        let single_string_literal = just('\'')
            .ignore_then(none_of('\'').repeated().to_slice())
            .then_ignore(just('\''));

        let quoted_string = string_literal.or(single_string_literal).map(Expr::String);

        let pattern = unquoted_word.map(Expr::String);

        let atom = choice((
            paren_expr,
            function,
            let_binding,
            variable,
            quoted_string,
            pattern,
        ))
        .padded()
        .map_with(|ast, e| Spanned {
            inner: ast,
            span: e.span(),
        });

        // Intersect operator has higher precedence
        let op_intersect = choice((just("^"), text::keyword("intersect")))
            .padded()
            .to(SetOp::Intersect);

        let intersection =
            atom.clone()
                .foldl(op_intersect.then(atom).repeated(), |left, (op, right)| {
                    let span = SimpleSpan::from(left.span.start..right.span.end);
                    Spanned {
                        inner: Expr::SetOp(op, Box::new(left), Box::new(right)),
                        span,
                    }
                });

        // Union / Difference have lower precedence
        let op_union = choice((just("+"), text::keyword("union")))
            .padded()
            .to(SetOp::Union);

        let op_diff = choice((just("-"), text::keyword("except")))
            .padded()
            .to(SetOp::Difference);

        let set_op = intersection.clone().foldl(
            choice((op_union, op_diff)).then(intersection).repeated(),
            |left, (op, right)| {
                let span = SimpleSpan::from(left.span.start..right.span.end);
                Spanned {
                    inner: Expr::SetOp(op, Box::new(left), Box::new(right)),
                    span,
                }
            },
        );

        set_op
    })
}

impl<'a> Expr<'a> {
    pub fn eval(&self, ctx: &QueryContext<'a>) -> QueryStream<'a> {
        match self {
            &Expr::String(pattern) => {
                match crate::bazel::label::parse_target_pattern(pattern, &MAIN_REPO_ROOT) {
                    Ok(pattern) => {
                        let mut targets = ctx.workspace.clone().expand_pattern(pattern);
                        async_stream::stream! {
                            let mut matched = Vec::new();
                            while let Some(result) = targets.next().await {
                                match result {
                                    Ok(target) => matched.push(target),
                                    Err(error) => {
                                        yield Err(error.to_string());
                                        return;
                                    }
                                }
                            }
                            matched.sort_unstable_by(|left, right| left.label.cmp(&right.label));
                            for target in matched {
                                yield Ok(target);
                            }
                        }
                        .boxed()
                    }
                    Err(error) => stream::once(async move { Err(error.to_string()) }).boxed(),
                }
            }
            Expr::Int(_) => {
                stream::once(async { Err("Int not supported out of function context".to_owned()) })
                    .boxed()
            }
            Expr::Function(name, _args) => {
                let error = format!("Function {name} not fully implemented");
                stream::once(async move { Err(error) }).boxed()
            }
            Expr::Let(name, value, body) => {
                let value = value.inner.eval(ctx).tee();
                let mut nested = ctx.clone();
                nested.variables.insert(name, value);
                body.inner.eval(&nested)
            }
            Expr::Variable(name) => match ctx.variables.get(name) {
                Some(stream) => stream.clone().boxed(),
                None => {
                    let error = format!("Undefined variable {name}");
                    stream::once(async move { Err(error) }).boxed()
                }
            },
            Expr::SetOp(SetOp::Union, left, right) => {
                let left = left.inner.eval(ctx);
                let right = right.inner.eval(ctx);
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
            Expr::SetOp(SetOp::Intersect, _, _) => {
                stream::once(async { Err("Intersect not yet implemented".to_owned()) }).boxed()
            }
            Expr::SetOp(SetOp::Difference, _, _) => {
                stream::once(async { Err("Difference not yet implemented".to_owned()) }).boxed()
            }
        }
    }
}

pub async fn query<W>(
    out: &mut W,
    options: Arc<InvocationOptions>,
    output: QueryOutput,
    query: &str,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let workspace = Workspace::new(".", options).await?;

    let ast = parser().parse(query).into_result().map_err(|errs| {
        anyhow::anyhow!(
            "Failed to parse query: {}\nSee https://bazel.build/reference/query for syntax",
            errs.into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;

    let context = QueryContext::new(workspace);
    let mut result = ast.inner.eval(&context);
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
        if !target.label.repo.as_str().is_empty() {
            out.write_all(b"@@").await?;
            out.write_all(target.label.repo.as_str().as_bytes()).await?;
        }
        out.write_all(b"//").await?;
        out.write_all(target.label.package().as_bytes()).await?;
        out.write_all(b":").await?;
        out.write_all(target.label.name().as_bytes()).await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Expr<'_> {
        parser()
            .parse(input)
            .into_result()
            .expect("Parse failed")
            .inner
    }

    #[test]
    fn test_wildcard() {
        assert_eq!(parse("//foo:bar"), Expr::String("//foo:bar"));
        assert_eq!(parse("//foo/..."), Expr::String("//foo/..."));
        assert_eq!(parse("//foo/...:*"), Expr::String("//foo/...:*"));
        assert_eq!(parse("//foo:*"), Expr::String("//foo:*"));
        assert_eq!(parse("//foo:all"), Expr::String("//foo:all"));
    }

    #[test]
    fn test_deps_function() {
        if let Expr::Function(name, args) = parse("deps(//foo)") {
            assert_eq!(name, "deps");
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].inner, Expr::String("//foo"));
        } else {
            panic!("Expected deps function");
        }
    }

    #[test]
    fn test_union() {
        match parse("//foo + //bar") {
            Expr::SetOp(SetOp::Union, left, right) => {
                assert_eq!(left.inner, Expr::String("//foo"));
                assert_eq!(right.inner, Expr::String("//bar"));
            }
            _ => panic!("Expected union op"),
        }
    }

    #[test]
    fn test_intersection() {
        match parse("//foo ^ //bar") {
            Expr::SetOp(SetOp::Intersect, left, right) => {
                assert_eq!(left.inner, Expr::String("//foo"));
                assert_eq!(right.inner, Expr::String("//bar"));
            }
            _ => panic!("Expected intersect op"),
        }
    }

    #[test]
    fn test_precedence() {
        match parse("//a + //b ^ //c") {
            Expr::SetOp(SetOp::Union, left, right) => {
                assert_eq!(left.inner, Expr::String("//a"));
                match &right.inner {
                    Expr::SetOp(SetOp::Intersect, inner_left, inner_right) => {
                        assert_eq!(inner_left.inner, Expr::String("//b"));
                        assert_eq!(inner_right.inner, Expr::String("//c"));
                    }
                    _ => panic!("Expected inner intersect"),
                }
            }
            _ => panic!("Expected outer union"),
        }
    }
    #[test]
    fn test_int_argument_in_function() {
        if let Expr::Function(name, args) = parse("deps(//foo, 7)") {
            assert_eq!(name, "deps");
            assert_eq!(args.len(), 2);
            assert_eq!(args[0].inner, Expr::String("//foo"));
            assert_eq!(args[1].inner, Expr::Int(7));
        } else {
            panic!("Expected deps function with Int argument");
        }
    }

    #[test]
    fn test_pure_int_parses_as_target() {
        // Pure integers are not valid top-level expressions in Bazel queries.
        // Outside a function arg they are parsed as targets.
        assert_eq!(
            parser()
                .parse("7")
                .into_result()
                .expect("parse failed")
                .inner,
            Expr::String("7")
        );
    }

    #[test]
    fn test_unquoted_word_rules() {
        // May not start with - or *
        assert!(parser().parse("-foo").into_result().is_err());
        assert!(parser().parse("*foo").into_result().is_err());

        // Cannot contain +
        assert!(parser().parse("foo+bar").into_result().is_err());

        // May contain + if starts with @@
        assert_eq!(
            parser()
                .parse("@@foo+bar")
                .into_result()
                .expect("parse failed")
                .inner,
            Expr::String("@@foo+bar")
        );
    }
}
