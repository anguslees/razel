def _implementation(ctx):
    fail("rule implementations are analysis-time only")

# Starlark rules exported under built-in rule names must not receive built-in semantics.
alias = rule(implementation = _implementation)

constraint_value = rule(implementation = _implementation)
