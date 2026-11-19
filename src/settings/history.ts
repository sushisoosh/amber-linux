import type { HistoryEntry } from "../shared/types";

export interface HistoryFilters {
  query: string;
  program: string | null;
  date: "all" | "today" | "7days" | "30days";
  status: "all" | "inserted" | "not-inserted";
}

export const DEFAULT_HISTORY_FILTERS: Readonly<HistoryFilters> = {
  query: "", program: null, date: "all", status: "all",
};

export function historyPrograms(entries: readonly HistoryEntry[]) {
  const programs = new Map<string, string>();
  for (const entry of entries) {
    const label = entry.program.trim();
    const value = label.toLowerCase();
    if (!programs.has(value)) programs.set(value, label || "Unknown program");
  }
  return Array.from(programs, ([value, label]) => ({ value, label }))
    .sort((a, b) => a.label.localeCompare(b.label));
}

export function filterHistory(entries: readonly HistoryEntry[], filters: Readonly<HistoryFilters>, now: number) {
  const query = filters.query.trim().toLowerCase();
  const program = filters.program?.trim().toLowerCase();
  let from = -Infinity;
  let until = Infinity;
  if (filters.date !== "all") {
    const start = new Date(now);
    start.setHours(0, 0, 0, 0);
    const end = new Date(start);
    end.setDate(end.getDate() + 1);
    // Calendar arithmetic keeps midnight boundaries correct across DST changes.
    start.setDate(start.getDate() - (filters.date === "7days" ? 6 : filters.date === "30days" ? 29 : 0));
    from = start.getTime();
    until = end.getTime();
  }
  return entries.filter((entry) => {
    const entryProgram = entry.program.trim().toLowerCase();
    return (program === undefined || entryProgram === program)
      && (!query || entry.text.toLowerCase().includes(query) || (entryProgram || "unknown program").includes(query))
      && (filters.date === "all" || (entry.at >= from && entry.at < until))
      && (filters.status === "all" || entry.inserted === (filters.status === "inserted"));
  });
}
