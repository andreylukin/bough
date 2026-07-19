import unittest

from pipeline.aggregate import aggregate_records
from pipeline.load import load_records
from pipeline.render import render_report
from pipeline.transform import transform_records
from pipeline.validate import validate_records


def prepared(lines):
    return transform_records(validate_records(load_records(lines)))


class TestLoad(unittest.TestCase):
    def test_parses_fields(self):
        rec = load_records(["2026-01-05 east widget 12 500"])[0]
        self.assertEqual(rec["date"], "2026-01-05")
        self.assertEqual(rec["region"], "east")
        self.assertEqual(rec["product"], "widget")
        self.assertEqual(rec["qty"], "12")
        self.assertEqual(rec["price"], "500")

    def test_skips_blank_lines(self):
        self.assertEqual(load_records(["", "   ", "\n"]), [])


class TestValidate(unittest.TestCase):
    def test_drops_bad_qty_and_short_lines(self):
        lines = [
            "2026-01-05 east widget twelve 500",
            "2026-01-06 west gadget 3",
            "2026-01-07 west gadget 3 1500",
        ]
        recs = validate_records(load_records(lines))
        self.assertEqual(len(recs), 1)
        self.assertEqual(recs[0]["product"], "gadget")


class TestTransform(unittest.TestCase):
    def test_revenue_in_cents(self):
        rec = prepared(["2026-01-05 east widget 12 500"])[0]
        self.assertEqual(rec["qty"], 12)
        self.assertEqual(rec["revenue"], 6000)


class TestAggregate(unittest.TestCase):
    def test_totals_by_region(self):
        recs = prepared(
            [
                "2026-01-05 east widget 12 500",
                "2026-01-06 east gadget 3 1500",
                "2026-01-07 west cable 1 3200",
            ]
        )
        regions = aggregate_records(recs)
        self.assertEqual(regions["east"]["revenue"], 10500)
        self.assertEqual(regions["west"]["revenue"], 3200)


class TestRender(unittest.TestCase):
    def test_header_and_total_row(self):
        regions = aggregate_records(prepared(["2026-01-05 east widget 2 500"]))
        lines = render_report(regions).splitlines()
        self.assertTrue(lines[0].startswith("REGION"))
        self.assertTrue(lines[-1].startswith("TOTAL"))


if __name__ == "__main__":
    unittest.main()
