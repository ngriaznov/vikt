"""Cross-file hub, Python side: py_hub does real work and calls py_helper (its
own file), and is called from wrapper_a.py and wrapper_b.py — two *different*
files. --scope file can only build a call graph within one file at a time, so
under file scope py_hub, wrapper_a and wrapper_b each look like a lone
function with no callers or callees; only --scope repo, which lets the call
graph and the re-rank both cross file boundaries, can see wrapper_a.py and
wrapper_b.py calling into this file at all.
"""


def py_hub(x):
    a = x + 1
    b = a * 2
    c = b - 3
    return py_helper(c) + a


def py_helper(x):
    return x * 2
