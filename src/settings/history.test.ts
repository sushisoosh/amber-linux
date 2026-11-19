/// <reference types="node" />

import assert from "node:assert/strict";
import test from "node:test";
import type { HistoryEntry } from "../shared/types";
import { DEFAULT_HISTORY_FILTERS, filterHistory, historyPrograms, type HistoryFilters } from "./history";

const now = new Date(2026, 10, 19, 12).getTime();
const entry = (changes: Partial<HistoryEntry> = {}): HistoryEntry => ({
  at: now, text: "Meeting notes", program: "Editor.exe", engine: "local", inserted: true, ...changes,
});
const filter = (entries: readonly HistoryEntry[], filters: Partial<HistoryFilters> = {}) =>
  filterHistory(entries, { ...DEFAULT_HISTORY_FILTERS, ...filters }, now);

test("History search matches text and programs case-insensitively and trims the query", () => {
  const entries = [entry(), entry({ text: "Other text", program: "Browser.exe" })];
  assert.deepEqual(filter(entries, { query: "  mEeTiNg  " }), [entries[0]]);
  assert.deepEqual(filter(entries, { query: "BROWSER.EXE" }), [entries[1]]);
  assert.deepEqual(filter(entries, { query: "  " }), entries);
  assert.deepEqual(filter(entries, { query: "no match" }), []);
  assert.equal(filter([entry({ text: "\u00dcber das Projekt" })], { query: "\u00fcBER" }).length, 1);
});

test("History combines search, exact program, date and insertion filters", () => {
  const entries = [
    entry(),
    entry({ text: "Unrelated" }),
    entry({ program: "Browser.exe" }),
    entry({ at: new Date(2026, 10, 18, 23, 59, 59, 999).getTime() }),
    entry({ inserted: false }),
    entry({ program: "Other Editor.exe" }),
  ];
  assert.deepEqual(filter(entries, { query: "NOTES", program: "EDITOR.EXE", date: "today", status: "inserted" }), [entries[0]]);
  assert.deepEqual(filter(entries, { query: "notes", program: "editor.exe", date: "today", status: "not-inserted" }), [entries[4]]);
  assert.deepEqual(filter(entries, { program: "not saved.exe" }), []);
});

test("Unknown programs are searchable and selectable without colliding with all programs", () => {
  const entries = [entry({ program: "" }), entry({ program: " \t " }), entry(), entry({ program: "Unknown program" })];
  assert.deepEqual(filter(entries, { program: "" }), entries.slice(0, 2));
  assert.deepEqual(filter(entries, { query: "UNKNOWN PROGRAM" }), [entries[0], entries[1], entries[3]]);
  assert.deepEqual(filter(entries, { program: "unknown program" }), [entries[3]]);
  assert.deepEqual(filter(entries, { program: null }), entries);
});

test("Program options use the full history, deduplicate case and include unknown programs", () => {
  const entries = [entry({ program: "Zulu.exe" }), entry({ program: "editor.exe" }), entry({ program: " EDITOR.EXE " }), entry({ program: "" })];
  assert.deepEqual(historyPrograms(entries), [
    { value: "editor.exe", label: "editor.exe" },
    { value: "", label: "Unknown program" },
    { value: "zulu.exe", label: "Zulu.exe" },
  ]);
  assert.deepEqual(historyPrograms([]), []);
});

test("Date filters include local midnight and exclude the next day across month and DST boundaries", () => {
  // Run in different TZs as well: these dates span US and European clock changes.
  for (const current of [new Date(2026, 0, 2, 12), new Date(2026, 2, 9, 12), new Date(2026, 2, 30, 12), new Date(2026, 10, 2, 12)]) {
    for (const [date, days] of [["today", 1], ["7days", 7], ["30days", 30]] as const) {
      const from = new Date(current.getFullYear(), current.getMonth(), current.getDate() - days + 1).getTime();
      const until = new Date(current.getFullYear(), current.getMonth(), current.getDate() + 1).getTime();
      const entries = [entry({ at: from - 1 }), entry({ at: from }), entry({ at: current.getTime() }), entry({ at: until - 1 }), entry({ at: until })];
      assert.deepEqual(filterHistory(entries, { ...DEFAULT_HISTORY_FILTERS, date }, current.getTime()), entries.slice(1, 4), `${date} at ${current}`);
      assert.deepEqual(filterHistory(entries, DEFAULT_HISTORY_FILTERS, current.getTime()), entries);
    }
  }
});

test("Filtering and program options preserve source order, objects, filters and default state", () => {
  const entries = Object.freeze([
    Object.freeze(entry({ at: now - 1, program: "Zulu.exe" })),
    Object.freeze(entry({ at: now + 1, program: "Editor.exe" })),
  ]);
  const filters = Object.freeze({ ...DEFAULT_HISTORY_FILTERS, query: "NOTES" });
  const before = structuredClone(entries);
  const result = filterHistory(entries, filters, now);
  historyPrograms(entries);
  assert.deepEqual(entries, before);
  assert.notEqual(result, entries);
  assert.equal(result[0], entries[0]);
  assert.equal(result[1], entries[1]);
  assert.deepEqual(filters, { query: "NOTES", program: null, date: "all", status: "all" });
  assert.deepEqual(DEFAULT_HISTORY_FILTERS, { query: "", program: null, date: "all", status: "all" });
  assert.deepEqual(filter([]), []);
});
