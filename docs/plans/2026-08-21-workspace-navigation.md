# Workspace Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use pane-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the UI selects in three steps — `workspace -> tab -> pane` — instead of the two it has today. `/` lists workspaces, `/w/<workspace>` lists that workspace's tabs, `/w/<workspace>/t/<tab>` lists that tab's panes, `/w/<workspace>/t/<tab>/p/<pane>` is one pane's log.

**Architecture:** herdr's `session.snapshot` already carries a `workspaces` array (`workspace_id`, `label`) and every tab already carries `workspace_id`, so this is a reshape at the same seam that already groups panes under tabs — no new herdr call, no new socket method. `src/herdr.rs` nests one level deeper; the web client mirrors the shape and grows one route segment.

**Tech Stack:** Rust (axum 0.8, serde), TypeScript, Astro, Vitest.

**Spec:** none — this is a direct user directive: "add a new step before tab selection and make 3 steps: workspace -> tab -> pane in total."

## Global Constraints

- `aube` is the only web package manager — never `npm`/`pnpm`/`yarn`.
- TypeScript `any` is prohibited.
- Artful simplicity per `.claude/skills/artful-simplicity/SKILL.md`: no speculative flexibility. A workspace level, not a generic tree.
- Comment style: sparse, constraint-stating, matching the existing voice in each file.
- Rust edition 2024, axum 0.8.
- Labels are terminal titles, never markup: `textContent` only in the UI.

## The pinned contract

Both tracks build to this exact JSON. It is the whole interface between them; neither may change it unilaterally.

```json
{
  "workspaces": [
    {
      "id": "w2",
      "label": "aituber-v1",
      "tabs": [
        {
          "id": "w2:t1",
          "label": "backend",
          "panes": [
            { "id": "w2:p1", "label": "orchestrator", "agent": "claude", "state": "idle" }
          ]
        }
      ]
    }
  ]
}
```

`Pane` is unchanged from today. `Tab` is unchanged except that it is now nested inside a workspace. `Session` is `{ workspaces }` where it was `{ tabs }`.

URL shapes, in full:

| path | shows |
|---|---|
| `/` | every workspace |
| `/w/<workspaceId>` | that workspace's tabs |
| `/w/<workspaceId>/t/<tabId>` | that tab's panes |
| `/w/<workspaceId>/t/<tabId>/p/<paneId>` | that pane's log |

Anything else reads as the root, exactly as today. Ids keep their colons through `encodeURIComponent`.

## Tracks

| Track | Goal | Tasks | Owned files | Depends on |
|---|---|---|---|---|
| `api` | `/api/session` nests panes under tabs under workspaces | 1 | `src/herdr.rs` | — |
| `ui` | three-step navigation over the pinned contract | 2, 3 | `web/src/lib/route.ts`, `web/src/lib/api.ts`, `web/src/pages/index.astro` | — |

Both tracks branch from the coordination branch `automated-deploy`, not from `main`: the deploy work already rewrote `src/main.rs`'s router, and branching from `main` would guarantee a conflict in that region for no benefit.

**Integration order:** either order. The tracks share no file, and the contract above is what makes them independent.

**Post-integration follow-up:** the README's API section and its UI-paths paragraph (`README.md:25-31`) describe the two-step shape; they are updated once, after both tracks land, alongside the deploy documentation.

---

### Task 1: Nest tabs under workspaces in the session reshape

**Track:** `api`

**Files:**
- Modify: `src/herdr.rs` (the deserialize structs, the exposed types, `to_session`, and the `groups_panes_under_their_tab` test)

**Interfaces:**
- Produces: `Session { workspaces: Vec<Workspace> }`, `Workspace { id, label, tabs }`. `Tab` and `Pane` keep their current fields.
- Consumed by: the `ui` track, through the pinned contract only.

- [ ] **Step 1: Write the failing test** — replace `groups_panes_under_their_tab` with a workspace-aware version, and extend the fixture with a second workspace and an empty one:

