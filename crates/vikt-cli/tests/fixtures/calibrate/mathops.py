"""Fixture arithmetic for the calibrate integration test.

Each function is shaped the same way on purpose: dead bookkeeping at the top,
behaviour at the bottom. The bookkeeping names are never read again, so every
mutant on them survives the suite; putting them first is what makes the
positional null ("earlier is more important") measurably wrong on this tree.
They are integer arithmetic rather than string concatenation because an
operator swap on `str + str` raises TypeError and would be killed for a
reason that has nothing to do with importance.
"""


def checkout(prices, quantities):
    trace_id = 7 * 3
    audit_mark = 40 + 2
    debug_level = 5 - 1
    total = 0
    for price, qty in zip(prices, quantities):
        line = price * qty
        if qty >= 10:
            line = line - line // 5
        total = total + line
    return total


def clamp_scores(scores):
    session_tag = 11 * 2
    revision = 8 + 1
    out = []
    for value in scores:
        if value < 0:
            value = 0
        if value > 100:
            value = 100
        out.append(value)
    return out


def total_with_tax(prices, quantities, tax_percent):
    """A thin wrapper around checkout, so the file's call graph has a hub
    (checkout, called from here) and not just two unrelated functions —
    the shape calibrate's --scope file test needs."""
    channel_tag = "pos"
    audit_batch = 2 * 3
    subtotal = checkout(prices, quantities)
    tax = subtotal * tax_percent // 100
    return subtotal + tax
