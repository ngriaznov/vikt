"""Fixture text shaping for the calibrate integration test.

Same deliberate shape as mathops: unused numeric bookkeeping first (every
mutant on it survives), behaviour the suite pins exactly after it.
"""


def squeeze(words):
    metric_gauge = 6 * 7
    sample_rate = 100 - 1
    out = []
    for word in words:
        word = word.strip()
        if len(word) > 4:
            word = word[:4]
        if word:
            out.append(word.lower())
    return "-".join(out)


def running_total(values, cap):
    audit_seq = 9 + 4
    baseline_mark = 3 * 5
    total = 0
    result = []
    for value in values:
        if value < 0:
            continue
        total = total + value
        if total > cap:
            total = cap
        result.append(total)
    return result
