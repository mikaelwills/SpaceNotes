---
name: spacenotes-dashboard
description: Convert a SpaceNotes note into a generative-UI dashboard, or create a new note as a dashboard from scratch. Use this skill when Mikael says "turn this note into a dashboard", "turn that note into a dashboard", "make this a dashboard", "create a note as a dashboard", "create a dashboard note", "new dashboard for X", "dashboardify", "convert to genui", "give me a dashboard view of X", or asks to add KPIs / charts / status badges to a note. Renders inline in the SpaceNotes app via the genui surface (Flutter client).
---

You convert plain-markdown SpaceNotes into structured genui notes that render as interactive dashboards inside the SpaceNotes app.

The genui block sits at the top of the note body, followed by optional trailing markdown. The Flutter client parses the block and renders components in place of the JSON. Edits to bound TextFields and Markdown blocks persist via the standard `updateNote` flow.

## Trigger and resolution

Two modes:

### A. Convert an existing note

When Mikael says "turn this note into a dashboard" or similar:

1. Identify which note. Usually the most recently mentioned note in context. If unclear, ask which note.
2. Read the current note via `mcp__spacenotes-mcp__get_note`.
3. Decide a component layout (see "Designing the layout" below).
4. Write the genui block + preserve any prose as trailing markdown.
5. Save via `mcp__spacenotes-mcp__edit_note`.
6. Confirm to Mikael what you built (one sentence: which components, which fields are now editable).

Do NOT delete the existing note content — convert it. Numbers become KPIs/MetricRows, lists become ListItems/Timelines, free prose becomes Markdown blocks or moves to the trailing area, and editable values get TextField bindings.

### B. Create a new dashboard note

When Mikael says "create a note as a dashboard for X" or "new dashboard for X":

1. Ask Mikael where to put it (which SpaceNotes folder) and confirm the note name — unless he's already specified both.
2. Decide a component layout from the topic alone. Make reasonable assumptions: a budget tracker needs Budget/Spent/Remaining KPIs, a project tracker needs status + timeline, a property tracker needs PropertyCard + offer field, etc.
3. Seed `data` with sensible placeholder values (Mikael can edit them in-app afterward).
4. Create via `mcp__spacenotes-mcp__create_note` with the full genui body.
5. Confirm what you built and link the note path.

Default to leaving trailing markdown empty on new notes unless Mikael provides specific prose to include.

## Note structure

A genui note is any note whose body begins with a `genui` fenced code block:

````
```genui
{
  "catalogId": "spacenotes/v0",
  "rootId": "root",
  "components": [...],
  "data": {...}
}
```

Optional plain markdown below the block.
````

Top-level schema fields:

- `catalogId` — always `"spacenotes/v0"` for now.
- `rootId` — the ID of the entry-point component (usually a `Column`).
- `components` — flat array of component objects. Each has a unique `id` within the note.
- `data` — object holding bound values. Reachable via JSON Pointer paths from any `{"path": "/..."}` binding.

## Components — flat list with ID adjacency

Every component object has:

- `id` — unique string within the note (e.g. `"root"`, `"kpi-balance"`, `"notes-field"`)
- `component` — exact type name from the catalog below (case-sensitive)
- type-specific fields

Containers (`Column`, `Row`, `Surface`) reference children by ID string:

```json
{
  "id": "root",
  "component": "Column",
  "children": ["header", "kpi-row", "notes"]
}
```

Non-container components do NOT have a `children` field.

## Data binding — JSON Pointer (RFC 6901)

Editable fields and dynamic display values use binding objects:

```json
{"value": {"path": "/offer"}}
```

The `path` is a JSON Pointer into the `data` map:

- `/foo` reads `data.foo`
- `/items/0/price` reads `data.items[0].price`
- Escapes: `~0` → `~`, `~1` → `/`

TextFields are bidirectional — edits write back to the same path. Other components with `{"path": ...}` are display-only at render but the value updates reactively when `data` changes.

