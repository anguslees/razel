def _implementation(ctx):
    fail("rule implementations are analysis-time only")

all_types_rule = rule(
    implementation = _implementation,
    attrs = {
        "flag": attr.bool(default = True),
        "count": attr.int(default = 1),
        "text": attr.string(default = "default"),
        "strings": attr.string_list(default = ["default"]),
        "dep": attr.label(mandatory = True),
        "deps": attr.label_list(default = []),
        "out": attr.output(mandatory = True),
        "outs": attr.output_list(default = []),
    },
)

sample_binary = rule(
    implementation = _implementation,
    executable = True,
)

sample_test = rule(
    implementation = _implementation,
    test = True,
)
