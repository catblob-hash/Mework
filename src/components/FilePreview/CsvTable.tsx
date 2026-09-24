import { useMemo, useState } from "react";
import { useI18n } from "../../i18n";
import { guessDelimiter, numericColumns, parseDelimited } from "../../lib/csv";

/** Rows drawn at once; a table longer than this is a job for a spreadsheet, not a pane. */
const MAX_ROWS = 5000;

function delimiterFor(path: string, text: string): string {
  const extension = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  if (extension === "tsv" || extension === "tab") return "\t";
  if (extension === "psv") return "|";
  return guessDelimiter(text);
}

/**
 * Delimited text as a table: a numbered row per record, the first record as the
 * header by default, and numbers right-aligned so a column of them can be read
 * down.
 */
export function CsvTable({ path, content }: { path: string; content: string }) {
  const { t } = useI18n();
  const [header, setHeader] = useState(true);
  const table = useMemo(() => parseDelimited(content, delimiterFor(path, content), MAX_ROWS + 1), [content, path]);
  const rows = table.rows.slice(0, MAX_ROWS + (header ? 1 : 0));
  const truncated = table.truncated || table.rows.length > rows.length;
  const width = rows.reduce((max, row) => Math.max(max, row.length), 0);
  const numeric = useMemo(() => numericColumns(rows, header ? 1 : 0), [header, rows]);
  const head = header ? rows[0] ?? [] : null;
  const body = header ? rows.slice(1) : rows;
  const columns = Array.from({ length: width }, (_, index) => index);

  return (
    <div className="file-preview file-preview--csv">
      <div className="file-preview__csv-bar">
        <label className="file-preview__csv-toggle">
          <input type="checkbox" checked={header} onChange={(event) => setHeader(event.target.checked)} />
          <span>{t("首行是表头", "First row is the header")}</span>
        </label>
        <span className="file-preview__meta">
          {t("{rows} 行 · {columns} 列", "{rows} rows · {columns} columns", { rows: body.length, columns: width })}
          {truncated ? ` · ${t("只显示了前 {count} 行", "only the first {count} rows are shown", { count: MAX_ROWS })}` : ""}
        </span>
      </div>
      <div className="file-preview__csv-scroll">
        <table className="file-preview__csv">
          {head && (
            <thead>
              <tr>
                <td className="file-preview__csv-index" />
                {columns.map((column) => (
                  <th key={column} className={numeric[column] ? "file-preview__csv-number" : undefined}>{head[column] ?? ""}</th>
                ))}
              </tr>
            </thead>
          )}
          <tbody>
            {body.map((row, index) => (
              <tr key={index}>
                <th className="file-preview__csv-index" scope="row">{index + 1}</th>
                {columns.map((column) => (
                  <td key={column} className={numeric[column] ? "file-preview__csv-number" : undefined}>{row[column] ?? ""}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
