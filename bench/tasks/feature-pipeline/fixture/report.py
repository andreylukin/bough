"""CLI: python3 report.py SALES_FILE -- print the regional sales report."""

import sys

from pipeline.aggregate import aggregate_records
from pipeline.load import load_records
from pipeline.render import render_report
from pipeline.transform import transform_records
from pipeline.validate import validate_records


def build_report(lines):
    records = load_records(lines)
    records = validate_records(records)
    records = transform_records(records)
    return render_report(aggregate_records(records))


def main():
    with open(sys.argv[1]) as f:
        print(build_report(f))


if __name__ == "__main__":
    main()
