def _implementation(ctx):
    pass

output_rule = rule(
    implementation = _implementation,
    attrs = {
        "out": attr.output(mandatory = True),
    },
)
