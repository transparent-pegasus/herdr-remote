export type Route = { workspaceId?: string; tabId?: string; paneId?: string };

/** `/`, `/w/<workspace>`, `/w/<workspace>/t/<tab>`,
 *  `/w/<workspace>/t/<tab>/p/<pane>`. Anything else reads as the root. */
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