```rust
    fn snapshot_fixture() -> Snapshot {
        serde_json::from_value(json!({
            "workspaces": [
                { "workspace_id": "w1", "label": "backend" },
                { "workspace_id": "w2", "label": "frontend" },
                { "workspace_id": "w3", "label": "empty workspace" }
            ],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "backend" },
                { "tab_id": "w1:t2", "workspace_id": "w1", "label": "empty" },
                { "tab_id": "w2:t1", "workspace_id": "w2", "label": "web" },
                { "tab_id": "w3:t1", "workspace_id": "w3", "label": "orphan" }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "agent": "claude",
                  "agent_status": "idle", "title": "orchestrator" },
                { "pane_id": "w1:p2", "tab_id": "w1:t1", "agent": null,
                  "agent_status": "unknown", "terminal_title_stripped": "  " },
                { "pane_id": "w1:p4", "tab_id": "w1:t1", "agent": null,
                  "agent_status": "idle", "label": "renamed",
                  "title": "ignored terminal title" },
                { "pane_id": "w2:p1", "tab_id": "w2:t1", "agent": "codex",
                  "agent_status": "working", "title": "ui" },
                { "pane_id": "w1:p3", "tab_id": "w1:tX", "agent": null,
                  "agent_status": "unknown" }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn groups_panes_under_their_tab_and_workspace() {
        let session = to_session(snapshot_fixture());

        // The workspace whose only tab holds no pane is dropped, as is the
        // empty tab inside a workspace that survives.
        assert_eq!(session.workspaces.len(), 2);
        let backend = &session.workspaces[0];
        assert_eq!(backend.id, "w1");
        assert_eq!(backend.label, "backend");
        assert_eq!(backend.tabs.len(), 1);

        let tab = &backend.tabs[0];
        assert_eq!(tab.id, "w1:t1");
        // The pane whose tab is not listed is still dropped.
        assert_eq!(tab.panes.len(), 3);
        assert_eq!(
            tab.panes[0],
            Pane {
                id: "w1:p1".into(),
                label: "orchestrator".into(),
                agent: Some("claude".into()),
                state: "idle".into(),
            }
        );
        // Blank titles fall through to the pane id rather than rendering empty.
        assert_eq!(tab.panes[1].label, "w1:p2");
        assert_eq!(tab.panes[1].agent, None);
        // A herdr rename wins over the terminal title.
        assert_eq!(tab.panes[2].label, "renamed");

        // A tab belongs to exactly one workspace: w2's tab is not under w1.
        let frontend = &session.workspaces[1];
        assert_eq!(frontend.id, "w2");
        assert_eq!(frontend.tabs.len(), 1);
        assert_eq!(frontend.tabs[0].panes.len(), 1);
        assert_eq!(frontend.tabs[0].panes[0].id, "w2:p1");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test groups_panes_under_their_tab_and_workspace`
Expected: FAIL — `Snapshot` has no `workspaces` field and `Session` has no `workspaces`.

- [ ] **Step 3: Implement** — add the workspace to what is read, add it to what is exposed, and nest the existing grouping inside it:

```rust
#[derive(Deserialize)]
struct Snapshot {
    workspaces: Vec<WorkspaceInfo>,
    tabs: Vec<TabInfo>,
    panes: Vec<PaneInfo>,
}

#[derive(Deserialize)]
struct WorkspaceInfo {
    workspace_id: String,
    label: String,
}

#[derive(Deserialize)]
struct TabInfo {
    tab_id: String,
    workspace_id: String,
    label: String,
}
```

```rust
#[derive(Serialize, Debug, PartialEq)]
pub struct Session {
    pub workspaces: Vec<Workspace>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub label: String,
    pub tabs: Vec<Tab>,
}
```

