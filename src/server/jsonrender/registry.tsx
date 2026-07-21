/**
 * React implementations of the artifact UI catalog (catalog.ts) — the browser half,
 * compiled into the viewer bundle (bundle.ts) and rendered by viewer.tsx. Class
 * names come from styles.ts. Table headers sort on click; bars are direct-labeled;
 * everything else is intentionally static, dense markup.
 */
import { Fragment, useState } from "react";
import { defineRegistry } from "@json-render/react";
import { catalog } from "./catalog.ts";

interface Column {
  key: string;
  label: string;
  align?: "left" | "right";
}
type Cell = string | number | boolean | null;

function SortableTable(
  { columns, rows, caption }: { columns: Column[]; rows: Record<string, Cell>[]; caption?: string },
) {
  const [sort, setSort] = useState<{ key: string; dir: 1 | -1 } | null>(null);
  const sorted = sort
    ? [...rows].sort((a, b) => {
      const [x, y] = [a[sort.key], b[sort.key]];
      if (typeof x === "number" && typeof y === "number") return (x - y) * sort.dir;
      return String(x ?? "").localeCompare(String(y ?? "")) * sort.dir;
    })
    : rows;
  const toggle = (key: string) =>
    setSort((s) => (s?.key === key && s.dir === 1 ? { key, dir: -1 } : { key, dir: 1 }));
  return (
    <div className="b-tablewrap">
      <table className="b-table">
        {caption && <caption>{caption}</caption>}
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} className={c.align} onClick={() => toggle(c.key)}>
                {c.label}
                {sort?.key === c.key ? (sort.dir === 1 ? " ↑" : " ↓") : ""}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {sorted.map((row, i) => (
            <tr key={i}>
              {columns.map((c) => (
                <td key={c.key} className={c.align}>{String(row[c.key] ?? "")}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export const { registry } = defineRegistry(catalog, {
  components: {
    Page: ({ props, children }) => (
      <div className="b-page">
        <header>
          <h1>{props.title}</h1>
          {props.subtitle && <p className="b-subtitle">{props.subtitle}</p>}
        </header>
        {children}
      </div>
    ),
    Section: ({ props, children }) => (
      <section className="b-section">
        {props.title && <h2>{props.title}</h2>}
        {props.hint && <p className="b-hint">{props.hint}</p>}
        {children}
      </section>
    ),
    Columns: ({ props, children }) => (
      <div
        className="b-columns"
        style={{ gridTemplateColumns: `repeat(${props.count ?? 2}, 1fr)` }}
      >
        {children}
      </div>
    ),
    Stat: ({ props }) => (
      <div className="b-stat">
        <div className="b-label">{props.label}</div>
        <div className="b-value">{props.value}</div>
        {props.delta && <div className={`b-delta ${props.intent ?? "neutral"}`}>{props.delta}</div>}
      </div>
    ),
    Text: ({ props }) => (
      <p className={`b-text${props.muted ? " muted" : ""}${props.mono ? " mono" : ""}`}>
        {props.text}
      </p>
    ),
    Callout: ({ props }) => (
      <div className={`b-callout ${props.intent}`}>
        {props.title && <span className="b-title">{props.title}</span>}
        {props.text}
      </div>
    ),
    Badge: ({ props }) => <span className={`b-badge ${props.intent ?? ""}`}>{props.label}</span>,
    Table: ({ props }) => (
      <SortableTable columns={props.columns} rows={props.rows} caption={props.caption} />
    ),
    KeyValue: ({ props }) => (
      <div className="b-kv">
        {props.pairs.map((p, i) => (
          <Fragment key={i}>
            <div className="b-k">{p.key}</div>
            <div>{p.value}</div>
          </Fragment>
        ))}
      </div>
    ),
    List: ({ props }) =>
      props.ordered
        ? <ol className="b-list">{props.items.map((it, i) => <li key={i}>{it}</li>)}</ol>
        : <ul className="b-list">{props.items.map((it, i) => <li key={i}>{it}</li>)}</ul>,
    Code: ({ props }) => (
      <div className="b-code">
        {(props.title || props.lang) && (
          <div className="b-codetitle">{props.title ?? props.lang}</div>
        )}
        <pre>{props.code}</pre>
      </div>
    ),
    BarChart: ({ props }) => {
      const max = Math.max(...props.bars.map((b) => Math.abs(b.value)), 1e-9);
      return (
        <div className="b-chart">
          {props.title && <div className="b-title">{props.title}</div>}
          {props.bars.map((b, i) => (
            <div key={i} className="b-row" title={`${b.label}: ${b.value}${props.unit ?? ""}`}>
              <span className="b-barlabel">{b.label}</span>
              <span className="b-bar" style={{ width: `${(Math.abs(b.value) / max) * 100}%` }} />
              <span className="b-barvalue">
                {b.value}
                {props.unit ?? ""}
              </span>
            </div>
          ))}
        </div>
      );
    },
    Link: ({ props }) => <a href={props.href}>{props.label}</a>,
    Divider: () => <hr />,
    Image: ({ props }) => <img src={props.src} alt={props.alt ?? ""} />,
  },
});
