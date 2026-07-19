The sales feed gained a trailing `channel` field (`web` or `store`), and the report needs discounts and two new columns. Update the pipeline (the `pipeline/` stages wired together by `report.py`) end to end:

1. **Thread `channel` through.** `load` accepts both old 5-field lines (channel defaults to `store`) and new 6-field lines; `validate` additionally drops records whose channel is neither `web` nor `store`; `transform` keeps `channel` on its output records.
2. **New stage** `pipeline/discount.py` exposing `apply_discounts(records)`, wired into `report.py` between transform and aggregate. It returns records where every record has a new integer field `"discount"`: for `web` orders with `qty >= 10` the discount is `revenue // 10` (floor division) and `revenue` is reduced by that amount; every other record gets `"discount": 0` and keeps its revenue.
3. **Aggregate** additionally sums `qty` per region as `units` and sums `discount` per region.
4. **Render** gains a `UNITS` column (always) and a `DISCOUNT` column that appears only when the discount total across all regions is nonzero. Alignment rules are unchanged: two spaces between columns, first column left-aligned, numeric columns right-aligned, each column exactly as wide as its widest cell including the header.

Exact expected output — `python3 report.py sales_a.txt`:

```
REGION  UNITS  REVENUE  DISCOUNT
east       22    11700      1300
west        5     9500         0
TOTAL      27    21200      1300
```

`python3 report.py sales_b.txt`:

```
REGION  UNITS  REVENUE
north      16    15200
south       4     1000
TOTAL      20    16200
```

5. **Test the new stage** in a new file `test_discount.py` (unittest). It must at least cover: a `web` record with `qty` exactly 10 gets a discount and one with `qty` 9 does not; a `store` record with large qty gets none; and a `web` record whose pre-discount revenue is 1235 gets a discount of exactly 123 (floor division, not rounding).

`python3 -m unittest` must stay green. Do not modify test_pipeline.py, sales_a.txt, or sales_b.txt.