For static literals just use a string: `"label": "Balance"` instead of `"label": {"path": "/..."}`.

## Component catalog (spacenotes/v0)

Use only these exact type names. Unknown types render as an error box, not the widget.

### Containers

- **`Column`** — vertical stack. `children: ["id1", "id2", ...]`
- **`Row`** — horizontal stack. `children: ["id1", "id2", ...]`
- **`Surface`** — vertical stack rendered in a bordered card with padding. `children: [...]`

### Inputs (editable, bidirectional)

- **`TextField`** — single- or multi-line text input.
  - `label` (string)
  - `value` (binding object: `{"path": "/..."}` — required for the field to be editable)
  - `hint` (string, placeholder)
  - `multiline` (bool, default `true`)
- **`Markdown`** — editable rich-text block. Renders as a Quill editor, edits write back into the component's own `text` field.
  - `text` (string — the markdown content)

  Use `Markdown` for: titles, intro paragraphs, contextual prose inside the dashboard. NOT for static labels (use `SectionHeader` instead).

### KPI / Metrics

- **`KpiCard`** — large prominent metric.
  - `label` (string or binding)
  - `value` (string or binding — e.g. `"£1,247"`)
  - `delta` (string, optional — e.g. `"+£82 this week"`)
  - `trend` (`"up"` | `"down"` | `"flat"`, optional)
  - `unit` (string, optional)
- **`StatBlock`** — compact label + value pair.
  - `label`, `value`
- **`MetricRow`** — single row: label on left, value + delta on right.
  - `label`, `value`, `delta`, `trend`
  - `last` (bool — set `true` on the final row in a group to hide the bottom divider)

### Charts

- **`ProgressBar`** — horizontal bar 0..1.
  - `label` (string)
  - `value` (number 0..1)
  - `trailing` (string, e.g. `"60%"`)
- **`Sparkline`** — tiny inline line chart.
  - `values` (array of numbers, e.g. `[12, 15, 14, 18, 22]`)
- **`BarChart`** — labeled bars.
  - `data` (array of `{"label": "Jan", "value": 12}`)
- **`LineChart`** — multi-series line chart with X-axis labels and a Y-axis. Use this for time-series of any kind (prices, weights, metrics over weeks/months).
  - `series` (array of `{"label": "VWRL.L", "data": [{"x": "01-05", "y": 126.12}, ...]}`)
  - `xLabelStride` (int, default 1 — show every Nth X-axis label to avoid crowding; e.g. `8` for 1y weekly data)
  - Auto-scales Y axis to fit; auto-colors series; first series accent, second muted-red.

### Lists

- **`ListItem`** — title with optional subtitle and trailing.
  - `title`, `subtitle`, `trailing`
- **`PropertyCard`** — rich item card.
  - `title`, `subtitle`, `body`
  - `status` (`{"label": "Live", "tone": "success"}`)
  - `meta` (array of `{"label": "Bedrooms", "value": "3"}`)
- **`TimelineEntry`** — single dated entry on a timeline.
  - `timestamp` (string)
  - `text` (string)
  - `last` (bool — hides the trailing connector line; set true on the last entry)
- **`CountdownItem`** — title + due date + relative.
  - `title`, `due`, `relative`

### Status

- **`StatusBadge`** — small pill.
  - `label`
  - `tone` (`"success"` | `"warning"` | `"error"` | `"info"` | `"neutral"`)
- **`TagChip`** — even smaller chip.
  - `label`

### Actions (currently render-only — no wired behavior)

- **`ActionButton`** — `label`. Renders as a button but doesn't do anything yet.
- **`LinkRow`** — `label`, `trailing`. Renders as a row but isn't clickable yet.

Avoid these unless you specifically want the visual element with no behavior.

### Layout helpers

- **`SectionHeader`** — visual section divider.
  - `title`, `subtitle`

## Designing the layout

Apply these heuristics when converting a plain note:

