---
name: flint
description: Render a chart, graph, plot or 3D visualization inside an artifact — declare what the data MEANS and Flint derives axes, scales, formatting, colour and layout, instead of you hand-rolling SVG.
---

# Charts in an artifact

Do not hand-write SVG for a chart, and do not hand-write an ECharts option
either. Write a **Flint** spec — data plus what each field *means* plus which
chart you want — and let the compiler derive the axis types, tick formats,
aggregation, zero baselines, colour scheme, label rotation and sizing.

Flint (`flint-chart`, MIT, Microsoft) and ECharts are **vendored into bough** and
served from the same loopback origin as the artifact. No CDN, no network, works
offline — the artifact bar holds.

## The whole pattern

```html
<div id="chart" style="width:100%;height:380px"></div>
<script src="/artifacts/_lib/echarts.js"></script>
<script type="module">
import { assembleECharts } from '/artifacts/_lib/flint.js';

const rows = [
  { month: '2024-01', region: 'West', revenue: 120 },
  { month: '2024-02', region: 'West', revenue: 160 },
  { month: '2024-01', region: 'East', revenue:  90 },
  { month: '2024-02', region: 'East', revenue: 140 },
];

const option = assembleECharts({
  data: { values: rows },
  semantic_types: { month: 'YearMonth', region: 'Category', revenue: 'Amount' },
  chart_spec: {
    chartType: 'Line Chart',
    encodings: { x: { field: 'month' }, y: { field: 'revenue' }, color: { field: 'region' } },
  },
});

const chart = echarts.init(document.getElementById('chart'));
chart.setOption(option);
addEventListener('resize', () => chart.resize());
</script>
```

`semantic_types` is the part that earns its keep. Saying `month` is a
`YearMonth` — not just a string — is what produces a time axis with a sane tick
format; saying a field is `Percentage` or `Profit` is what stops it being
stacked or put on a sequential ramp when it diverges.

## Semantic types

Time — `DateTime` `Date` `Time` `Timestamp` `Year` `Quarter` `Month` `Week`
`Day` `Hour` `YearMonth` `YearQuarter` `YearWeek` `Decade` `Duration`

Measures — `Quantity` `Count` `Amount` `Price` `Percentage` `Temperature`
`Profit` `PercentageChange` `Sentiment` `Correlation` `Rank` `Score`

Geo — `Latitude` `Longitude` `Country` `State` `City` `Region` `Address`
`ZipCode`

Other — `Category` `Name` `Status` `Boolean` `Direction` `Range` `ID` `Number`
`Unknown`

A field can carry options instead of a bare string:

```js
region: { semanticType: 'Category', sortOrder: ['N', 'E', 'S', 'W'] }
```

## Chart types and their channels

Use the name **exactly**. Bind only the channels listed.

| Chart type | Channels |
|---|---|
| Scatter Plot | x y color size opacity column row |
| Regression | x y size color column row |
| Connected Scatter Plot | x y order color detail column row |
| Ranged Dot Plot | x y color |
| Boxplot | x y color opacity column row |
| Strip Plot | x y color size column row |
| Bar Chart | x y color opacity column row |
| Grouped Bar Chart | x y group color column row |
| Stacked Bar Chart | x y color column row |
| Lollipop Chart | x y color column row |
| Pyramid Chart | x y color |
| Heatmap | x y color column row |
| Calendar Heatmap | x color |
| Line Chart | x y color opacity column row |
| Bump Chart | x y color detail column row |
| Slope Chart | x y color detail column row |
| Area Chart | x y color opacity column row |
| Streamgraph | x y color column row |
| Range Area Chart | x y y2 color column row |
| Pie Chart | size color column row |
| Funnel Chart | y size |
| Treemap | color size detail |
| Sunburst Chart | color size detail group |
| Tree | color detail size |
| Histogram | x color column row |
| Density Plot | x color column row |
| ECDF Plot | x color detail column row |
| Parallel Coordinates | color detail |
| Candlestick Chart | x open high low close column row |
| Waterfall Chart | x y color column row |
| Gantt Chart | y x x2 color detail column row |
| Bullet Chart | y x goal color column row |
| Radar Chart | x y color column row |
| Rose Chart | x y color column row |
| Gauge Chart | size column |
| Sankey Diagram | x y size |
| Network Graph | x y size |

`column` and `row` facet — that is how you get small multiples, and it is
almost always better than cramming six series onto one pair of axes.

`baseSize` sets the box the layout solver works in:

```js
chart_spec: { chartType: 'Heatmap', encodings: { … }, baseSize: { width: 640, height: 400 } }
```

## Reshape the data first

Flint binds fields to channels. It does not join, pivot, filter or aggregate for
you. Do that in plain JavaScript before you build the spec — one row per mark,
columns named for what they are.

## 3D and anything Flint has no chart type for

Flint is 2D. For 3D, load `echarts-gl` on top of echarts and write the option by
hand — `bar3D`, `scatter3D`, `line3D`, `surface`, `globe`, plus `grid3D` and
`xAxis3D`/`yAxis3D`/`zAxis3D`:

```html
<script src="/artifacts/_lib/echarts.js"></script>
<script src="/artifacts/_lib/echarts-gl.js"></script>
```

Load `echarts-gl.js` **only** on a page that has a 3D mark; it is 640KB and
buys a flat chart nothing. Reach for 3D when the third axis carries real
information — a surface over two continuous inputs, a volumetric scatter. A 3D
bar chart of one-dimensional data is harder to read than the bar chart it came
from.

## Bar to hold

- **One `echarts.init` per container**, and `resize` on window resize, or the
  chart is a fixed-width image on a phone.
- **Give the container an explicit height.** ECharts measures its box; a
  height-less div renders nothing and looks like a broken script.
- **Let Flint pick colours.** Overriding the scheme per series is how a page
  ends up with six charts that share no visual language.
- Label the axes through the data — the field name becomes the axis title, so
  name the column `revenue_usd`, not `y`.
- If `assembleECharts` throws, the spec is wrong (unknown chart type, a channel
  that chart does not have, a field not in the data). Read the message; do not
  fall back to hand-written SVG.

## What this skill is not for

A single number, a two-row comparison, or a sparkline-sized trend belongs in
your reply text or a small table. A chart engine around three data points is
the "dressed-up thin content" failure the artifact bar already warns about.
