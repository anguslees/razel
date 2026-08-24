def _implementation(ctx):
    fail("rule implementations are analysis-time only")

all_types_rule = rule(
    implementation = _implementation,
    attrs = {
        "flag": attr.bool(default = True),
        "count": attr.int(default = 1),
        "counts": attr.int_list(default = [1]),
        "text": attr.string(default = "default"),
        "strings": attr.string_list(default = ["default"]),
        "string_map": attr.string_dict(default = {"key": "value"}),
        "string_lists": attr.string_list_dict(default = {"key": ["value"]}),
        "dep": attr.label(mandatory = True),
        "deps": attr.label_list(default = []),
        "keyed_strings": attr.label_keyed_string_dict(default = {}),
        "grouped_deps": attr.label_list_dict(default = {}),
        "named_deps": attr.string_keyed_label_dict(default = {}),
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