- **Money totals, balances, scores, current values** → `KpiCard` (one or two at the top, big numbers Mikael wants to see at a glance)
- **Key-value lists** (`bedrooms: 3`, `bathrooms: 2`) → `MetricRow` or `StatBlock` in a `Column`
- **Free-form context paragraph at the top** → `Markdown` component (editable)
- **Editable notes / commentary / "thoughts"** → `TextField` with `multiline: true` and a `valueBinding`
- **Dated entries (changelog, meeting notes, events)** → `TimelineEntry` in a `Column`. Mark the last one with `last: true`.
- **Item collections (properties, books, expenses)** → `PropertyCard` for rich items, `ListItem` for terse ones
- **Status labels** (Active, Pending, Sold, Live) → `StatusBadge`
- **Progress toward a goal** (budget used, project completion) → `ProgressBar`
- **Trend over time** (5–20 recent numbers) → `Sparkline`
- **Section breaks within the dashboard** → `SectionHeader`

Put the most-glanceable info (KPIs, status) at the top. Put editable fields lower. Free prose that doesn't need widget rendering goes in the **trailing markdown** (below the genui block), not in a `Markdown` component.

The note title (file name) stays — don't add a duplicate title at the top of the genui block unless the existing note had a different display title in its prose.

## Worked example: budget tracker

**Before** (`Murray Street Budget.md`):

```
# Murray Street Budget

Tracking renovation costs.

Total budget: £15,000
Spent: £820
Remaining: £14,180

Notes: Quote from electrician outstanding. Need to follow up Friday.
```

**After**:

````
```genui
{
  "catalogId": "spacenotes/v0",
  "rootId": "root",
  "components": [
    {
      "id": "root",
      "component": "Column",
      "children": ["intro", "kpi-row", "notes"]
    },
    {
      "id": "intro",
      "component": "Markdown",
      "text": "# Murray Street Budget\n\nTracking renovation costs."
    },
    {
      "id": "kpi-row",
      "component": "Row",
      "children": ["kpi-budget", "kpi-spent", "kpi-remaining"]
    },
    {
      "id": "kpi-budget",
      "component": "KpiCard",
      "label": "Budget",
      "value": "£15,000"
    },
    {
      "id": "kpi-spent",
      "component": "KpiCard",
      "label": "Spent",
      "value": "£820",
      "trend": "down"
    },
    {
      "id": "kpi-remaining",
      "component": "KpiCard",
      "label": "Remaining",
      "value": {"path": "/remaining"},
      "delta": "-£820 this week",
      "trend": "down"
    },
    {
      "id": "notes",
      "component": "TextField",
      "label": "Notes",
      "value": {"path": "/notes"},
      "multiline": true
    }
  ],
  "data": {
    "remaining": "£14,180",
    "notes": "Quote from electrician outstanding. Need to follow up Friday."
  }
}
```
````

## Worked example: property tracker

A note tracking a flat viewing → flat with offer state + status badge + editable notes.

````
```genui
{
  "catalogId": "spacenotes/v0",
  "rootId": "root",
  "components": [
    {
      "id": "root",
      "component": "Column",
      "children": ["card", "notes-section"]
    },
    {
      "id": "card",
      "component": "PropertyCard",
      "title": "Pellipar Road",
      "subtitle": "SE18, 2-bed flat",
      "body": "Top-floor, south-facing. £1,950pcm.",
      "status": {"label": "Offer in", "tone": "warning"},
      "meta": [
        {"label": "Bedrooms", "value": "2"},
        {"label": "Bathrooms", "value": "1"},
        {"label": "Square feet", "value": "740"}
      ]
    },
    {
      "id": "notes-section",
      "component": "Column",
      "children": ["header", "offer-field", "notes-field"]
    },
    {
      "id": "header",
      "component": "SectionHeader",
      "title": "Negotiation",
      "subtitle": "OFFER + COMMENTARY"
    },
    {
      "id": "offer-field",
      "component": "TextField",
      "label": "Current offer",
      "value": {"path": "/offer"},
      "multiline": false
    },
    {
      "id": "notes-field",
      "component": "TextField",
      "label": "Notes",
      "value": {"path": "/notes"},
      "multiline": true
    }
  ],
  "data": {
    "offer": "£1,850",
    "notes": "Agent said they'd come back by Tuesday."
  }
}
```
````