```rust
fn to_session(snapshot: Snapshot) -> Session {
    let workspaces = snapshot
        .workspaces
        .iter()
        .map(|workspace| Workspace {
            id: workspace.workspace_id.clone(),
            label: workspace.label.clone(),
            tabs: snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == workspace.workspace_id)
                .map(|tab| Tab {
                    id: tab.tab_id.clone(),
                    label: tab.label.clone(),
                    panes: snapshot
                        .panes
                        .iter()
                        .filter(|pane| pane.tab_id == tab.tab_id)
                        .map(|pane| Pane {
                            id: pane.pane_id.clone(),
                            label: pane.label(),
                            agent: pane.agent.clone(),
                            state: pane.agent_status.clone(),
                        })
                        .collect(),
                })
                .filter(|tab| !tab.panes.is_empty())
                .collect(),
        })
        // A workspace with nothing to open is a dead row on a phone screen, the
        // same reason an empty tab is dropped one level down.
        .filter(|workspace| !workspace.tabs.is_empty())
        .collect();
    Session { workspaces }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/herdr.rs
git commit -m "feat: group tabs under their workspace"
```

---

### Task 2: Route the workspace segment

**Track:** `ui`

**Files:**
- Modify: `web/src/lib/route.ts`, `web/src/lib/route.test.ts`

**Interfaces:**
- Produces: `Route = { workspaceId?, tabId?, paneId? }`; `parseRoute`/`href` over the four shapes in the pinned contract.

- [ ] **Step 1: Write the failing tests** — replace the existing three tests:

```ts
test("the four real shapes round-trip", () => {
	for (const route of [
		{},
		{ workspaceId: "w1" },
		{ workspaceId: "w1", tabId: "w1:t1" },
		{ workspaceId: "w1", tabId: "w1:t1", paneId: "w1:p2" },
	]) {
		expect(parseRoute(href(route))).toEqual(route);
	}
});

test("ids keep their colons through encoding", () => {
	expect(href({ workspaceId: "w1", tabId: "w1:t1", paneId: "w1:p2" })).toBe(
		"/w/w1/t/w1%3At1/p/w1%3Ap2",
	);
});

test("anything unrecognised falls back to the nearest real level", () => {
	expect(parseRoute("/")).toEqual({});
	expect(parseRoute("/nope")).toEqual({});
	expect(parseRoute("/w")).toEqual({});
	expect(parseRoute("/%E0%A4%A")).toEqual({});
	// A level that does not parse drops the levels below it, never the one above.
	expect(parseRoute("/w/w1/x/t1")).toEqual({ workspaceId: "w1" });
	expect(parseRoute("/w/w1/t")).toEqual({ workspaceId: "w1" });
	expect(parseRoute("/w/w1/t/w1%3At1/x/p2")).toEqual({
		workspaceId: "w1",
		tabId: "w1:t1",
	});
	expect(parseRoute("/w/w1/t/w1%3At1/p")).toEqual({
		workspaceId: "w1",
		tabId: "w1:t1",
	});
	// A tab without its workspace is not addressable at all.
	expect(parseRoute("/t/w1%3At1")).toEqual({});
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && aube exec vitest run src/lib/route.test.ts`
Expected: FAIL — `href` still emits `/t/<tab>` and `parseRoute` does not know `workspaceId`.

- [ ] **Step 3: Implement**

```ts
export type Route = { workspaceId?: string; tabId?: string; paneId?: string };

/** `/`, `/w/<workspace>`, `/w/<workspace>/t/<tab>`,
 *  `/w/<workspace>/t/<tab>/p/<pane>`. Invalid levels fall back to
 *  the nearest real parent. */
export function parseRoute(pathname: string): Route {
	let parts: string[];
	// A hand-typed URL can carry a stray `%`, and a throw here would blank the page.
	try {
		parts = pathname.split("/").filter(Boolean).map(decodeURIComponent);
	} catch {
		return {};
	}
	const [w, workspaceId, t, tabId, p, paneId] = parts;
	if (w !== "w" || !workspaceId) return {};
	if (t !== "t" || !tabId) return { workspaceId };
	if (p !== "p" || !paneId) return { workspaceId, tabId };
	return { workspaceId, tabId, paneId };
}

export function href(route: Route): string {
	if (!route.workspaceId) return "/";
	const workspace = `/w/${encodeURIComponent(route.workspaceId)}`;
	if (!route.tabId) return workspace;
	const tab = `${workspace}/t/${encodeURIComponent(route.tabId)}`;
	return route.paneId ? `${tab}/p/${encodeURIComponent(route.paneId)}` : tab;
}
```

