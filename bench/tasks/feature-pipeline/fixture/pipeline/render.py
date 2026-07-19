"""Stage 5: render the aggregated report as an aligned text table.

Layout rules: columns separated by two spaces; the first column left-aligned,
numeric columns right-aligned; every column exactly as wide as its widest cell
(header included). Regions sort alphabetically; a TOTAL row closes the table.
"""


def render_report(regions):
    headers = ["REGION", "REVENUE"]
    rows = [[name, str(regions[name]["revenue"])] for name in sorted(regions)]
    rows.append(["TOTAL", str(sum(regions[name]["revenue"] for name in regions))])
    widths = [max(len(h), *(len(row[i]) for row in rows)) for i, h in enumerate(headers)]
    lines = [
        headers[0].ljust(widths[0])
        + "".join("  " + h.rjust(w) for h, w in zip(headers[1:], widths[1:]))
    ]
    for row in rows:
        lines.append(
            row[0].ljust(widths[0])
            + "".join("  " + c.rjust(w) for c, w in zip(row[1:], widths[1:]))
        )
    return "\n".join(lines)