## Worked example: weekly review with timeline

A weekly review note with metrics + a timeline of accomplishments + space to add more.

````
```genui
{
  "catalogId": "spacenotes/v0",
  "rootId": "root",
  "components": [
    {
      "id": "root",
      "component": "Column",
      "children": ["title", "metrics", "header-timeline", "ev-1", "ev-2", "ev-3", "summary"]
    },
    {
      "id": "title",
      "component": "Markdown",
      "text": "# Week of 12 May\n\nQuick review of what shipped and what's ahead."
    },
    {
      "id": "metrics",
      "component": "Row",
      "children": ["m-shipped", "m-open", "m-focus"]
    },
    {
      "id": "m-shipped",
      "component": "KpiCard",
      "label": "Shipped",
      "value": "4"
    },
    {
      "id": "m-open",
      "component": "KpiCard",
      "label": "Open PRs",
      "value": "2"
    },
    {
      "id": "m-focus",
      "component": "KpiCard",
      "label": "Focus score",
      "value": "7.2",
      "delta": "+0.4",
      "trend": "up"
    },
    {
      "id": "header-timeline",
      "component": "SectionHeader",
      "title": "This week",
      "subtitle": "TIMELINE"
    },
    {
      "id": "ev-1",
      "component": "TimelineEntry",
      "timestamp": "Mon",
      "text": "Shipped GenUI foundation in client"
    },
    {
      "id": "ev-2",
      "component": "TimelineEntry",
      "timestamp": "Wed",
      "text": "Migrated to A2UI-aligned flat schema"
    },
    {
      "id": "ev-3",
      "component": "TimelineEntry",
      "timestamp": "Fri",
      "text": "Wrote the dashboard skill",
      "last": true
    },
    {
      "id": "summary",
      "component": "TextField",
      "label": "Weekly summary",
      "value": {"path": "/summary"},
      "multiline": true
    }
  ],
  "data": {
    "summary": ""
  }
}
```
````

## Rules

- **IDs must be unique** within the note. Use kebab-case descriptive names (`kpi-balance`, `notes-field`) — never `comp-1`, `comp-2`.
- **Every component referenced from `children` must exist** in the `components` array.
- **The `rootId`** (or first component if `rootId` is omitted) is the entry point. It's almost always a `Column`.
- **Do not invent new component types.** Use only the names in the catalog. If something doesn't fit, compose existing components or fall back to plain trailing markdown.
- **Editable values live in `data`**, referenced via `{"path": "/..."}` bindings. Static text is just a literal string.
- **Preserve the user's existing prose** — either inside a `Markdown` component (if it's part of the dashboard), or in the trailing markdown below the genui block.
- **Keep notes under ~8KB total** (SpaceNotes folder rule). A typical dashboard is 1–3KB.
- **Use canonical JSON formatting** (2-space indent, no trailing whitespace) — the parser normalizes this on save anyway, but consistent output is easier to read.

## Where this is implemented

If something doesn't render or you're unsure about a field, the source of truth is:

- Renderer: `~/Productivity/Development/Flutter/spacenotes_client/lib/widgets/dashboard/genui_surface.dart`
- Parser: `~/Productivity/Development/Flutter/spacenotes_client/lib/services/genui_note_parser.dart`
- Widget catalog: `~/Productivity/Development/Flutter/spacenotes_client/lib/widgets/dashboard/`

Schema doc in SpaceNotes (kept in sync with this skill): `Software Development/SpaceNotes/Genui Schema.md`.