- [ ] **Step 4: Run the tests**

Run: `cd web && aube exec vitest run src/lib/route.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/route.ts web/src/lib/route.test.ts
git commit -m "feat: route the workspace segment"
```

---

### Task 3: Three-step navigation in the client and the page

**Track:** `ui`

**Files:**
- Modify: `web/src/lib/api.ts`, `web/src/lib/api.test.ts`, `web/src/pages/index.astro`

**Interfaces:**
- Consumes: `Route` from Task 2, and the pinned contract from the `api` track.
- Produces: `Workspace` type, `Session = { workspaces }`, and a page that lists workspaces at the root.

- [ ] **Step 1: Write the failing test** — extend `web/src/lib/api.test.ts` so the session guard is pinned to the new shape. Match the file's existing style; the assertions it needs are that a body with `workspaces` is accepted and a body carrying only the old `tabs` key is rejected as unexpected.

- [ ] **Step 2: Run to verify it fails**

Run: `cd web && aube exec vitest run src/lib/api.test.ts`
Expected: FAIL — `isSession` still accepts `{ tabs }`.

- [ ] **Step 3: Implement the client shape** (`web/src/lib/api.ts`)

```ts
export type Tab = { id: string; label: string; panes: Pane[] };

export type Workspace = { id: string; label: string; tabs: Tab[] };

export type Session = { workspaces: Workspace[] };
```

and in `isSession`, check `Array.isArray((value as Session).workspaces)` instead of `.tabs`.

- [ ] **Step 4: Implement the page** (`web/src/pages/index.astro`)

Everything on screen already derives from the URL; this adds one level to that derivation and one row renderer.

`current()` resolves the extra level:

```ts
      function current(): {
        route: Route;
        workspace?: Workspace;
        tab?: Tab;
        pane?: Pane;
      } {
        const route = parseRoute(location.pathname);
        const workspace = session?.workspaces.find(
          (candidate) => candidate.id === route.workspaceId,
        );
        const tab = workspace?.tabs.find((candidate) => candidate.id === route.tabId);
        const pane = tab?.panes.find((candidate) => candidate.id === route.paneId);
        return { route, workspace, tab, pane };
      }
```

A workspace row mirrors `tabRow`, counting tabs:

```ts
      function workspaceRow(workspace: Workspace) {
        const count =
          workspace.tabs.length === 1 ? "1 tab" : `${workspace.tabs.length} tabs`;
        return row(href({ workspaceId: workspace.id }), workspace.label, count);
      }
```

`tabRow` needs the workspace id to build its href, so it takes the workspace:

```ts
      function tabRow(workspace: Workspace, tab: Tab) {
        const count = tab.panes.length === 1 ? "1 pane" : `${tab.panes.length} panes`;
        return row(
          href({ workspaceId: workspace.id, tabId: tab.id }),
          tab.label,
          count,
        );
      }
```

In `render()`:

- A workspace that closes between two renders drops to the root the way a closed tab already does: when `route.workspaceId` is set and no workspace matches, `go("/", true)` and say "That workspace is gone."
- The closed-tab fallback goes to the workspace, not to the root: `go(href({ workspaceId: workspace!.id }), true)`.
- The closed-pane fallback goes to the tab: `go(href({ workspaceId: workspace!.id, tabId: tab!.id }), true)`.
- The heading is the deepest thing selected: pane, else tab, else workspace, else `"Herdr Remote"`.
- Back is hidden only at the root, and points one level up: from a pane to its tab, from a tab to its workspace, from a workspace to `/`.
- The list renders panes when a tab is open, that workspace's tabs when only a workspace is open, and every workspace otherwise.
- The pane hrefs carry all three ids.

Everything else — the poller, the composer, the key buttons, `syncSend` — reads panes through `current()` and needs no change.

- [ ] **Step 5: Run the tests and the build**

Run: `cd web && aube run test && aube run check && aube run build`
Expected: PASS, and `web/dist` builds.

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/api.ts web/src/lib/api.test.ts web/src/pages/index.astro
git commit -m "feat: pick a workspace before a tab"
```
